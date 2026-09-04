//! Minimal Gmail v1 HTTP client. Holds credentials, transparently refreshes
//! the access token (proactively when expired, reactively on 401), and
//! deserializes responses into typed structs.
//!
//! v0 surface: `get_profile`, `list_labels`, `list_threads_page`,
//! `get_thread_metadata`. Mutating endpoints (modify, send, drafts) come
//! with the outbox-drain follow-up.

use std::{future::Future, sync::Arc};

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::config::OAuthConfig;
use crate::credentials::{self, StoredCredentials};
use crate::oauth;

const BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me";
const PUBSUB_BASE: &str = "https://pubsub.googleapis.com/v1";

pub struct GmailClient {
    config: OAuthConfig,
    creds: Mutex<StoredCredentials>,
    http: Client,
}

impl GmailClient {
    /// Construct from credentials already on disk. Errors if `mach auth login`
    /// hasn't been run yet.
    pub fn from_stored_credentials(config: OAuthConfig, account: &str) -> Result<Arc<Self>> {
        let creds = credentials::load(account)
            .context("loading stored credentials")?
            .ok_or_else(|| anyhow!("no credentials for {account} — run `mach auth login` first"))?;
        Self::from_credentials(config, creds)
    }

    pub(crate) fn from_credentials(
        config: OAuthConfig,
        creds: StoredCredentials,
    ) -> Result<Arc<Self>> {
        let http = Client::builder()
            .gzip(true)
            .user_agent(concat!("mach/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Arc::new(Self {
            config,
            creds: Mutex::new(creds),
            http,
        }))
    }

    pub async fn email(&self) -> String {
        self.creds.lock().await.email.clone()
    }

    pub async fn needs_reauth(&self) -> bool {
        self.creds.lock().await.needs_reauth()
    }

    async fn access_token(&self) -> Result<String> {
        let creds = self.creds.lock().await;
        if let Some(reason) = &creds.needs_reauth {
            bail!("account {} needs re-authentication: {reason}", creds.email);
        }
        let need_refresh = creds.is_access_expired();
        drop(creds);
        if need_refresh {
            self.refresh().await?;
        }
        Ok(self.creds.lock().await.access_token.clone())
    }

    async fn refresh(&self) -> Result<()> {
        let refresh_token = self.creds.lock().await.refresh_token.clone();
        debug!("refreshing access token");
        let (access_token, expires_at) =
            match oauth::refresh_access_token(&self.config, &refresh_token).await {
                Ok(token) => token,
                Err(oauth::RefreshError::NeedsReauth(reason)) => {
                    let mut creds = self.creds.lock().await;
                    creds.needs_reauth = Some(reason.clone());
                    credentials::save(&creds).context("persisting re-authentication state")?;
                    bail!("account {} needs re-authentication: {reason}", creds.email);
                }
                Err(oauth::RefreshError::Other(error)) => return Err(error),
            };
        let mut c = self.creds.lock().await;
        c.access_token = access_token;
        c.expires_at = expires_at;
        c.needs_reauth = None;
        credentials::save(&c).context("persisting refreshed credentials")?;
        Ok(())
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        // One retry on 401 — covers the case where the cached access_token
        // got invalidated server-side (revoked, password change, etc.) faster
        // than the local expires_at suggests.
        for attempt in 0..2u8 {
            let token = self.access_token().await?;
            let resp = self.http.get(url).bearer_auth(&token).send().await?;
            match resp.status() {
                StatusCode::OK => return resp.json().await.context("parsing JSON response"),
                StatusCode::UNAUTHORIZED if attempt == 0 => {
                    warn!("got 401, forcing token refresh and retrying");
                    self.refresh().await?;
                    continue;
                }
                s => {
                    let body = resp.text().await.unwrap_or_default();
                    bail!("HTTP {s} from {url}: {body}");
                }
            }
        }
        unreachable!()
    }

    async fn get_json_optional<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
    ) -> Result<Option<T>> {
        for attempt in 0..2u8 {
            let token = self.access_token().await?;
            let resp = self.http.get(url).bearer_auth(&token).send().await?;
            match resp.status() {
                StatusCode::OK => {
                    return resp.json().await.context("parsing JSON response").map(Some)
                }
                StatusCode::NOT_FOUND => return Ok(None),
                StatusCode::UNAUTHORIZED if attempt == 0 => {
                    warn!("got 401, forcing token refresh and retrying");
                    self.refresh().await?;
                }
                status => {
                    let body = resp.text().await.unwrap_or_default();
                    bail!("HTTP {status} from {url}: {body}");
                }
            }
        }
        unreachable!()
    }

    pub async fn get_profile(&self) -> Result<Profile> {
        self.get_json(&format!("{BASE}/profile")).await
    }

    pub async fn watch(&self, topic_name: &str) -> Result<i64> {
        let response: WatchResponse = self
            .post_json_with_response(
                &format!("{BASE}/watch"),
                &serde_json::json!({
                    "topicName": topic_name,
                    "labelIds": ["INBOX"],
                }),
            )
            .await?;
        response
            .expiration
            .parse()
            .context("parsing Gmail watch expiration")
    }

    pub async fn stop(&self) -> Result<()> {
        self.post_json(&format!("{BASE}/stop"), &serde_json::json!({}))
            .await
    }

    pub async fn list_labels(&self) -> Result<Vec<RemoteLabel>> {
        let resp: LabelsResponse = self.get_json(&format!("{BASE}/labels")).await?;
        Ok(resp.labels.unwrap_or_default())
    }

    pub async fn list_threads_page(
        &self,
        query: &str,
        page_token: Option<&str>,
    ) -> Result<ThreadListResponse> {
        let q: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
        let mut url = format!("{BASE}/threads?q={q}&maxResults=100");
        if let Some(p) = page_token {
            let pe: String = url::form_urlencoded::byte_serialize(p.as_bytes()).collect();
            url.push_str("&pageToken=");
            url.push_str(&pe);
        }
        self.get_json(&url).await
    }

    /// GET a specific attachment's bytes. Returns the decoded body. Used
    /// by the desktop's `mach://attachment/...` protocol handler to satisfy
    /// `<img src="cid:...">` references at render time.
    pub async fn get_attachment_bytes(
        &self,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<Vec<u8>> {
        use base64::{
            alphabet,
            engine::{general_purpose::GeneralPurpose, DecodePaddingMode, GeneralPurposeConfig},
            Engine as _,
        };
        const B64URL: GeneralPurpose = GeneralPurpose::new(
            &alphabet::URL_SAFE,
            GeneralPurposeConfig::new()
                .with_decode_padding_mode(DecodePaddingMode::Indifferent)
                .with_decode_allow_trailing_bits(true),
        );

        let url = format!("{BASE}/messages/{message_id}/attachments/{attachment_id}");
        let resp: AttachmentResponse = self.get_json(&url).await?;
        let bytes = B64URL
            .decode(resp.data.trim())
            .context("decoding attachment base64url")?;
        Ok(bytes)
    }

    /// Fetch a thread with full MIME payloads — bodies, attachments, the works.
    /// Used by `BodyFetcher` on first thread open.
    pub async fn get_thread_full(&self, id: &str) -> Result<RemoteThread> {
        let url = format!("{BASE}/threads/{id}?format=full");
        self.get_json(&url).await
    }

    /// POST to `threads/{id}/modify` — add/remove label IDs on a thread.
    /// Used to round-trip archive/star/mark-read/etc. The response is the
    /// updated thread metadata; we ignore the body (sync engine reconciles
    /// via history events).
    pub async fn modify_thread(
        &self,
        thread_id: &str,
        add_label_ids: &[String],
        remove_label_ids: &[String],
    ) -> Result<()> {
        let url = format!("{BASE}/threads/{thread_id}/modify");
        let body = serde_json::json!({
            "addLabelIds": add_label_ids,
            "removeLabelIds": remove_label_ids,
        });
        self.post_json(&url, &body).await
    }

    /// POST to `threads/{id}/trash` — move thread to Trash. Equivalent to
    /// `modify_thread([], [INBOX, ...])` + add `TRASH`, but Gmail provides
    /// a dedicated endpoint that handles the label gymnastics.
    pub async fn trash_thread(&self, thread_id: &str) -> Result<()> {
        let url = format!("{BASE}/threads/{thread_id}/trash");
        self.post_json(&url, &serde_json::json!({})).await
    }

    /// POST a Gmail RFC 2822 message via `messages/send`. The `raw` arg is
    /// the base64url-encoded MIME message (no padding).
    pub async fn send_raw(&self, raw_b64url: &str, thread_id: Option<&str>) -> Result<String> {
        let url = format!("{BASE}/messages/send");
        let mut body = serde_json::json!({ "raw": raw_b64url });
        if let Some(tid) = thread_id {
            body["threadId"] = serde_json::Value::String(tid.to_string());
        }
        let resp: serde_json::Value = self.post_json_with_response(&url, &body).await?;
        Ok(resp["id"].as_str().unwrap_or("").to_string())
    }

    async fn post_json(&self, url: &str, body: &serde_json::Value) -> Result<()> {
        self.post_json_with_response::<serde_json::Value>(url, body)
            .await
            .map(|_| ())
    }

    async fn post_json_with_response<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<T> {
        for attempt in 0..2u8 {
            let token = self.access_token().await?;
            let resp = self
                .http
                .post(url)
                .bearer_auth(&token)
                .json(body)
                .send()
                .await?;
            match resp.status() {
                StatusCode::OK | StatusCode::NO_CONTENT => {
                    if resp.content_length().unwrap_or(0) == 0 {
                        // Empty body — synthesize null which deserializes
                        // happily into serde_json::Value::Null.
                        let null = serde_json::Value::Null;
                        return serde_json::from_value(null).context("parsing empty POST response");
                    }
                    return resp.json().await.context("parsing POST response");
                }
                StatusCode::UNAUTHORIZED if attempt == 0 => {
                    warn!("got 401 on POST; forcing refresh + retry");
                    self.refresh().await?;
                    continue;
                }
                s => {
                    let body = resp.text().await.unwrap_or_default();
                    bail!("HTTP {s} from POST {url}: {body}");
                }
            }
        }
        unreachable!()
    }

    /// GET `users/me/history?startHistoryId=X&historyTypes=...`. Returns
    /// the raw history response — caller handles 404 (gap) and pagination.
    pub async fn list_history(
        &self,
        start_history_id: u64,
        page_token: Option<&str>,
    ) -> Result<HistoryListResponse> {
        let mut url = format!(
            "{BASE}/history?startHistoryId={start_history_id}\
             &historyTypes=messageAdded&historyTypes=messageDeleted\
             &historyTypes=labelAdded&historyTypes=labelRemoved\
             &maxResults=500"
        );
        if let Some(t) = page_token {
            let enc: String = url::form_urlencoded::byte_serialize(t.as_bytes()).collect();
            url.push_str("&pageToken=");
            url.push_str(&enc);
        }
        self.get_json_with_404(&url).await
    }

    async fn get_json_with_404<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        for attempt in 0..2u8 {
            let token = self.access_token().await?;
            let resp = self.http.get(url).bearer_auth(&token).send().await?;
            match resp.status() {
                StatusCode::OK => return resp.json().await.context("parsing JSON response"),
                StatusCode::UNAUTHORIZED if attempt == 0 => {
                    warn!("got 401, forcing token refresh and retrying");
                    self.refresh().await?;
                    continue;
                }
                StatusCode::NOT_FOUND => {
                    bail!("HTTP 404 (history gap — cursor too old)");
                }
                s => {
                    let body = resp.text().await.unwrap_or_default();
                    bail!("HTTP {s} from {url}: {body}");
                }
            }
        }
        unreachable!()
    }

    pub async fn get_thread_metadata(&self, id: &str) -> Result<RemoteThread> {
        let url = format!(
            "{BASE}/threads/{id}?format=metadata\
             &metadataHeaders=Subject\
             &metadataHeaders=From\
             &metadataHeaders=To\
             &metadataHeaders=Cc\
             &metadataHeaders=Date\
             &metadataHeaders=Message-Id\
             &metadataHeaders=In-Reply-To\
             &metadataHeaders=References\
             &metadataHeaders=Reply-To\
             &metadataHeaders=List-Unsubscribe\
             &metadataHeaders=List-Unsubscribe-Post"
        );
        self.get_json(&url).await
    }

    /// Fetch metadata while treating a 404 as a confirmed remote deletion.
    /// Incremental sync uses this to distinguish missing threads from
    /// transient failures that must prevent checkpoint advancement.
    pub async fn get_thread_metadata_optional(&self, id: &str) -> Result<Option<RemoteThread>> {
        let url = format!(
            "{BASE}/threads/{id}?format=metadata\
             &metadataHeaders=Subject\
             &metadataHeaders=From\
             &metadataHeaders=To\
             &metadataHeaders=Cc\
             &metadataHeaders=Date\
             &metadataHeaders=Message-Id\
             &metadataHeaders=In-Reply-To\
             &metadataHeaders=References\
             &metadataHeaders=Reply-To\
             &metadataHeaders=List-Unsubscribe\
             &metadataHeaders=List-Unsubscribe-Post"
        );
        self.get_json_optional(&url).await
    }
}

