//! Walk a Gmail payload tree and pull out text/plain + text/html bodies.
//!
//! Gmail returns a recursive `MessagePayload`: `multipart/*` nodes have
//! `parts`, leaves have `body.data` (base64url, **no padding**). We do a
//! preorder traversal preferring the *first* `text/plain` we encounter; if
//! none exists, we fall back to HTML → plaintext via `html2text`.

use base64::{
    alphabet,
    engine::{general_purpose::GeneralPurpose, DecodePaddingMode, GeneralPurposeConfig},
    Engine as _,
};
use tracing::trace;

/// URL-safe base64 that accepts EITHER padded or unpadded input. Gmail
/// pads some messages and not others — we tolerate both.
const B64URL: GeneralPurpose = GeneralPurpose::new(
    &alphabet::URL_SAFE,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

use crate::client::MessagePayload;

/// Materialized body for one message.
#[derive(Debug, Clone, Default)]
pub struct ParsedBody {
    pub plain: Option<String>,
    pub html: Option<String>,
    pub calendar: Option<String>,
    pub calendar_attachment_id: Option<String>,
    /// Inline image attachments referenced from `body_html` via `cid:`.
    /// Populated only when at least one `image/*` part with a `Content-ID`
    /// header is seen during the MIME walk.
    pub inline_images: Vec<InlineImageRef>,
}

/// Reference to one inline image. We don't fetch bytes here — that's
/// deferred until the desktop renderer asks for it via the `mach://`
/// custom protocol.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InlineImageRef {
    /// `cid:` body references match against this. Stripped of `<>` brackets.
    pub content_id: String,
    /// Gmail's attachment ID — needed for `users.messages.attachments.get`.
    pub attachment_id: String,
    pub mime_type: String,
    pub filename: Option<String>,
    pub size: Option<i64>,
}

impl ParsedBody {
    /// Effective plaintext — return `plain` if present, otherwise HTML
    /// stripped to text. Used for the FTS5 index and surface rendering.
    ///
    /// We pass an enormous wrap width to html2text so URLs stay on a single
    /// line in storage. Each surface re-wraps at viewport width at render
    /// time (ratatui's `Wrap`, browser `white-space: pre-wrap`). Wrapping
    /// in *storage* destroys URLs and is a one-way operation we'd rather
    /// not commit to.
    ///
    /// Output is also cleaned: HTML entities are decoded and html2text's
    /// `[Image]` placeholders are replaced with a single 🖼 marker so the
    /// rendered body doesn't read like a parser dump.
    pub fn effective_plain(&self) -> Option<String> {
        let raw = if let Some(p) = &self.plain {
            p.clone()
        } else {
            let html = self.html.as_deref()?;
            // Strip `<style>`, `<script>`, `<head>` content BEFORE passing
            // to html2text — the crate happily echoes CSS rules out as
            // "text" otherwise (the visible bug on the TUI when a sender's
            // marketing HTML lacks a text/plain alternative).
            let stripped = strip_noise_blocks(html);
            html2text::from_read(stripped.as_bytes(), 1_000_000).ok()?
        };
        Some(clean(&raw))
    }
}

/// Drop everything inside `<style>`, `<script>`, and `<head>` blocks —
/// case-insensitive, multi-line. We *keep* the rest of the HTML so
/// html2text can extract the visible body.
fn strip_noise_blocks(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let lower = html.to_ascii_lowercase();
    let mut i = 0;
    while i < html.len() {
        // Find the next noise-block open tag from our current position.
        let next = ["<style", "<script", "<head"]
            .iter()
            .filter_map(|tag| lower[i..].find(tag).map(|n| (i + n, *tag)))
            .min_by_key(|(pos, _)| *pos);
        match next {
            Some((start, tag)) => {
                out.push_str(&html[i..start]);
                // Close-tag derived from the opener: "<style" → "</style>".
                let close = format!("</{}>", tag.trim_start_matches('<'));
                let close_pos = lower[start..]
                    .find(&close)
                    .map(|n| start + n + close.len())
                    .unwrap_or(html.len());
                i = close_pos;
            }
            None => {
                out.push_str(&html[i..]);
                break;
            }
        }
    }
    out
}

