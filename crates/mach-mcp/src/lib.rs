//! MCP server (Model Context Protocol).
//!
//! Speaks JSON-RPC 2.0 over stdio (newline-delimited messages, per the MCP
//! stdio transport spec). The wire is dead simple — Claude Desktop or any
//! MCP client launches `mach mcp` as a child process and pipes JSON-RPC.
//!
//! Tool surface:
//! - **mach** — single generic tool whose `inputSchema` is the
//!   `Action` enum's JSON Schema. The model passes any Action JSON; the
//!   server dispatches it. This keeps the surface lined up exactly with
//!   the CLI's `mach do` — same shape, same outcomes.
//!
//! ECHO suppression and incremental sync are deliberately out of scope
//! for the MCP server. Mutations land via the same outbox the TUI uses;
//! the next `mach sync` (TUI tick or manual) drains them.

use std::sync::Arc;

use anyhow::{Context, Result};
use mach_core::ids::AccountScope;
use mach_core::{action::DISPATCHER_ACTION_NAMES, Action, Dispatcher, UserConfig};
use mach_gmail::GmailAccountPool;
use mach_store::SqliteStore;
use schemars::schema_for;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, error, warn};

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "mach";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct Server {
    dispatcher: Dispatcher,
    body_fetchers: Arc<GmailAccountPool>,
}

impl Server {
    pub fn new(
        store: Arc<SqliteStore>,
        body_fetchers: Arc<GmailAccountPool>,
        scope: AccountScope,
    ) -> Self {
        Self {
            dispatcher: Dispatcher::with_scope(store, scope),
            body_fetchers,
        }
    }

    pub fn with_user_config(mut self, user_config: UserConfig) -> Self {
        self.dispatcher = self.dispatcher.with_user_config(user_config);
        self
    }

    /// Run the server to completion. Returns when stdin closes (client
    /// disconnect) or on fatal protocol error.
    pub async fn run(self) -> Result<()> {
        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin);
        let mut stdout = tokio::io::stdout();
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break, // EOF
                Ok(_) => {}
                Err(e) => {
                    error!(error = %e, "stdin read error");
                    break;
                }
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let req: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(e) => {
                    warn!(error = %e, "malformed json from client");
                    let err = rpc_error(Value::Null, -32700, &format!("Parse error: {e}"), None);
                    write_line(&mut stdout, &err).await?;
                    continue;
                }
            };
            // Notifications (no `id` field) — silently process, no reply.
            let id = req.get("id").cloned();
            let method = req
                .get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let params = req.get("params").cloned().unwrap_or(Value::Null);

            debug!(method = %method, "rpc request");

            let response = self.handle(&method, params, id.clone()).await;
            if let Some(resp) = response {
                write_line(&mut stdout, &resp).await?;
            }
        }
        Ok(())
    }

    async fn handle(&self, method: &str, params: Value, id: Option<Value>) -> Option<Value> {
        // Notifications: no `id`, no response.
        let id = id?;
        match method {
            "initialize" => Some(rpc_ok(id, self.initialize_result())),
            "ping" => Some(rpc_ok(id, json!({}))),
            "tools/list" => Some(rpc_ok(id, self.tools_list())),
            "tools/call" => match self.tools_call(params).await {
                Ok(content) => Some(rpc_ok(id, content)),
                Err(e) => Some(rpc_error(id, -32603, &e.to_string(), None)),
            },
            "resources/list" => Some(rpc_ok(id, json!({ "resources": [] }))),
            "prompts/list" => Some(rpc_ok(id, json!({ "prompts": [] }))),
            _ => Some(rpc_error(
                id,
                -32601,
                &format!("Method not found: {method}"),
                None,
            )),
        }
    }

    fn initialize_result(&self) -> Value {
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {
                "tools": {},
            },
            "serverInfo": {
                "name": SERVER_NAME,
                "version": SERVER_VERSION,
            },
        })
    }

    fn tools_list(&self) -> Value {
        let action_schema = schema_for!(Action);
        let mut schema_value = serde_json::to_value(&action_schema).unwrap_or(json!({}));
        retain_dispatcher_actions(&mut schema_value);
        json!({
            "tools": [
                {
                    "name": "mach",
                    "description": "Dispatch a mach Action against the local cache + outbox. \
                        Mutations apply optimistically and round-trip to Gmail on the next \
                        `mach sync`. Search uses FTS5 \
                        over the cached body index.",
                    "inputSchema": schema_value,
                }
            ]
        })
    }

    async fn tools_call(&self, params: Value) -> Result<Value> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .context("missing tool name")?;
        if name != "mach" {
            anyhow::bail!("unknown tool: {name}");
        }
        let args = params
            .get("arguments")
            .cloned()
            .context("missing tool arguments")?;
        let action: Action =
            serde_json::from_value(args).context("arguments did not match an Action variant")?;
        if !action.is_dispatcher_supported() {
            anyhow::bail!(
                "{} is reserved for an interactive surface and is not implemented by MCP",
                action.name()
            );
        }

        // Same backfill semantics as the CLI's `mach do open_thread`.
        if let Action::OpenThread { id } = &action {
            let summary = self
                .dispatcher
                .store()
                .get_thread(self.dispatcher_scope(), id)
                .await?;
            if let Some(fetcher) = summary
                .as_ref()
                .and_then(|thread| self.body_fetchers.get(&thread.account_id))
            {
                let _ = fetcher.fetch_if_needed(id).await;
            }
        }

        let outcome = self.dispatcher.execute(action).await?;
        let pretty = serde_json::to_string_pretty(&outcome)?;
        // MCP returns content as an array of typed parts. We give one text
        // block with the outcome JSON; the model can parse + reason over it.
        Ok(json!({
            "content": [
                { "type": "text", "text": pretty }
            ]
        }))
    }

    fn dispatcher_scope(&self) -> &AccountScope {
        self.dispatcher.scope()
    }
}

fn retain_dispatcher_actions(schema: &mut Value) {
    let Some(branches) = schema.get_mut("oneOf").and_then(Value::as_array_mut) else {
        return;
    };
    branches.retain(|branch| {
        schema_action_name(branch).is_some_and(|name| DISPATCHER_ACTION_NAMES.contains(&name))
    });
}

fn schema_action_name(branch: &Value) -> Option<&str> {
    branch
        .pointer("/properties/kind/enum/0")
        .or_else(|| branch.pointer("/properties/kind/const"))
        .and_then(Value::as_str)
}

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut err = json!({ "code": code, "message": message });
    if let Some(d) = data {
        err["data"] = d;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": err })
}

async fn write_line<W: AsyncWriteExt + Unpin>(w: &mut W, v: &Value) -> Result<()> {
    let mut s = serde_json::to_string(v)?;
    s.push('\n');
    w.write_all(s.as_bytes()).await?;
    w.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_schema_only_exposes_dispatcher_actions() {
        let mut schema = serde_json::to_value(schema_for!(Action)).unwrap();
        retain_dispatcher_actions(&mut schema);
        let branches = schema["oneOf"].as_array().unwrap();
        let names: Vec<_> = branches.iter().filter_map(schema_action_name).collect();

        assert_eq!(names.len(), DISPATCHER_ACTION_NAMES.len());
        assert!(names
            .iter()
            .all(|name| DISPATCHER_ACTION_NAMES.contains(name)));
        assert!(!names.contains(&"send_draft"));
    }
}
