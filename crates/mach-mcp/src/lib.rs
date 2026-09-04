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
    scope: AccountScope,
    user_config: UserConfig,
    known_accounts: Vec<AccountId>,
    needs_reauth: BTreeMap<String, bool>,
}

impl Server {
    pub fn new(
        store: Arc<SqliteStore>,
        body_fetchers: Arc<GmailAccountPool>,
        scope: AccountScope,
    ) -> Self {
        let known_accounts = body_fetchers.accounts().collect();
        Self {
            dispatcher: Dispatcher::with_scope(store.clone(), scope.clone()),
            store,
            body_fetchers,
            scope,
            user_config: UserConfig::default(),
            known_accounts,
            needs_reauth: BTreeMap::new(),
        }
    }

    pub fn with_user_config(mut self, user_config: UserConfig) -> Self {
        self.user_config = user_config;
        self.rebuild_dispatcher();
        self
    }

    pub fn with_accounts(
        mut self,
        accounts: Vec<mach_gmail::credentials::StoredCredentials>,
    ) -> Self {
        self.needs_reauth = accounts
            .iter()
            .map(|credentials| (credentials.email.clone(), credentials.needs_reauth()))
            .collect();
        self.known_accounts = accounts
            .into_iter()
            .map(|credentials| AccountId::new(credentials.email))
            .collect();
        self.rebuild_dispatcher();
        self
    }

    fn rebuild_dispatcher(&mut self) {
        let dispatcher = Dispatcher::with_scope(self.store.clone(), self.scope.clone())
            .with_user_config(self.user_config.clone());
        self.dispatcher = match self.user_config.default_account(&self.known_accounts) {
            Some(account) => dispatcher.with_default_account(account),
            None => dispatcher,
        };
    }

