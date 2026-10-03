//! Cloudflare's Clef decision model scores each new inbox thread so code can
//! decide which ones deserve a desktop notification. Clef only scores; the
//! threshold lives here. Any failure fails open (thread still notifies).

use mach_core::store::ThreadSummary;
use mach_core::user_config::TriageConfig;
use std::sync::OnceLock;
use tracing::{info, warn};

const NOTIFY_THRESHOLD: f64 = 0.6;

#[derive(Debug, PartialEq)]
pub struct Verdict {
    pub notify: f64,
    pub needs_reply: f64,
    pub kind: String,
    pub kind_confidence: f64,
}

impl Verdict {
    pub fn worth_notifying(&self) -> bool {
        self.notify >= NOTIFY_THRESHOLD || self.needs_reply >= NOTIFY_THRESHOLD
    }
}

fn questions() -> serde_json::Value {
    serde_json::json!({
        "notify": {
            "type": "noul",
            "instructions": "Would a busy professional want an immediate desktop notification for this email?",
            "criteria": {
                "true": "Written by a real person the recipient knows, including a teacher, school, coach, family member, or colleague, even if sent to a group; or an urgent security or time-sensitive matter",
                "false": "Marketing, newsletter publication, receipt, shipping update, or routine automated notification"
            }
        },
        "needs_reply": {
            "type": "noul",
            "instructions": "Does this email expect a reply or action from the recipient?",
            "criteria": {
                "true": "Asks a question, requests something, or awaits a decision",
                "false": "Informational only; no response expected"
            }
        },
        "kind": {
            "type": "choice",
            "instructions": "What kind of email is this?",
            "criteria": {
                "person": "Written by a human to the recipient",
                "transactional": "Receipt, order, shipping, account or security notice",
                "newsletter": "Periodic content mailing",
                "promo": "Marketing or sales offer",
                "automated": "Machine-generated alert, digest, or system notification"
            }
        }
    })
}

fn state_for(thread: &ThreadSummary) -> serde_json::Value {
    serde_json::json!({
        "account": thread.account_id.as_str(),
        "from": thread.participants.first().cloned().unwrap_or_default(),
        "subject": thread.subject,
        "snippet": thread.snippet,
    })
}

/// Pull our answers out of the response. Workers AI nests them under one or
/// more `result` envelopes depending on the model, so unwrap until found.
pub fn parse_verdict(mut body: &serde_json::Value) -> Option<Verdict> {
    while body.get("answers").is_none() {
        body = body.get("result")?;
    }
    let answers = &body["answers"];
    let kind = answers.get("kind")?;
    Some(Verdict {
        notify: answers.get("notify")?.get("noul")?.as_f64()?,
        needs_reply: answers.get("needs_reply")?.get("noul")?.as_f64()?,
        kind: kind.get("choice")?.as_str()?.to_string(),
        kind_confidence: kind.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.0),
    })
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("reqwest client")
    })
}

pub async fn score(cfg: &TriageConfig, thread: &ThreadSummary) -> anyhow::Result<Verdict> {
    let url = format!(
        "https://api.cloudflare.com/client/v4/accounts/{}/ai/run/@cf/cloudflare/clef",
        cfg.cloudflare_account_id
    );
    let response = client()
        .post(&url)
        .bearer_auth(&cfg.cloudflare_api_token)
        .json(&serde_json::json!({ "state": state_for(thread), "questions": questions() }))
        .send()
        .await?;
    let status = response.status();
    let body: serde_json::Value = response.json().await?;
    if !status.is_success() {
        anyhow::bail!("clef http {status}: {body}");
    }
    parse_verdict(&body).ok_or_else(|| anyhow::anyhow!("unexpected clef response: {body}"))
}

/// Score every new thread. Log-only unless `cfg.gate` is set; then drop the
/// threads Clef says are not worth a notification. Errors keep the thread.
pub async fn gate_new_threads(
    cfg: Option<&TriageConfig>,
    threads: Vec<ThreadSummary>,
) -> Vec<ThreadSummary> {
    let Some(cfg) = cfg else { return threads };
    let mut kept = Vec::with_capacity(threads.len());
    // ponytail: sequential; new threads per 15s tick are a handful.
    for thread in threads {
        match score(cfg, &thread).await {
            Ok(verdict) => {
                let keep = verdict.worth_notifying();
                info!(
                    target: "mach::triage",
                    thread = thread.id.as_str(),
                    from = thread.participants.first().map(String::as_str).unwrap_or(""),
                    subject = thread.subject,
                    notify = verdict.notify,
                    needs_reply = verdict.needs_reply,
                    kind = verdict.kind,
                    kind_confidence = verdict.kind_confidence,
                    would_notify = keep,
                    gate = cfg.gate,
                    "clef scored new thread"
                );
                if keep || !cfg.gate {
                    kept.push(thread);
                }
            }
            Err(error) => {
                warn!(target: "mach::triage", thread = thread.id.as_str(), %error, "clef scoring failed; notifying anyway");
                kept.push(thread);
            }
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answers() -> serde_json::Value {
        serde_json::json!({
            "model": "clef",
            "answers": {
                "notify": { "type": "noul", "noul": 0.2 },
                "needs_reply": { "type": "noul", "noul": 0.9 },
                "kind": { "type": "choice", "choice": "person", "confidence": 0.8,
                          "probabilities": { "person": 0.8, "promo": 0.2 } }
            }
        })
    }

    #[test]
    fn parses_bare_and_nested_cloudflare_responses() {
        let bare = parse_verdict(&answers()).unwrap();
        let wrapped = parse_verdict(&serde_json::json!({ "success": true, "result": answers() })).unwrap();
        let double = parse_verdict(&serde_json::json!({ "result": { "state": "Completed", "result": answers() } })).unwrap();
        assert_eq!(bare, wrapped);
        assert_eq!(bare, double);
        assert_eq!(bare.kind, "person");
        assert!(bare.worth_notifying(), "needs_reply alone should pass the gate");
        assert!(parse_verdict(&serde_json::json!({ "errors": [] })).is_none());
    }

    #[test]
    fn low_scores_do_not_notify() {
        let verdict = Verdict { notify: 0.3, needs_reply: 0.1, kind: "promo".into(), kind_confidence: 0.9 };
        assert!(!verdict.worth_notifying());
    }
}