/// Common cleanup applied to all plaintext bodies — regardless of whether
/// the source was text/plain or html→text. Marketing tools (Hyperagent,
/// Substack, Mailchimp) generate text/plain alternatives that still
/// contain `&amp;` and `[Image]` placeholders, so we normalize them away.
pub fn clean(s: &str) -> String {
    let s = decode_entities(s);
    replace_image_markers(&s)
}

fn replace_image_markers(s: &str) -> String {
    // Match `[Image]`, `[Image: alt-text]`, and similar. Replaces with a
    // single 🖼 glyph so the reader sees one visual marker, not a column of
    // them.
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '[' {
            // Peek-then-match without allocating.
            let rest: String = chars
                .clone()
                .take_while(|c| *c != ']' && *c != '\n')
                .collect();
            if rest.starts_with("Image") {
                // Consume up through the `]`.
                for ch in chars.by_ref() {
                    if ch == ']' {
                        break;
                    }
                }
                out.push('🖼');
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// Decode the most common HTML entities. Hand-rolled because pulling in a
/// full HTML parser for five named entities + numeric refs would be
/// overkill. Covers `&amp; &lt; &gt; &quot; &apos; &#39; &nbsp;
/// &mdash; &ndash; &hellip; &copy; &reg; &trade; &#NN; &#xHH;`.
fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'&' {
            if let Some(end) = find_semicolon(bytes, i + 1, 10) {
                let ent = &s[i + 1..end];
                if let Some(decoded) = lookup_entity(ent) {
                    out.push_str(&decoded);
                    i = end + 1;
                    continue;
                }
            }
        }
        // SAFETY: i is at a UTF-8 char boundary because we only advance to
        // semicolon offsets we just verified, and `&` is single-byte ASCII.
        let c = s[i..].chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    out
}

fn find_semicolon(bytes: &[u8], start: usize, max_scan: usize) -> Option<usize> {
    let end = (start + max_scan).min(bytes.len());
    bytes[start..end]
        .iter()
        .position(|b| *b == b';')
        .map(|p| start + p)
}

fn lookup_entity(name: &str) -> Option<String> {
    let s = match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" | "#39" => "'",
        "nbsp" | "#160" | "#xa0" | "#xA0" => " ",
        "mdash" | "#8212" => "—",
        "ndash" | "#8211" => "–",
        "hellip" | "#8230" => "…",
        "copy" => "©",
        "reg" => "®",
        "trade" => "™",
        "lsquo" => "‘",
        "rsquo" => "’",
        "ldquo" => "“",
        "rdquo" => "”",
        // Numeric: &#NN; or &#xHH;
        num if num.starts_with('#') => {
            let rest = &num[1..];
            let code = if let Some(hex) = rest.strip_prefix('x').or_else(|| rest.strip_prefix('X'))
            {
                u32::from_str_radix(hex, 16).ok()?
            } else {
                rest.parse::<u32>().ok()?
            };
            return char::from_u32(code).map(|c| c.to_string());
        }
        _ => return None,
    };
    Some(s.to_string())
}

/// Walk the payload tree preorder, accumulating the first text/plain and
/// first text/html we find. Stops descending into attachments
/// (`attachment_id` present) and parts without `body.data`.
pub fn extract(payload: &MessagePayload) -> ParsedBody {
    let mut out = ParsedBody::default();
    walk(payload, &mut out);
    out
}

fn walk(p: &MessagePayload, out: &mut ParsedBody) {
    let mime = p.mime_type.as_deref().unwrap_or("");

    // Inline image: attachment_id present + Content-ID header points back
    // from the message body. We harvest the reference so the desktop can
    // fetch it lazily via `mach://attachment/...`.
    if mime.starts_with("image/") {
        if let Some(body) = &p.body {
            if let Some(att_id) = body.attachment_id.clone() {
                if let Some(cid) = header_value(&p.headers, "Content-ID") {
                    let cid = cid
                        .trim()
                        .trim_start_matches('<')
                        .trim_end_matches('>')
                        .to_string();
                    let filename = header_value(&p.headers, "Content-Disposition")
                        .and_then(extract_filename)
                        .or_else(|| {
                            header_value(&p.headers, "Content-Type").and_then(extract_filename)
                        });
                    out.inline_images.push(InlineImageRef {
                        content_id: cid,
                        attachment_id: att_id,
                        mime_type: mime.to_string(),
                        filename,
                        size: body.size,
                    });
                }
            }
        }
    }

    // Leaf with data → maybe slot it in.
    if let Some(body) = &p.body {
        if body.attachment_id.is_some() {
            if matches!(mime, "text/calendar" | "application/ics")
                && out.calendar.is_none()
                && out.calendar_attachment_id.is_none()
            {
                out.calendar_attachment_id = body.attachment_id.clone();
            }
        } else if let Some(data) = &body.data {
            match mime {
                "text/plain" if out.plain.is_none() => {
                    if let Some(s) = decode_b64url(data) {
                        out.plain = Some(s);
                    }
                }
                "text/html" if out.html.is_none() => {
                    if let Some(s) = decode_b64url(data) {
                        out.html = Some(s);
                    }
                }
                "text/calendar" | "application/ics"
                    if out.calendar.is_none() && out.calendar_attachment_id.is_none() =>
                {
                    if let Some(s) = decode_b64url(data) {
                        out.calendar = Some(s);
                    }
                }
                _ => {
                    trace!(mime, "ignored leaf part");
                }
            }
        }
    }

    if let Some(parts) = &p.parts {
        for child in parts {
            walk(child, out);
            // No early break: we want to keep walking to collect inline
            // images even after plain+html are both set.
        }
    }
}

fn header_value(headers: &[crate::client::MessageHeader], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case(name))
        .map(|h| h.value.clone())
}

