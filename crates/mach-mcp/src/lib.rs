//! MCP server (Model Context Protocol).
//!
//! Speaks JSON-RPC 2.0 over stdio (newline-delimited messages, per the MCP
//! stdio transport spec). The wire is dead simple — Claude Desktop or any
//! MCP client launches `mach mcp` as a child process and pipes JSON-RPC.
//!
//! The server keeps the raw **mach** Action tool and adds task-level inbox,
//! reading, search, reply, and digest tools for agent callers.
//!
//! ECHO suppression and incremental sync are deliberately out of scope
//! for the MCP server. Mutations land via the same outbox the TUI uses;
//! the next `mach sync` (TUI tick or manual) drains them.

use std::{collections::BTreeMap, sync::Arc, time::SystemTime};

use anyhow::{Context, Result};
use mach_core::{
    action::DISPATCHER_ACTION_NAMES,
    ids::{AccountId, AccountScope, LabelId, MessageId, ThreadId},
    store::{MailStore, ThreadSummary},
    Action, Dispatcher, DraftPatch, UserConfig,
};
use mach_gmail::{GmailAccountPool, OutboxWorker};
use mach_store::SqliteStore;
use schemars::schema_for;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, error, warn};

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "mach";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct Server {
    store: Arc<SqliteStore>,
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
            dispatcher: Dispatcher::with_scope(store.clone(), scope),
            store,
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
                },
                {
                    "name": "inbox_overview",
                    "description": "List the newest inbox threads and total unread count from the local cache, optionally restricted to one account; use this to scan mail before opening a thread.",
                    "inputSchema": object_schema(
                        json!({
                            "account": { "type": "string", "description": "Account email address." },
                            "limit": { "type": "integer", "minimum": 0, "maximum": 4294967295u64, "default": 50 }
                        }),
                        &[],
                    ),
                },
                {
                    "name": "read_thread",
                    "description": "Open one cached thread and return its messages with plain-text bodies, fetching missing bodies from Gmail when that account is online and truncating each body for a bounded model context.",
                    "inputSchema": object_schema(
                        json!({
                            "id": { "type": "string", "description": "Thread ID." },
                            "max_chars": { "type": "integer", "minimum": 0, "maximum": 4294967295u64, "default": 8000 }
                        }),
                        &["id"],
                    ),
                },
                {
                    "name": "find_threads",
                    "description": "Search cached mail with mach's Gmail-style operators and, when needed and online, extend the search through Gmail before returning account-qualified thread summaries.",
                    "inputSchema": object_schema(
                        json!({
                            "query": { "type": "string", "description": "Text and operators such as from:, label:, is:unread, newer_than:, and has:attachment." },
                            "limit": { "type": "integer", "minimum": 0, "maximum": 4294967295u64, "default": 50 }
                        }),
                        &["query"],
                    ),
                },
                {
                    "name": "draft_reply",
                    "description": "Create a reply or reply-all draft from a message, place the supplied Markdown above the generated quote, and optionally queue and immediately attempt delivery through that account's Gmail outbox.",
                    "inputSchema": object_schema(
                        json!({
                            "message_id": { "type": "string", "description": "Message ID to reply to." },
                            "body": { "type": "string", "description": "New reply text in Markdown." },
                            "all": { "type": "boolean", "default": false },
                            "send": { "type": "boolean", "default": false }
                        }),
                        &["message_id", "body"],
                    ),
                },
                {
                    "name": "daily_digest",
                    "description": "Summarize recent inbox activity by Gmail category, show the ten newest unread threads, and identify unread or starred conversations whose latest message came from someone else.",
                    "inputSchema": object_schema(
                        json!({
                            "since_hours": { "type": "integer", "minimum": 0, "maximum": 4294967295u64, "default": 24 }
                        }),
                        &[],
                    ),
                }
            ]
        })
    }

    async fn tools_call(&self, params: Value) -> Result<Value> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .context("missing tool name")?;
        let args = params
            .get("arguments")
            .cloned()
            .context("missing tool arguments")?;
        let value = match name {
            "mach" => return self.call_mach(args).await,
            "inbox_overview" => self.inbox_overview(&args).await?,
            "read_thread" => self.read_thread(&args).await?,
            "find_threads" => self.find_threads(&args).await?,
            "draft_reply" => self.draft_reply(&args).await?,
            "daily_digest" => self.daily_digest(&args).await?,
            _ => anyhow::bail!("unknown tool: {name}"),
        };
        tool_content(&value)
    }

    async fn call_mach(&self, args: Value) -> Result<Value> {
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
        tool_content(&outcome)
    }

    async fn inbox_overview(&self, args: &Value) -> Result<Value> {
        let scope = self.requested_scope(optional_str(args, "account")?)?;
        let limit = optional_u32(args, "limit", 50)?;
        let threads = self
            .store
            .list_threads_in_label(&scope, &LabelId::new("INBOX"), u32::MAX)
            .await?;
        Ok(overview(threads, limit))
    }

    async fn read_thread(&self, args: &Value) -> Result<Value> {
        let id = ThreadId::new(required_str(args, "id")?);
        let max_chars = optional_u32(args, "max_chars", 8_000)? as usize;
        let summary = self
            .store
            .get_thread(self.dispatcher_scope(), &id)
            .await?
            .with_context(|| format!("thread {id} not found"))?;
        if let Some(fetcher) = self.body_fetchers.get(&summary.account_id) {
            let _ = fetcher.fetch_if_needed(&id).await;
        }
        let mut outcome = self.dispatcher.execute(Action::OpenThread { id }).await?;
        if let Some(messages) = outcome
            .data
            .as_mut()
            .and_then(|data| data.get_mut("messages"))
            .and_then(Value::as_array_mut)
        {
            for message in messages {
                let plain = message
                    .get("body_plain")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| {
                        message
                            .get("body_html")
                            .and_then(Value::as_str)
                            .and_then(|html| html2text::from_read(html.as_bytes(), 1_000_000).ok())
                    });
                message["body_plain"] = plain
                    .map(|body| Value::String(truncate_body(&body, max_chars)))
                    .unwrap_or(Value::Null);
                message["body_html"] = Value::Null;
            }
        }
        serde_json::to_value(outcome).context("serializing opened thread")
    }

    async fn find_threads(&self, args: &Value) -> Result<Value> {
        let query = required_str(args, "query")?;
        let limit = optional_u32(args, "limit", 50)?;
        let mut threads = self
            .store
            .search_threads(self.dispatcher_scope(), query, limit)
            .await?;
        if threads.len() < limit as usize && !self.body_fetchers.is_empty() {
            let report = self
                .body_fetchers
                .search_remote(self.dispatcher_scope(), query, limit)
                .await;
            threads.extend(report.results);
            threads.sort_by(|left, right| right.last_message_at.cmp(&left.last_message_at));
            threads
                .dedup_by(|left, right| left.account_id == right.account_id && left.id == right.id);
        }
        Ok(overview(threads, limit))
    }

    async fn draft_reply(&self, args: &Value) -> Result<Value> {
        let message_id = MessageId::new(required_str(args, "message_id")?);
        let body = required_str(args, "body")?;
        let all = optional_bool(args, "all", false)?;
        let send = optional_bool(args, "send", false)?;
        let reply = self
            .dispatcher
            .execute(Action::Reply { message_id, all })
            .await?;
        let draft = reply
            .data
            .as_ref()
            .and_then(|data| data.get("draft"))
            .context("reply did not create a draft")?;
        let draft_id = draft
            .get("id")
            .and_then(Value::as_str)
            .context("reply draft has no id")?;
        let account = draft
            .get("account_id")
            .and_then(Value::as_str)
            .context("reply draft has no account")?;
        let prefill = draft
            .get("body_md")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let draft_id = mach_core::DraftId::new(draft_id);
        self.dispatcher
            .execute(Action::SaveDraft {
                draft_id: draft_id.clone(),
                patch: DraftPatch {
                    body_md: Some(format!("{body}{prefill}")),
                    ..DraftPatch::default()
                },
            })
            .await?;
        if send {
            self.dispatcher
                .execute(Action::SendDraft {
                    draft_id: draft_id.clone(),
                })
                .await?;
            let account = AccountId::new(account);
            let fetcher = self
                .body_fetchers
                .get(&account)
                .with_context(|| format!("no Gmail client for {account}"))?;
            let stats = OutboxWorker::new(account, fetcher.client().clone(), self.store.clone())
                .drain_once(200)
                .await?;
            if stats.failed != 0 {
                anyhow::bail!("{} outbox operation(s) failed", stats.failed);
            }
        }
        Ok(json!({ "draft_id": draft_id, "sent": send }))
    }

    async fn daily_digest(&self, args: &Value) -> Result<Value> {
        let since_hours = optional_u32(args, "since_hours", 24)?;
        let cutoff = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
            - i64::from(since_hours) * 3_600_000;
        let threads = self
            .store
            .list_threads_in_label(self.dispatcher_scope(), &LabelId::new("INBOX"), u32::MAX)
            .await?
            .into_iter()
            .filter(|thread| thread.last_message_at.timestamp_millis() >= cutoff)
            .collect::<Vec<_>>();
        let mut category_counts = BTreeMap::<String, usize>::new();
        for thread in &threads {
            let category = thread
                .label_ids
                .iter()
                .find_map(|label| label.as_str().strip_prefix("CATEGORY_"))
                .unwrap_or("PRIMARY")
                .to_ascii_lowercase();
            *category_counts.entry(category).or_default() += 1;
        }
        let top_unread = threads
            .iter()
            .filter(|thread| thread.unread)
            .take(10)
            .cloned()
            .collect::<Vec<_>>();
        let mut awaiting_reply = Vec::new();
        for thread in &threads {
            let messages = self
                .store
                .list_messages_in_thread(self.dispatcher_scope(), &thread.id)
                .await?;
            if mach_core::is_awaiting_reply(thread, messages.last()) {
                awaiting_reply.push(thread.clone());
            }
        }
        Ok(json!({
            "category_counts": category_counts,
            "top_unread": overview_threads(&top_unread),
            "awaiting_reply": overview_threads(&awaiting_reply),
        }))
    }

    fn requested_scope(&self, account: Option<&str>) -> Result<AccountScope> {
        match (self.dispatcher_scope(), account) {
            (AccountScope::One(current), Some(requested)) if current.as_str() != requested => {
                anyhow::bail!("account {requested} is outside this server's scope")
            }
            (AccountScope::One(current), _) => Ok(AccountScope::One(current.clone())),
            (AccountScope::All, Some(account)) => Ok(AccountScope::One(AccountId::new(account))),
            (AccountScope::All, None) => Ok(AccountScope::All),
        }
    }

    fn dispatcher_scope(&self) -> &AccountScope {
        self.dispatcher.scope()
    }
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn required_str<'a>(args: &'a Value, name: &str) -> Result<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .with_context(|| format!("missing or invalid {name}"))
}

