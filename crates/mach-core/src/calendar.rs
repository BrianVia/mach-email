use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, Utc, Weekday};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PartStat {
    NeedsAction,
    Accepted,
    Declined,
    Tentative,
}

impl PartStat {
    pub fn as_ics(self) -> &'static str {
        match self {
            Self::NeedsAction => "NEEDS-ACTION",
            Self::Accepted => "ACCEPTED",
            Self::Declined => "DECLINED",
            Self::Tentative => "TENTATIVE",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarAttendee {
    pub email: String,
    pub partstat: PartStat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarInvite {
    pub uid: String,
    pub summary: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub location: Option<String>,
    pub organizer: String,
    pub attendees: Vec<CalendarAttendee>,
    pub method: Option<String>,
    pub sequence: u32,
    pub my_status: Option<PartStat>,
}

pub fn parse_calendar(ics: &str, account_email: &str) -> Option<CalendarInvite> {
    let lines = unfold(ics);
    let mut in_event = false;
    let mut method = None;
    let mut uid = None;
    let mut summary = None;
    let mut starts_at = None;
    let mut ends_at = None;
    let mut location = None;
    let mut organizer = None;
    let mut attendees = Vec::new();
    let mut sequence = 0;

    for line in lines {
        if line.eq_ignore_ascii_case("BEGIN:VEVENT") {
            in_event = true;
            continue;
        }
        if line.eq_ignore_ascii_case("END:VEVENT") {
            break;
        }
        let Some((head, value)) = line.split_once(':') else {
            continue;
        };
        let name = head.split(';').next().unwrap_or(head).to_ascii_uppercase();
        if !in_event {
            if name == "METHOD" {
                method = Some(value.trim().to_ascii_uppercase());
            }
            continue;
        }
        match name.as_str() {
            "UID" => uid = Some(unescape(value)),
            "SUMMARY" => summary = Some(unescape(value)),
            "DTSTART" => starts_at = parse_datetime(head, value),
            "DTEND" => ends_at = parse_datetime(head, value),
            "LOCATION" => location = Some(unescape(value)),
            "ORGANIZER" => organizer = Some(mailto(value)),
            "ATTENDEE" => attendees.push(CalendarAttendee {
                email: mailto(value),
                partstat: parameter(head, "PARTSTAT")
                    .and_then(parse_partstat)
                    .unwrap_or(PartStat::NeedsAction),
            }),
            "METHOD" => method = Some(value.trim().to_ascii_uppercase()),
            "SEQUENCE" => sequence = value.trim().parse().unwrap_or(0),
            _ => {}
        }
    }

    let starts_at = starts_at?;
    let my_status = attendees
        .iter()
        .find(|attendee| attendee.email.eq_ignore_ascii_case(account_email))
        .map(|attendee| attendee.partstat);
    Some(CalendarInvite {
        uid: uid?,
        summary: summary.unwrap_or_else(|| "(no title)".into()),
        starts_at,
        ends_at: ends_at.unwrap_or(starts_at),
        location: location.filter(|value| !value.is_empty()),
        organizer: organizer?,
        attendees,
        method,
        sequence,
        my_status,
    })
}

pub fn build_reply_ics(invite: &CalendarInvite, account_email: &str, status: PartStat) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//mach//Calendar Reply//EN\r\nMETHOD:REPLY\r\nBEGIN:VEVENT\r\nUID:{}\r\nSEQUENCE:{}\r\nDTSTART:{}\r\nDTEND:{}\r\nSUMMARY:{}\r\nORGANIZER:mailto:{}\r\nATTENDEE;PARTSTAT={}:mailto:{}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        escape(&invite.uid),
        invite.sequence,
        invite.starts_at.format("%Y%m%dT%H%M%SZ"),
        invite.ends_at.format("%Y%m%dT%H%M%SZ"),
        escape(&invite.summary),
        invite.organizer,
        status.as_ics(),
        account_email,
    )
}

fn unfold(ics: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in ics.replace("\r\n", "\n").split('\n') {
        if line.starts_with([' ', '\t']) {
            if let Some(previous) = lines.last_mut() {
                previous.push_str(&line[1..]);
            }
        } else {
            lines.push(line.trim_end_matches('\r').to_string());
        }
    }
    lines
}

fn parameter<'a>(head: &'a str, wanted: &str) -> Option<&'a str> {
    head.split(';').skip(1).find_map(|part| {
        let (name, value) = part.split_once('=')?;
        name.eq_ignore_ascii_case(wanted)
            .then(|| value.trim_matches('"'))
    })
}

fn parse_partstat(value: &str) -> Option<PartStat> {
    match value.to_ascii_uppercase().as_str() {
        "NEEDS-ACTION" => Some(PartStat::NeedsAction),
        "ACCEPTED" => Some(PartStat::Accepted),
        "DECLINED" => Some(PartStat::Declined),
        "TENTATIVE" => Some(PartStat::Tentative),
        _ => None,
    }
}

fn mailto(value: &str) -> String {
    value
        .trim()
        .strip_prefix("mailto:")
        .or_else(|| value.trim().strip_prefix("MAILTO:"))
        .unwrap_or(value.trim())
        .to_string()
}

fn unescape(value: &str) -> String {
    value
        .replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\n', "\\n")
}