/// Pull a `filename="..."` parameter out of a Content-Disposition or
/// Content-Type header. Returns None if absent.
fn extract_filename(header_value: String) -> Option<String> {
    for part in header_value.split(';') {
        let p = part.trim();
        if let Some(rest) = p
            .strip_prefix("filename=")
            .or_else(|| p.strip_prefix("name="))
        {
            let rest = rest.trim().trim_matches('"');
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    None
}

fn decode_b64url(data: &str) -> Option<String> {
    // Gmail uses URL-safe alphabet (`-`/`_`). Padding is inconsistent — some
    // messages have `==`, some don't. `Indifferent` mode tolerates both.
    // Also: bodies often have CR/LF wrapping that base64 ignores; decode
    // happily strips it.
    B64URL
        .decode(data.trim())
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{MessageBody, MessagePayload};

    fn leaf(mime: &str, raw: &str) -> MessagePayload {
        MessagePayload {
            headers: vec![],
            mime_type: Some(mime.into()),
            body: Some(MessageBody {
                data: Some(B64URL.encode(raw.as_bytes())),
                size: Some(raw.len() as i64),
                attachment_id: None,
            }),
            parts: None,
        }
    }

    #[test]
    fn extracts_text_plain_from_simple_message() {
        let body = extract(&leaf("text/plain", "hello world"));
        assert_eq!(body.plain.as_deref(), Some("hello world"));
        assert!(body.html.is_none());
    }

    #[test]
    fn extracts_first_calendar_part() {
        let payload = MessagePayload {
            headers: vec![],
            mime_type: Some("multipart/mixed".into()),
            body: None,
            parts: Some(vec![
                leaf("text/calendar", "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n"),
                leaf("text/calendar", "ignored"),
            ]),
        };
        assert_eq!(
            extract(&payload).calendar.as_deref(),
            Some("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n")
        );
    }

    #[test]
    fn picks_first_plain_in_multipart_alternative() {
        let p = MessagePayload {
            headers: vec![],
            mime_type: Some("multipart/alternative".into()),
            body: None,
            parts: Some(vec![
                leaf("text/plain", "PLAIN"),
                leaf("text/html", "<p>HTML</p>"),
            ]),
        };
        let body = extract(&p);
        assert_eq!(body.plain.as_deref(), Some("PLAIN"));
        assert_eq!(body.html.as_deref(), Some("<p>HTML</p>"));
    }

    #[test]
    fn falls_back_to_html_when_only_html_present() {
        let body = extract(&leaf("text/html", "<p>Hello <b>world</b></p>"));
        assert!(body.plain.is_none());
        let stripped = body.effective_plain().unwrap();
        assert!(stripped.contains("Hello"));
        assert!(stripped.contains("world"));
        assert!(!stripped.contains("<p>"));
    }

    #[test]
    fn ignores_attachment_parts() {
        let mut att = leaf("application/pdf", "fakepdf");
        att.body.as_mut().unwrap().attachment_id = Some("att1".into());
        let p = MessagePayload {
            headers: vec![],
            mime_type: Some("multipart/mixed".into()),
            body: None,
            parts: Some(vec![leaf("text/plain", "real body"), att]),
        };
        let body = extract(&p);
        assert_eq!(body.plain.as_deref(), Some("real body"));
    }

    #[test]
    fn decode_named_entities() {
        assert_eq!(clean("Share agents &amp; skills"), "Share agents & skills");
        assert_eq!(clean("&lt;script&gt;"), "<script>");
        assert_eq!(clean("&quot;hi&quot; he said"), "\"hi\" he said");
        assert_eq!(clean("rock&nbsp;solid"), "rock solid");
        assert_eq!(clean("M&amp;Ms &mdash; tasty"), "M&Ms — tasty");
    }

    #[test]
    fn decode_numeric_entities() {
        assert_eq!(clean("&#65;&#66;&#67;"), "ABC");
        assert_eq!(clean("&#xa0;hi"), " hi");
        assert_eq!(clean("&#8212;dash"), "—dash");
    }

    #[test]
    fn unknown_entities_pass_through() {
        // Don't corrupt unfamiliar entities — better to show `&xyz;` than
        // silently drop it.
        assert_eq!(clean("&xyz;"), "&xyz;");
    }

    #[test]
    fn image_marker_collapsed() {
        assert_eq!(clean("Hello\n[Image]\nWorld"), "Hello\n🖼\nWorld");
        assert_eq!(clean("[Image: logo.png] inline"), "🖼 inline");
    }

    #[test]
    fn url_with_query_string_survives_entity_pass() {
        // `&` inside URLs that *aren't* HTML entities should not be touched
        // beyond entity decoding. The URL stays whole.
        let s = "https://example.com/path?a=1&amp;b=2";
        assert_eq!(clean(s), "https://example.com/path?a=1&b=2");
    }

    #[test]
    fn strip_noise_blocks_drops_style_content() {
        let html = "<p>hi</p><style>.foo { color: red; }</style><p>bye</p>";
        let s = strip_noise_blocks(html);
        assert_eq!(s, "<p>hi</p><p>bye</p>");
    }

    #[test]
    fn strip_noise_blocks_drops_script_and_head() {
        let html = "<html><head><title>x</title></head><body>visible<script>alert(1)</script>more</body></html>";
        let s = strip_noise_blocks(html);
        assert!(!s.contains("title"));
        assert!(!s.contains("alert"));
        assert!(s.contains("visible"));
        assert!(s.contains("more"));
    }

    #[test]
    fn strip_noise_blocks_case_insensitive() {
        let html = "<P>x</P><STYLE>p{color:red}</STYLE>y";
        let s = strip_noise_blocks(html);
        assert!(!s.contains("color"));
        assert!(s.contains("<P>x</P>"));
    }

    #[test]
    fn html_only_body_no_longer_echoes_css() {
        // Reproduce the Hyperagent-style email: HTML with embedded styles
        // and no text/plain alternative.
        let p = MessagePayload {
            headers: vec![],
            mime_type: Some("text/html".into()),
            body: Some(MessageBody {
                data: Some(B64URL.encode(
                    r#"<html><head><style>.cta{background:#7c9cff}</style></head>
                       <body><p>Hello world</p><p>Visit <a href="https://example.com">us</a>.</p></body></html>"#
                        .as_bytes(),
                )),
                size: None,
                attachment_id: None,
            }),
            parts: None,
        };
        let body = extract(&p);
        let plain = body.effective_plain().unwrap();
        assert!(!plain.contains("background"));
        assert!(!plain.contains("#7c9cff"));
        assert!(plain.contains("Hello world"));
    }
}
