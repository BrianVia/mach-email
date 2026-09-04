#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    pub terms: Vec<String>,
    pub from: Vec<String>,
    pub to: Vec<String>,
    pub subject: Vec<String>,
    pub labels: Vec<String>,
    pub is_unread: Option<bool>,
    pub is_starred: Option<bool>,
    pub has_attachment: bool,
    pub newer_than_days: Option<u32>,
    pub older_than_days: Option<u32>,
    pub raw: String,
}

impl SearchQuery {
    pub fn parse(input: &str) -> Self {
        let mut query = Self {
            terms: Vec::new(),
            from: Vec::new(),
            to: Vec::new(),
            subject: Vec::new(),
            labels: Vec::new(),
            is_unread: None,
            is_starred: None,
            has_attachment: false,
            newer_than_days: None,
            older_than_days: None,
            raw: input.to_owned(),
        };

        for token in tokens(input) {
            let Some((key, value)) = token.split_once(':') else {
                query.terms.push(token);
                continue;
            };
            let parsed = match key {
                "from" if !value.is_empty() => push(&mut query.from, value),
                "to" if !value.is_empty() => push(&mut query.to, value),
                "subject" if !value.is_empty() => push(&mut query.subject, value),
                "label" if !value.is_empty() => push(&mut query.labels, value),
                "is" if value == "unread" => set(&mut query.is_unread, true),
                "is" if value == "read" => set(&mut query.is_unread, false),
                "is" if value == "starred" => set(&mut query.is_starred, true),
                "has" if value == "attachment" => {
                    query.has_attachment = true;
                    true
                }
                "newer_than" => parse_days(value)
                    .map(|days| query.newer_than_days = Some(days))
                    .is_some(),
                "older_than" => parse_days(value)
                    .map(|days| query.older_than_days = Some(days))
                    .is_some(),
                _ => false,
            };
            if !parsed {
                query.terms.push(token);
            }
        }
        query
    }

    pub fn to_fts5(&self) -> Option<String> {
        let mut parts = self
            .terms
            .iter()
            .map(|term| quote(term))
            .collect::<Vec<_>>();
        parts.extend(
            self.from
                .iter()
                .map(|value| format!("from_addr:{}", quote(value))),
        );
        parts.extend(
            self.to
                .iter()
                .map(|value| format!("to_addrs:{}", quote(value))),
        );
        parts.extend(
            self.subject
                .iter()
                .map(|value| format!("subject:{}", quote(value))),
        );
        (!parts.is_empty()).then(|| parts.join(" "))
    }
}

fn tokens(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quoted = false;
    for ch in input.chars() {
        match ch {
            '"' => quoted = !quoted,
            ch if ch.is_whitespace() && !quoted => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
            }
            _ => token.push(ch),
        }
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    tokens
}

fn push(values: &mut Vec<String>, value: &str) -> bool {
    values.push(value.to_owned());
    true
}

fn set(value: &mut Option<bool>, new: bool) -> bool {
    *value = Some(new);
    true
}

fn parse_days(value: &str) -> Option<u32> {
    value.strip_suffix('d').unwrap_or(value).parse().ok()
}

fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_words_and_search_operators() {
        let plain = SearchQuery::parse("quarterly invoice");
        assert_eq!(plain.terms, ["quarterly", "invoice"]);
        assert_eq!(
            plain.to_fts5().as_deref(),
            Some("\"quarterly\" \"invoice\"")
        );

        let query = SearchQuery::parse(
            "from:bob subject:\"project update\" invoice is:unread is:starred has:attachment newer_than:7d",
        );
        assert_eq!(query.from, ["bob"]);
        assert_eq!(query.subject, ["project update"]);
        assert_eq!(query.terms, ["invoice"]);
        assert_eq!(query.is_unread, Some(true));
        assert_eq!(query.is_starred, Some(true));
        assert!(query.has_attachment);
        assert_eq!(query.newer_than_days, Some(7));
        assert_eq!(
            query.to_fts5().as_deref(),
            Some("\"invoice\" from_addr:\"bob\" subject:\"project update\"")
        );
    }

    #[test]
    fn unknown_operator_is_a_hyphen_safe_free_term() {
        let query = SearchQuery::parse("unknown:value build-status");
        assert_eq!(query.terms, ["unknown:value", "build-status"]);
        assert_eq!(
            query.to_fts5().as_deref(),
            Some("\"unknown:value\" \"build-status\"")
        );
    }

    #[test]
    fn operators_only_have_no_fts_expression() {
        assert_eq!(SearchQuery::parse("is:read").to_fts5(), None);
        assert_eq!(
            SearchQuery::parse("older_than:30").older_than_days,
            Some(30)
        );
    }
}