fn optional_str<'a>(args: &'a Value, name: &str) -> Result<Option<&'a str>> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .with_context(|| format!("invalid {name}")),
    }
}

fn optional_u32(args: &Value, name: &str, default: u32) -> Result<u32> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => value
            .as_u64()
            .and_then(|number| u32::try_from(number).ok())
            .with_context(|| format!("invalid {name}")),
    }
}

fn optional_bool(args: &Value, name: &str, default: bool) -> Result<bool> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => value.as_bool().with_context(|| format!("invalid {name}")),
    }
}

fn truncate_body(body: &str, max_chars: usize) -> String {
    if body.chars().count() <= max_chars {
        return body.to_string();
    }
    let mut truncated = body.chars().take(max_chars).collect::<String>();
    truncated.push_str("\n[truncated]");
    truncated
}

fn overview(mut threads: Vec<ThreadSummary>, limit: u32) -> Value {
    let unread_count = threads.iter().filter(|thread| thread.unread).count();
    threads.sort_by(|left, right| right.last_message_at.cmp(&left.last_message_at));
    threads.truncate(limit as usize);
    json!({
        "unread_count": unread_count,
        "threads": overview_threads(&threads),
    })
}

fn overview_threads(threads: &[ThreadSummary]) -> Vec<Value> {
    threads
        .iter()
        .map(|thread| {
            json!({
                "id": thread.id,
                "account_id": thread.account_id,
                "subject": thread.subject,
                "from": thread.participants.join(", "),
                "date": thread.last_message_at,
                "snippet": thread.snippet,
                "unread": thread.unread,
                "starred": thread.starred,
                "labels": thread.label_ids,
            })
        })
        .collect()
}