fn parse_datetime(head: &str, value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim();
    if let Ok(datetime) = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%SZ") {
        return Some(datetime.and_utc());
    }
    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y%m%d") {
        return Some(date.and_hms_opt(0, 0, 0)?.and_utc());
    }
    let datetime = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S").ok()?;
    let offset = parameter(head, "TZID")
        .map(|tzid| timezone_offset_seconds(tzid, datetime.date()))
        .unwrap_or(0);
    Some((datetime - Duration::seconds(i64::from(offset))).and_utc())
}

fn timezone_offset_seconds(tzid: &str, date: NaiveDate) -> i32 {
    let tzid = tzid.trim_start_matches('/');
    let (standard, daylight, rule) = match tzid {
        "America/New_York"
        | "America/Detroit"
        | "America/Toronto"
        | "US/Eastern"
        | "Eastern Standard Time" => (-5, -4, "us"),
        "America/Chicago" | "US/Central" | "Central Standard Time" => (-6, -5, "us"),
        "America/Denver" | "US/Mountain" | "Mountain Standard Time" => (-7, -6, "us"),
        "America/Los_Angeles" | "US/Pacific" | "Pacific Standard Time" => (-8, -7, "us"),
        "Europe/London" | "GMT Standard Time" => (0, 1, "eu"),
        "Europe/Paris"
        | "Europe/Berlin"
        | "Europe/Amsterdam"
        | "Europe/Rome"
        | "W. Europe Standard Time" => (1, 2, "eu"),
        "Asia/Kolkata" | "Asia/Calcutta" | "India Standard Time" => return 19_800,
        "Asia/Tokyo" | "Tokyo Standard Time" => return 32_400,
        "Australia/Sydney" | "AUS Eastern Standard Time" => (10, 11, "au"),
        "UTC" | "Etc/UTC" | "GMT" => return 0,
        // ponytail: no timezone dependency; extend aliases when a real invite needs one.
        _ => return 0,
    };
    let daylight_active = match rule {
        "us" => {
            date >= nth_weekday(date.year(), 3, Weekday::Sun, 2)
                && date < nth_weekday(date.year(), 11, Weekday::Sun, 1)
        }
        "eu" => {
            date >= last_weekday(date.year(), 3, Weekday::Sun)
                && date < last_weekday(date.year(), 10, Weekday::Sun)
        }
        "au" => {
            date < nth_weekday(date.year(), 4, Weekday::Sun, 1)
                || date >= nth_weekday(date.year(), 10, Weekday::Sun, 1)
        }
        _ => false,
    };
    3600 * if daylight_active { daylight } else { standard }
}

fn nth_weekday(year: i32, month: u32, weekday: Weekday, nth: u32) -> NaiveDate {
    let first = NaiveDate::from_ymd_opt(year, month, 1).expect("valid month");
    let days = (7 + weekday.num_days_from_monday() as i64
        - first.weekday().num_days_from_monday() as i64)
        % 7;
    first + Duration::days(days + i64::from((nth - 1) * 7))
}

fn last_weekday(year: i32, month: u32, weekday: Weekday) -> NaiveDate {
    let next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    }
    .expect("valid month");
    let last = next - Duration::days(1);
    last - Duration::days(
        (7 + last.weekday().num_days_from_monday() as i64 - weekday.num_days_from_monday() as i64)
            % 7,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOGLE: &str = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:google-123\r\nSUMMARY:Product sync\r\nDTSTART;TZID=America/New_York:20260908T100000\r\nDTEND;TZID=America/New_York:20260908T103000\r\nLOCATION:Zoom\r\nORGANIZER;CN=Alex:mailto:alex@example.com\r\nATTENDEE;PARTSTAT=NEEDS-ACTION:mailto:me@example.com\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:alex@example.com\r\nSEQUENCE:2\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    const OUTLOOK: &str = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:outlook-456\r\nSUMMARY:Quarterly planning that is\r\n folded across lines\r\nDTSTART:20261001T140000Z\r\nDTEND:20261001T150000Z\r\nORGANIZER:MAILTO:boss@example.com\r\nATTENDEE;PARTSTAT=TENTATIVE:MAILTO:ME@EXAMPLE.COM\r\nSEQUENCE:0\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn parses_google_invite() {
        let invite = parse_calendar(GOOGLE, "me@example.com").unwrap();
        assert_eq!(invite.uid, "google-123");
        assert_eq!(invite.starts_at.to_rfc3339(), "2026-09-08T14:00:00+00:00");
        assert_eq!(invite.location.as_deref(), Some("Zoom"));
        assert_eq!(invite.my_status, Some(PartStat::NeedsAction));
        assert_eq!(invite.sequence, 2);
    }

    #[test]
    fn parses_outlook_invite_and_builds_reply() {
        let invite = parse_calendar(OUTLOOK, "me@example.com").unwrap();
        assert_eq!(
            invite.summary,
            "Quarterly planning that isfolded across lines"
        );
        assert_eq!(invite.my_status, Some(PartStat::Tentative));
        let reply = build_reply_ics(&invite, "me@example.com", PartStat::Accepted);
        assert!(reply.contains("METHOD:REPLY\r\n"));
        assert!(reply.contains("ATTENDEE;PARTSTAT=ACCEPTED:mailto:me@example.com"));
    }
}
