use chrono::{DateTime, Datelike, Days, Local, LocalResult, NaiveDate, TimeZone, Utc, Weekday};

pub fn send_later_presets(now_local: DateTime<Local>) -> Vec<(&'static str, DateTime<Utc>)> {
    let tomorrow = now_local
        .date_naive()
        .checked_add_days(Days::new(1))
        .expect("tomorrow is a valid date");
    let mut evening = local_time(now_local.date_naive(), 18);
    if evening < now_local {
        evening = local_time(tomorrow, 18);
    }
    let mut monday = now_local.date_naive();
    let days =
        (Weekday::Mon.num_days_from_monday() + 7 - monday.weekday().num_days_from_monday()) % 7;
    monday = monday
        .checked_add_days(Days::new(days.into()))
        .expect("next Monday is a valid date");
    if local_time(monday, 9) < now_local {
        monday = monday
            .checked_add_days(Days::new(7))
            .expect("next Monday is a valid date");
    }

    vec![
        (
            "In 1 hour",
            (now_local + chrono::Duration::hours(1)).to_utc(),
        ),
        ("This evening", evening.to_utc()),
        ("Tomorrow morning", local_time(tomorrow, 9).to_utc()),
        ("Monday morning", local_time(monday, 9).to_utc()),
    ]
}

fn local_time(date: NaiveDate, hour: u32) -> DateTime<Local> {
    match Local.from_local_datetime(&date.and_hms_opt(hour, 0, 0).expect("valid preset time")) {
        LocalResult::Single(time) => time,
        LocalResult::Ambiguous(earlier, _) => earlier,
        LocalResult::None => panic!("preset time does not exist in the local timezone"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_evening_rolls_to_tomorrow_after_six() {
        let now = Local.with_ymd_and_hms(2026, 9, 4, 19, 0, 0).unwrap();
        let presets = send_later_presets(now);
        let evening: DateTime<Local> = presets[1].1.into();
        assert_eq!(evening.date_naive().to_string(), "2026-09-05");
        assert_eq!(evening.format("%H:%M").to_string(), "18:00");
    }

    #[test]
    fn monday_morning_from_saturday() {
        let now = Local.with_ymd_and_hms(2026, 9, 5, 12, 0, 0).unwrap();
        let presets = send_later_presets(now);
        let monday: DateTime<Local> = presets[3].1.into();
        assert_eq!(monday.date_naive().to_string(), "2026-09-07");
        assert_eq!(monday.format("%H:%M").to_string(), "09:00");
    }
}