fn tool_content(value: &impl serde::Serialize) -> Result<Value> {
    Ok(json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(value)?,
        }]
    }))
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
    use mach_store::{MessageUpsert, ThreadUpsert};

    fn server() -> Server {
        let store = Arc::new(SqliteStore::new(mach_store::open_in_memory().unwrap()));
        Server::new(
            store,
            Arc::new(GmailAccountPool::default()),
            AccountScope::All,
        )
    }

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

    #[test]
    fn tools_list_exposes_raw_and_high_level_tools() {
        let tools = server().tools_list();
        let names = tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "mach",
                "inbox_overview",
                "read_thread",
                "find_threads",
                "draft_reply",
                "daily_digest",
            ]
        );
    }

    #[tokio::test]
    async fn inbox_overview_round_trips_through_tools_call() {
        let server = server();
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        server
            .store
            .upsert_threads(
                &AccountId::new("me@example.com"),
                vec![ThreadUpsert {
                    id: "thread-1".into(),
                    history_id: 1,
                    subject: "Hello".into(),
                    snippet: "Latest message".into(),
                    participants: vec!["Sender <sender@example.com>".into()],
                    last_message_at_ms: now,
                    label_ids: vec!["INBOX".into(), "UNREAD".into()],
                    messages: vec![MessageUpsert {
                        id: "message-1".into(),
                        thread_id: "thread-1".into(),
                        history_id: 1,
                        internal_date_ms: now,
                        from: "sender@example.com".into(),
                        to: vec!["me@example.com".into()],
                        cc: Vec::new(),
                        subject: "Hello".into(),
                        snippet: "Latest message".into(),
                        label_ids: vec!["INBOX".into(), "UNREAD".into()],
                        body_plain: Some("Latest message".into()),
                        headers_json: None,
                    }],
                }],
            )
            .await
            .unwrap();

        let response = server
            .handle(
                "tools/call",
                json!({ "name": "inbox_overview", "arguments": {} }),
                Some(json!(1)),
            )
            .await
            .unwrap();
        let payload: Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(payload["unread_count"], 1);
        assert_eq!(payload["threads"][0]["id"], "thread-1");
        assert_eq!(payload["threads"][0]["from"], "Sender <sender@example.com>");
    }
}