pub async fn pubsub_pull_loop<F, Fut>(
    client: Arc<GmailClient>,
    subscription: &str,
    on_message: F,
) -> Result<()>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = ()>,
{
    let pull_url = format!("{PUBSUB_BASE}/{subscription}:pull");
    let ack_url = format!("{PUBSUB_BASE}/{subscription}:acknowledge");
    loop {
        let response: PullResponse = client
            .post_json_with_response(
                &pull_url,
                &serde_json::json!({ "returnImmediately": false, "maxMessages": 10 }),
            )
            .await?;
        for received in response.received_messages {
            match decode_pubsub_message(&received.message.data) {
                Ok(notification) => on_message(notification.email_address).await,
                Err(error) => warn!(%error, "discarding malformed Pub/Sub notification"),
            }
            client
                .post_json(
                    &ack_url,
                    &serde_json::json!({ "ackIds": [received.ack_id] }),
                )
                .await?;
        }
    }
}

#[derive(Debug, Deserialize)]
struct WatchResponse {
    expiration: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullResponse {
    #[serde(default)]
    received_messages: Vec<ReceivedMessage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReceivedMessage {
    ack_id: String,
    message: PubSubMessage,
}

#[derive(Debug, Deserialize)]
struct PubSubMessage {
    data: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PubSubNotification {
    pub email_address: String,
    pub history_id: String,
}

pub fn decode_pubsub_message(data: &str) -> Result<PubSubNotification> {
    let json = STANDARD
        .decode(data)
        .context("decoding Pub/Sub message data")?;
    serde_json::from_slice(&json).context("parsing Pub/Sub message data")
}

#[derive(Debug, Deserialize)]
pub struct Profile {
    #[serde(rename = "emailAddress")]
    pub email: String,
    /// Gmail returns historyId as a string. Parse to u64 in the caller.
    #[serde(rename = "historyId")]
    pub history_id: String,
}

#[derive(Debug, Deserialize)]
pub struct LabelsResponse {
    pub labels: Option<Vec<RemoteLabel>>,
}

#[derive(Debug, Deserialize)]
pub struct RemoteLabel {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub label_type: String,
    pub color: Option<LabelColor>,
}

#[derive(Debug, Deserialize)]
pub struct LabelColor {
    #[serde(rename = "textColor")]
    pub text_color: Option<String>,
    #[serde(rename = "backgroundColor")]
    pub background_color: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ThreadListResponse {
    #[serde(default)]
    pub threads: Option<Vec<ThreadStub>>,
    #[serde(rename = "nextPageToken")]
    pub next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ThreadStub {
    pub id: String,
    pub snippet: Option<String>,
    #[serde(rename = "historyId")]
    pub history_id: String,
}

#[derive(Debug, Deserialize)]
pub struct RemoteThread {
    pub id: String,
    #[serde(rename = "historyId")]
    pub history_id: String,
    #[serde(default)]
    pub messages: Vec<RemoteMessage>,
}

#[derive(Debug, Deserialize)]
pub struct AttachmentResponse {
    /// base64url-encoded body
    pub data: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub size: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct HistoryListResponse {
    #[serde(default)]
    pub history: Vec<HistoryRecord>,
    #[serde(rename = "nextPageToken")]
    pub next_page_token: Option<String>,
    #[serde(rename = "historyId")]
    pub history_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct HistoryRecord {
    #[allow(dead_code)]
    pub id: Option<String>,
    #[serde(default, rename = "messagesAdded")]
    pub messages_added: Vec<HistoryMessageRef>,
    #[serde(default, rename = "messagesDeleted")]
    pub messages_deleted: Vec<HistoryMessageRef>,
    #[serde(default, rename = "labelsAdded")]
    pub labels_added: Vec<HistoryMessageRef>,
    #[serde(default, rename = "labelsRemoved")]
    pub labels_removed: Vec<HistoryMessageRef>,
}

#[derive(Debug, Deserialize)]
pub struct HistoryMessageRef {
    pub message: HistoryMessageStub,
    #[serde(default, rename = "labelIds")]
    pub label_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct HistoryMessageStub {
    #[allow(dead_code)]
    pub id: String,
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

#[derive(Debug, Deserialize)]
pub struct RemoteMessage {
    pub id: String,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "labelIds", default)]
    pub label_ids: Vec<String>,
    pub snippet: Option<String>,
    #[serde(rename = "internalDate")]
    pub internal_date: String,
    pub payload: Option<MessagePayload>,
}

/// One part of a Gmail message payload. Gmail returns a recursive tree:
/// `multipart/*` payloads have `parts`; leaf parts (text/plain, text/html,
/// inline images, attachments) have `body.data`. We walk the tree on
/// extraction. `body.data` is **base64url**, no padding.
#[derive(Debug, Deserialize)]
pub struct MessagePayload {
    #[serde(default)]
    pub headers: Vec<MessageHeader>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(rename = "mimeType")]
    pub mime_type: Option<String>,
    pub body: Option<MessageBody>,
    #[serde(default)]
    pub parts: Option<Vec<MessagePayload>>,
}

#[derive(Debug, Deserialize)]
pub struct MessageBody {
    /// Base64url-encoded content. Empty/missing for multipart and attachment parts.
    pub data: Option<String>,
    pub size: Option<i64>,
    #[serde(rename = "attachmentId")]
    pub attachment_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MessageHeader {
    pub name: String,
    pub value: String,
}

impl RemoteMessage {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.payload
            .as_ref()
            .and_then(|p| p.headers.iter().find(|h| h.name.eq_ignore_ascii_case(name)))
            .map(|h| h.value.as_str())
    }

    pub fn internal_date_ms(&self) -> i64 {
        self.internal_date.parse().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_pubsub_message_payload() {
        let data = STANDARD.encode(br#"{"emailAddress":"me@example.com","historyId":"987"}"#);
        assert_eq!(
            decode_pubsub_message(&data).unwrap(),
            PubSubNotification {
                email_address: "me@example.com".into(),
                history_id: "987".into(),
            }
        );
    }
}
