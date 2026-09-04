use std::collections::HashSet;

use crate::ids::{MessageId, ThreadId};
use crate::store::Message;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prefill {
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub body_md: String,
    pub thread_id: Option<ThreadId>,
    pub in_reply_to_message_id: Option<MessageId>,
}

pub fn reply_prefill(source: &Message, self_email: &str, all: bool) -> Prefill {
    let (to, cc) = if all {
        reply_all_recipients(source, self_email)
    } else {
        let recipient = source
            .headers
            .as_ref()
            .and_then(|headers| headers.reply_to.as_deref())
            .unwrap_or(&source.from)
            .to_string();
        (vec![recipient], Vec::new())
    };
    let original = source.body_plain.as_deref().unwrap_or(&source.snippet);
    let quoted = original
        .split('\n')
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n");

    Prefill {
        to,
        cc,
        subject: prefixed_subject(&source.subject, "Re: "),
        body_md: format!(
            "\n\nOn {}, {} wrote:\n{}",
            source.internal_date.format("%Y-%m-%d"),
            source.from,
            quoted
        ),
        thread_id: Some(source.thread_id.clone()),
        in_reply_to_message_id: Some(source.id.clone()),
    }
}

pub fn forward_prefill(source: &Message) -> Prefill {
    let original = source.body_plain.as_deref().unwrap_or(&source.snippet);
    Prefill {
        to: Vec::new(),
        cc: Vec::new(),
        subject: prefixed_subject(&source.subject, "Fwd: "),
        body_md: format!(
            "\n\n---------- Forwarded message ----------\nFrom: {}\nDate: {}\nSubject: {}\nTo: {}\n\n{}",
            source.from,
            source.internal_date.format("%Y-%m-%d"),
            source.subject,
            source.to.join(", "),
            original
        ),
        thread_id: None,
        in_reply_to_message_id: None,
    }
}

fn reply_all_recipients(source: &Message, self_email: &str) -> (Vec<String>, Vec<String>) {
    let mut seen = HashSet::new();
    let mut to = Vec::new();
    for address in std::iter::once(&source.from).chain(source.to.iter()) {
        let key = normalized_addr_spec(address);
        if key.is_empty() || key == normalized_addr_spec(self_email) || !seen.insert(key) {
            continue;
        }
        to.push(address.clone());
    }
    if to.is_empty() {
        to.push(source.from.clone());
        seen.insert(normalized_addr_spec(&source.from));
    }

    let mut cc = Vec::new();
    for address in &source.cc {
        let key = normalized_addr_spec(address);
        if key.is_empty() || key == normalized_addr_spec(self_email) || !seen.insert(key) {
            continue;
        }
        cc.push(address.clone());
    }
    (to, cc)
}

pub(crate) fn normalized_addr_spec(address: &str) -> String {
    addr_spec(address).to_ascii_lowercase()
}

fn addr_spec(address: &str) -> &str {
    let trimmed = address.trim();
    if let Some(start) = trimmed.rfind('<') {
        if let Some(relative_end) = trimmed[start + 1..].find('>') {
            return trimmed[start + 1..start + 1 + relative_end].trim();
        }
    }
    trimmed
}

fn prefixed_subject(subject: &str, prefix: &str) -> String {
    format!("{prefix}{}", strip_subject_prefixes(subject))
}

fn strip_subject_prefixes(subject: &str) -> &str {
    let mut rest = subject.trim_start();
    loop {
        let lower = rest.to_ascii_lowercase();
        let prefix_len = ["re:", "fwd:", "fw:"]
            .into_iter()
            .find(|prefix| lower.starts_with(prefix))
            .map(str::len);
        let Some(prefix_len) = prefix_len else {
            return rest;
        };
        rest = rest[prefix_len..].trim_start();
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::ids::{AccountId, LabelId};
    use crate::store::{Message, MessageHeaders};

    fn message() -> Message {
        Message {
            account_id: AccountId::new("me@example.com"),
            id: MessageId::new("gmail-message"),
            thread_id: ThreadId::new("gmail-thread"),
            from: "Alice Example <ALICE@example.com>".into(),
            to: vec!["Me <me@example.com>".into(), "Bob <bob@example.com>".into()],
            cc: vec![
                "Bob Duplicate <BOB@example.com>".into(),
                "Carol <carol@example.com>".into(),
                "ME@EXAMPLE.COM".into(),
            ],
            subject: "Re: Re: Fwd: Project x".into(),
            snippet: "snippet fallback".into(),
            internal_date: Utc.with_ymd_and_hms(2026, 8, 11, 14, 0, 0).unwrap(),
            body_plain: Some("first\n\nthird".into()),
            body_html: None,
            headers: Some(MessageHeaders {
                reply_to: Some("Team Replies <reply@example.com>".into()),
                ..MessageHeaders::default()
            }),
            label_ids: vec![LabelId::new("INBOX")],
            fetched_full: true,
            inline_images: vec![],
        }
    }

    #[test]
    fn reply_prefers_reply_to_and_reply_all_excludes_self_and_dedupes() {
        let source = message();
        let reply = reply_prefill(&source, "me@example.com", false);
        assert_eq!(reply.to, ["Team Replies <reply@example.com>"]);
        assert!(reply.cc.is_empty());

        let all = reply_prefill(&source, "ME@example.com", true);
        assert_eq!(
            all.to,
            ["Alice Example <ALICE@example.com>", "Bob <bob@example.com>"]
        );
        assert_eq!(all.cc, ["Carol <carol@example.com>"]);
    }

    #[test]
    fn replying_to_self_falls_back_to_from() {
        let mut source = message();
        source.from = "Me <me@example.com>".into();
        source.to = vec!["ME@example.com".into()];
        source.cc.clear();
        assert_eq!(
            reply_prefill(&source, "me@example.com", true).to,
            ["Me <me@example.com>"]
        );
    }

    #[test]
    fn subject_prefixes_are_stripped_repeatedly() {
        let source = message();
        assert_eq!(
            reply_prefill(&source, "me@example.com", false).subject,
            "Re: Project x"
        );
        assert_eq!(forward_prefill(&source).subject, "Fwd: Project x");
    }

    #[test]
    fn reply_quotes_every_body_line() {
        let body = reply_prefill(&message(), "me@example.com", false).body_md;
        assert_eq!(
            body,
            "\n\nOn 2026-08-11, Alice Example <ALICE@example.com> wrote:\n> first\n> \n> third"
        );
    }

    #[test]
    fn forward_has_unquoted_forwarded_block() {
        let body = forward_prefill(&message()).body_md;
        assert_eq!(
            body,
            "\n\n---------- Forwarded message ----------\nFrom: Alice Example <ALICE@example.com>\nDate: 2026-08-11\nSubject: Re: Re: Fwd: Project x\nTo: Me <me@example.com>, Bob <bob@example.com>\n\nfirst\n\nthird"
        );
    }

    #[test]
    fn address_helpers_extract_and_compare_addr_specs() {
        assert_eq!(addr_spec("Name <A@Example.COM>"), "A@Example.COM");
        assert_eq!(
            normalized_addr_spec("Name <A@Example.COM>"),
            "a@example.com"
        );
    }
}