    fn scoped_dispatcher(&self, scope: AccountScope) -> Dispatcher {
        let dispatcher = Dispatcher::with_scope(self.store.clone(), scope)
            .with_user_config(self.user_config.clone());
        match self.user_config.default_account(&self.known_accounts) {
            Some(account) => dispatcher.with_default_account(account),
            None => dispatcher,
        }
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
        add_account_argument(&mut schema_value);
        json!({
            "tools": [
                {
                    "name": "mach",
                    "description": "Dispatch a mach Action against the local cache + outbox. \
                        Mutations apply optimistically and round-trip to Gmail on the next \
                        `mach sync`. Search uses FTS5 over the cached body index. Reads may span \
                        all accounts; mutations and compose must resolve to exactly one account. \
                        Pass account (email or nickname) when the server runs unified.",
                    "inputSchema": schema_value,
                },
                {
                    "name": "list_accounts",
                    "description": "List configured accounts with nickname, unread count, authentication state, last incremental sync, and Gmail watch status.",
                    "inputSchema": object_schema(json!({}), &[]),
                },
                {
                    "name": "inbox_overview",
                    "description": "List the newest inbox threads and total unread count from the local cache, optionally restricted to one account; use this to scan mail before opening a thread.",
                    "inputSchema": object_schema(
                        json!({
                            "account": account_schema(),
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
                            "account": account_schema(),
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
                            "account": account_schema(),
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
                            "account": account_schema(),
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
                            "account": account_schema(),
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
            "list_accounts" => self.list_accounts().await?,
            "inbox_overview" => self.inbox_overview(&args).await?,
            "read_thread" => self.read_thread(&args).await?,
            "find_threads" => self.find_threads(&args).await?,
            "draft_reply" => self.draft_reply(&args).await?,
            "daily_digest" => self.daily_digest(&args).await?,
            _ => anyhow::bail!("unknown tool: {name}"),
        };
        self.tool_content(&value)
    }

    async fn call_mach(&self, mut args: Value) -> Result<Value> {
        let requested = optional_str(&args, "account")?.map(str::to_owned);
        if let Some(object) = args.as_object_mut() {
            object.remove("account");
        }
        let scope = self.requested_scope(requested.as_deref())?;
        let mut action: Action =
            serde_json::from_value(args).context("arguments did not match an Action variant")?;
        if let Action::ComposeNew { account } = &mut action {
            *account = scope.account().cloned();
        }
        if !action.is_dispatcher_supported() {
            anyhow::bail!(
                "{} is reserved for an interactive surface and is not implemented by MCP",
                action.name()
            );
        }

        // Same backfill semantics as the CLI's `mach do open_thread`.
        if let Action::OpenThread { id } = &action {
            let summary = self.dispatcher.store().get_thread(&scope, id).await?;
            if let Some(fetcher) = summary
                .as_ref()
                .and_then(|thread| self.body_fetchers.get(&thread.account_id))
            {
                let _ = fetcher.fetch_if_needed(id).await;
            }
        }

        let outcome = if scope == self.scope {
            self.dispatcher.execute(action).await?
        } else {
            self.scoped_dispatcher(scope).execute(action).await?
        };
        self.tool_content(&outcome)
    }

    async fn list_accounts(&self) -> Result<Value> {
        let mut accounts = Vec::with_capacity(self.known_accounts.len());
        let push = mach_gmail::config::pubsub_topic().is_some();
        for account in &self.known_accounts {
            let overview = self.store.account_overview(account).await?;
            accounts.push(json!({
                "email": account,
                "account_id": overview.account_id,
                "nickname": self.user_config.account_label(account.as_str()),
                "unread": overview.unread,
                "needs_reauth": self.needs_reauth.get(account.as_str()).copied().unwrap_or(false),
                "last_incremental_at": overview.last_incremental_at,
                "watch_status": push.then(|| watch_status(overview.watch_expiration)),
            }));
        }
        Ok(Value::Array(accounts))
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
        let scope = self.requested_scope(optional_str(args, "account")?)?;
        let id = ThreadId::new(required_str(args, "id")?);
        let max_chars = optional_u32(args, "max_chars", 8_000)? as usize;
        let summary = self
            .store
            .get_thread(&scope, &id)
            .await?
            .with_context(|| format!("thread {id} not found"))?;
        if let Some(fetcher) = self.body_fetchers.get(&summary.account_id) {
            let _ = fetcher.fetch_if_needed(&id).await;
        }
        let mut outcome = if scope == self.scope {
            self.dispatcher.execute(Action::OpenThread { id }).await?
        } else {
            self.scoped_dispatcher(scope)
                .execute(Action::OpenThread { id })
                .await?
        };
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
        let scope = self.requested_scope(optional_str(args, "account")?)?;
        let query = required_str(args, "query")?;
        let limit = optional_u32(args, "limit", 50)?;
        let mut threads = self.store.search_threads(&scope, query, limit).await?;
        if threads.len() < limit as usize && !self.body_fetchers.is_empty() {
            let report = self.body_fetchers.search_remote(&scope, query, limit).await;
            threads.extend(report.results);
            threads.sort_by(|left, right| right.last_message_at.cmp(&left.last_message_at));
            threads
                .dedup_by(|left, right| left.account_id == right.account_id && left.id == right.id);
        }
        Ok(overview(threads, limit))
    }

    async fn draft_reply(&self, args: &Value) -> Result<Value> {
        let scope = self.requested_scope(optional_str(args, "account")?)?;
        let message_id = MessageId::new(required_str(args, "message_id")?);
        let body = required_str(args, "body")?;
        let all = optional_bool(args, "all", false)?;
        let send = optional_bool(args, "send", false)?;
        let scoped_dispatcher;
        let dispatcher = if scope == self.scope {
            &self.dispatcher
        } else {
            // ponytail: per-call dispatcher loses in-memory undo history; cache one per account
            // if MCP clients begin relying on account-scoped `undo`.
            scoped_dispatcher = self.scoped_dispatcher(scope);
            &scoped_dispatcher
        };
        let reply = dispatcher
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
        dispatcher
            .execute(Action::SaveDraft {
                draft_id: draft_id.clone(),
                patch: DraftPatch {
                    body_md: Some(format!("{body}{prefill}")),
                    ..DraftPatch::default()
                },
            })
            .await?;
        if send {
            dispatcher
                .execute(Action::SendDraft {
                    draft_id: draft_id.clone(),
                })
                .await?;
            let account = AccountId::new(account);
            let fetcher = self
                .body_fetchers
                .get(&account)
                .with_context(|| format!("no Gmail client for {account}"))?;
            let stats = OutboxWorker::new(
                account.clone(),
                fetcher.client().clone(),
                self.store.clone(),
            )
            .drain_once(200)
            .await?;
            if stats.failed != 0 {
                anyhow::bail!("{} outbox operation(s) failed for {account}", stats.failed);
            }
        }
        Ok(json!({ "draft_id": draft_id, "sent": send }))
    }

    async fn daily_digest(&self, args: &Value) -> Result<Value> {
        let scope = self.requested_scope(optional_str(args, "account")?)?;
        let since_hours = optional_u32(args, "since_hours", 24)?;
        let cutoff = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
            - i64::from(since_hours) * 3_600_000;
        let threads = self
            .store
            .list_threads_in_label(&scope, &LabelId::new("INBOX"), u32::MAX)
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
                .list_messages_in_thread(&scope, &thread.id)
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
        let requested = account
            .map(|account| {
                self.user_config
                    .resolve_account(account, &self.known_accounts)
                    .with_context(|| format!("unknown account {account}"))
            })
            .transpose()?;
        match (&self.scope, requested) {
            (AccountScope::One(current), Some(requested)) if current != &requested => {
                anyhow::bail!("account {requested} is outside this server's scope")
            }
            (AccountScope::One(current), _) => Ok(AccountScope::One(current.clone())),
            (AccountScope::All, Some(account)) => Ok(AccountScope::One(account)),
            (AccountScope::All, None) => Ok(AccountScope::All),
        }
    }

    fn tool_content(&self, value: &impl serde::Serialize) -> Result<Value> {
        let mut value = serde_json::to_value(value)?;
        decorate_account_labels(&mut value, &self.user_config);
        tool_content(&value)
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

fn account_schema() -> Value {
    json!({ "type": "string", "description": "Account email address or nickname." })
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

fn decorate_account_labels(value: &mut Value, config: &UserConfig) {
    match value {
        Value::Object(object) => {
            if let Some(account) = object
                .get("account_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
            {
                object.insert(
                    "account_label".into(),
                    Value::String(config.account_label(&account).to_string()),
                );
            }
            for child in object.values_mut() {
                decorate_account_labels(child, config);
            }
        }
        Value::Array(array) => {
            for child in array {
                decorate_account_labels(child, config);
            }
        }
        _ => {}
    }
}

fn add_account_argument(schema: &mut Value) {
    let Some(branches) = schema.get_mut("oneOf").and_then(Value::as_array_mut) else {
        return;
    };
    for branch in branches {
        if let Some(properties) = branch.get_mut("properties").and_then(Value::as_object_mut) {
            properties.insert("account".into(), account_schema());
        }
    }
}

fn watch_status(expiration: Option<i64>) -> &'static str {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    match expiration {
        None => "not registered",
        Some(value) if mach_gmail::should_renew(Some(value), now) => "renewal due",
        Some(_) => "active",
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
                "list_accounts",
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

    #[tokio::test]
    async fn raw_mach_account_nickname_scopes_one_call() {
        let mut server = server();
        let work = AccountId::new("work@example.com");
        let home = AccountId::new("home@example.com");
        server.known_accounts = vec![work.clone(), home.clone()];
        server
            .user_config
            .accounts
            .insert(work.to_string(), "Work".into());
        server.rebuild_dispatcher();
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        for (account, id) in [(&work, "work-thread"), (&home, "home-thread")] {
            server
                .store
                .upsert_threads(
                    account,
                    vec![ThreadUpsert {
                        id: id.into(),
                        history_id: 1,
                        subject: "Needle".into(),
                        snippet: String::new(),
                        participants: vec![],
                        last_message_at_ms: now,
                        label_ids: vec!["INBOX".into()],
                        messages: vec![MessageUpsert {
                            id: format!("{id}-message"),
                            thread_id: id.into(),
                            history_id: 1,
                            internal_date_ms: now,
                            from: "sender@example.com".into(),
                            to: vec![account.to_string()],
                            cc: vec![],
                            subject: "Needle".into(),
                            snippet: String::new(),
                            label_ids: vec!["INBOX".into()],
                            body_plain: Some("Needle".into()),
                            headers_json: None,
                        }],
                    }],
                )
                .await
                .unwrap();
        }

        let result = server
            .call_mach(json!({
                "kind": "search",
                "query": "Needle",
                "limit": 10,
                "account": "work"
            }))
            .await
            .unwrap();
        let payload: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        let data = payload["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["account_id"], "work@example.com");
        assert_eq!(data[0]["account_label"], "Work");
    }

    #[tokio::test]
    async fn compose_new_in_unified_scope_has_helpful_error() {
        let mut server = server();
        server.known_accounts = vec![
            AccountId::new("work@example.com"),
            AccountId::new("home@example.com"),
        ];
        server.rebuild_dispatcher();

        let error = server
            .call_mach(json!({ "kind": "compose_new" }))
            .await
            .unwrap_err();
        assert!(error.to_string().contains(
            "compose needs an account: pass account, use --account, or set [accounts] default in config.toml"
        ));
    }
}
