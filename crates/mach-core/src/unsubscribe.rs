use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::store::MessageHeaders;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UnsubscribeTarget {
    Https { url: String, one_click: bool },
    Mailto { to: String, subject: String },
}

/// Best first: an RFC 8058 one-click POST needs nothing from the user, a
/// mailto sends from their account, and a plain https page needs a browser.
pub fn unsubscribe_targets(headers: &MessageHeaders) -> Vec<UnsubscribeTarget> {
    let one_click = headers.list_unsubscribe_post.as_deref() == Some("List-Unsubscribe=One-Click");
    let mut targets: Vec<_> = headers
        .list_unsubscribe
        .as_deref()
        .into_iter()
        .flat_map(|value| value.split(','))
        .filter_map(|value| {
            let raw = value.trim().strip_prefix('<')?.strip_suffix('>')?;
            let parsed = Url::parse(raw).ok()?;
            match parsed.scheme() {
                "https" => Some(UnsubscribeTarget::Https {
                    url: parsed.into(),
                    one_click,
                }),
                "mailto" => Some(UnsubscribeTarget::Mailto {
                    to: parsed.path().to_string(),
                    subject: parsed
                        .query_pairs()
                        .find_map(|(key, value)| {
                            (key.eq_ignore_ascii_case("subject")).then(|| value.into_owned())
                        })
                        .unwrap_or_default(),
                }),
                _ => None,
            }
        })
        .collect();
    targets.sort_by_key(|target| match target {
        UnsubscribeTarget::Https { one_click: true, .. } => 0,
        UnsubscribeTarget::Mailto { .. } => 1,
        UnsubscribeTarget::Https { one_click: false, .. } => 2,
    });
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(list: &str, post: Option<&str>) -> MessageHeaders {
        MessageHeaders {
            list_unsubscribe: Some(list.into()),
            list_unsubscribe_post: post.map(str::to_string),
            ..MessageHeaders::default()
        }
    }

    #[test]
    fn parses_https_only() {
        assert_eq!(
            unsubscribe_targets(&headers("<https://example.com/unsubscribe>", None)),
            [UnsubscribeTarget::Https {
                url: "https://example.com/unsubscribe".into(),
                one_click: false,
            }]
        );
    }

    #[test]
    fn parses_mailto_subject() {
        assert_eq!(
            unsubscribe_targets(&headers(
                "<mailto:leave@example.com?subject=Please%20remove%20me>",
                None,
            )),
            [UnsubscribeTarget::Mailto {
                to: "leave@example.com".into(),
                subject: "Please remove me".into(),
            }]
        );
    }

    #[test]
    fn parses_both() {
        assert_eq!(
            unsubscribe_targets(&headers(
                "<https://example.com/u>, <mailto:leave@example.com>",
                None,
            ))
            .len(),
            2
        );
    }

    #[test]
    fn orders_one_click_then_mailto_then_page() {
        let mailto = UnsubscribeTarget::Mailto {
            to: "leave@example.com".into(),
            subject: String::new(),
        };
        assert_eq!(
            unsubscribe_targets(&headers(
                "<mailto:leave@example.com>, <https://example.com/u>",
                Some("List-Unsubscribe=One-Click"),
            )),
            [
                UnsubscribeTarget::Https {
                    url: "https://example.com/u".into(),
                    one_click: true,
                },
                mailto.clone(),
            ]
        );
        assert_eq!(
            unsubscribe_targets(&headers("<https://example.com/u>, <mailto:leave@example.com>", None)),
            [
                mailto,
                UnsubscribeTarget::Https {
                    url: "https://example.com/u".into(),
                    one_click: false,
                },
            ]
        );
    }

    #[test]
    fn marks_one_click_https() {
        assert_eq!(
            unsubscribe_targets(&headers(
                "<https://example.com/u>",
                Some("List-Unsubscribe=One-Click"),
            )),
            [UnsubscribeTarget::Https {
                url: "https://example.com/u".into(),
                one_click: true,
            }]
        );
    }
}
