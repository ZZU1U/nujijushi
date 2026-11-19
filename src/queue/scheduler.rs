//! Per-channel scheduling: active hours in the channel timezone, intervals, jitter.

use chrono::{DateTime, Duration, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;

use crate::error::{Error, Result};
use crate::model::Channel;

/// `HH:MM`, tolerating `H:MM` and `HH:MM:SS`.
pub fn parse_hhmm(raw: &str) -> Result<NaiveTime> {
    let trimmed = raw.trim();
    for format in ["%H:%M", "%H:%M:%S"] {
        if let Ok(time) = NaiveTime::parse_from_str(trimmed, format) {
            return Ok(time);
        }
    }
    Err(Error::invalid(format!(
        "expected a HH:MM time, got {raw:?}"
    )))
}

pub fn channel_tz(channel: &Channel) -> Result<Tz> {
    channel
        .tz
        .trim()
        .parse::<Tz>()
        .map_err(|_| Error::invalid(format!("unknown timezone: {}", channel.tz)))
}

/// Is `now` inside the channel's posting window?
pub fn is_active(channel: &Channel, now: DateTime<Utc>) -> bool {
    let Ok(tz) = channel_tz(channel) else {
        return true;
    };
    let (Ok(from), Ok(to)) = (
        parse_hhmm(&channel.active_from),
        parse_hhmm(&channel.active_to),
    ) else {
        return true;
    };
    let local = now.with_timezone(&tz).time();
    if from <= to {
        local >= from && local < to
    } else {
        // window wraps midnight, e.g. 22:00 - 02:00
        local >= from || local < to
    }
}

/// First instant at or after `after` that is inside the active window.
pub fn next_active_start(channel: &Channel, after: DateTime<Utc>) -> DateTime<Utc> {
    if is_active(channel, after) {
        return after;
    }
    let Ok(tz) = channel_tz(channel) else {
        return after;
    };
    let Ok(from) = parse_hhmm(&channel.active_from) else {
        return after;
    };
    let local = after.with_timezone(&tz);
    for day_offset in 0..3 {
        let date = local.date_naive() + Duration::days(day_offset);
        let Some(candidate) = date
            .and_time(from)
            .and_local_timezone(tz)
            .earliest()
            .map(|dt| dt.with_timezone(&Utc))
        else {
            continue;
        };
        if candidate >= after {
            return candidate;
        }
    }
    after
}

/// When the next slot is due. `jitter` is added to the interval (0 = off).
pub fn next_due(channel: &Channel, now: DateTime<Utc>, jitter_secs: i64) -> DateTime<Utc> {
    let interval = channel.interval_minutes.max(1) * 60;
    let jitter = jitter_secs.clamp(0, channel.jitter_secs.max(0));
    let next = now + Duration::seconds(interval + jitter);
    next_active_start(channel, next)
}

/// Should the scheduler consider this channel right now?
pub fn is_due(channel: &Channel, now: DateTime<Utc>) -> bool {
    if !channel.enabled || channel.paused {
        return false;
    }
    if channel.last_sent_at.is_none() && channel.next_due_at.is_none() {
        // fresh channel: publish on the next tick inside the window
        return is_active(channel, now);
    }
    match channel.next_due_at {
        Some(due) => due <= now,
        None => is_active(channel, now),
    }
}

/// Local midnight of the channel, as an UTC instant (daily cap window).
pub fn local_day_start(channel: &Channel, now: DateTime<Utc>) -> DateTime<Utc> {
    let Ok(tz) = channel_tz(channel) else {
        return now;
    };
    let local = now.with_timezone(&tz);
    local
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|midnight| tz.from_local_datetime(&midnight).earliest())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or(now)
}

/// Daily cap gate (`daily_cap == 0` means unlimited).
pub fn within_daily_cap(channel: &Channel, sent_today: i64) -> bool {
    channel.daily_cap <= 0 || sent_today < channel.daily_cap
}

/// Settings key the `run` loop writes on every pass (RFC3339 timestamp).
///
/// The UI reads it to answer the one question a silent queue raises: is nothing
/// due, or is nothing *running*? `serve` does not schedule, so without this a
/// channel that never fires looks identical to a channel that is simply waiting.
pub const TICK_KEY: &str = "scheduler.tick";

/// A scheduler that has not ticked for this long is considered not running.
///
/// The loop ticks every `--tick` seconds (15 by default): two minutes leaves room
/// for a slow pass and still explains a queue that is not moving.
pub const STALE_SECS: i64 = 120;

/// Age of the stored tick, in seconds. Pure: the caller reads the setting.
pub fn tick_age(raw: Option<&str>, now: DateTime<Utc>) -> Option<i64> {
    let last = DateTime::parse_from_rfc3339(raw?.trim()).ok()?;
    Some((now - last.with_timezone(&Utc)).num_seconds().max(0))
}

/// Everything a channel needs to pass before a slot may fire.
pub fn slot_allowed(channel: &Channel, now: DateTime<Utc>, sent_today: i64) -> bool {
    is_due(channel, now) && is_active(channel, now) && within_daily_cap(channel, sent_today)
}

/// Human readable explanation for the UI/CLI.
pub fn slot_reason(channel: &Channel, now: DateTime<Utc>, sent_today: i64) -> String {
    if !channel.enabled {
        return "disabled".into();
    }
    if channel.paused {
        return "paused".into();
    }
    if !within_daily_cap(channel, sent_today) {
        return format!("daily cap reached ({sent_today}/{})", channel.daily_cap);
    }
    if !is_active(channel, now) {
        return format!(
            "outside active hours ({} - {}, next window {})",
            channel.active_from,
            channel.active_to,
            next_active_start(channel, now).format("%Y-%m-%d %H:%M UTC")
        );
    }
    if !is_due(channel, now)
        && let Some(due) = channel.next_due_at
    {
        return format!("next slot at {}", due.format("%Y-%m-%d %H:%M UTC"));
    }
    "due".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    fn channel(from: &str, to: &str, tz: &str) -> Channel {
        let mut ch = Channel::new("test", "@test");
        ch.active_from = from.into();
        ch.active_to = to.into();
        ch.tz = tz.into();
        ch
    }

    #[test]
    fn parses_times() {
        assert_eq!(
            parse_hhmm("09:30").unwrap(),
            NaiveTime::from_hms_opt(9, 30, 0).unwrap()
        );
        assert_eq!(
            parse_hhmm(" 9:05 ").unwrap(),
            NaiveTime::from_hms_opt(9, 5, 0).unwrap()
        );
        assert!(parse_hhmm("9am").is_err());
    }

    #[test]
    fn active_window() {
        let ch = channel("09:00", "23:00", "UTC");
        assert!(!is_active(&ch, utc(2025, 5, 1, 8, 59)));
        assert!(is_active(&ch, utc(2025, 5, 1, 9, 0)));
        assert!(is_active(&ch, utc(2025, 5, 1, 22, 59)));
        assert!(!is_active(&ch, utc(2025, 5, 1, 23, 0)));
    }

    #[test]
    fn window_wrapping_midnight() {
        let ch = channel("22:00", "02:00", "UTC");
        assert!(is_active(&ch, utc(2025, 5, 1, 23, 0)));
        assert!(is_active(&ch, utc(2025, 5, 1, 1, 30)));
        assert!(!is_active(&ch, utc(2025, 5, 1, 12, 0)));
    }

    #[test]
    fn timezone_shifts_the_window() {
        let ch = channel("09:00", "23:00", "Europe/Berlin");
        // 08:00 UTC = 10:00 in Berlin (summer)
        assert!(is_active(&ch, utc(2025, 7, 1, 8, 0)));
        // 07:00 UTC = 09:00 Berlin
        assert!(is_active(&ch, utc(2025, 7, 1, 7, 0)));
        // 06:00 UTC = 08:00 Berlin
        assert!(!is_active(&ch, utc(2025, 7, 1, 6, 0)));
    }

    #[test]
    fn next_active_start_jumps_to_the_next_morning() {
        let ch = channel("09:00", "23:00", "UTC");
        let after = utc(2025, 5, 1, 23, 30);
        let next = next_active_start(&ch, after);
        assert_eq!(next, utc(2025, 5, 2, 9, 0));
    }

    #[test]
    fn next_due_never_lands_outside_the_window() {
        let mut ch = channel("09:00", "17:00", "UTC");
        ch.interval_minutes = 60;
        let now = utc(2025, 5, 1, 16, 30);
        let due = next_due(&ch, now, 0);
        assert_eq!(due, utc(2025, 5, 2, 9, 0));
    }

    #[test]
    fn jitter_is_bounded_by_the_channel_setting() {
        let mut ch = channel("00:00", "23:59", "UTC");
        ch.interval_minutes = 10;
        ch.jitter_secs = 60;
        let now = utc(2025, 5, 1, 12, 0);
        assert_eq!(next_due(&ch, now, 0), utc(2025, 5, 1, 12, 10));
        assert_eq!(next_due(&ch, now, 600), utc(2025, 5, 1, 12, 11));
        assert_eq!(next_due(&ch, now, -30), utc(2025, 5, 1, 12, 10));
    }

    #[test]
    fn day_start_respects_dst() {
        let ch = channel("09:00", "23:00", "Europe/Berlin");
        // 2025-03-30: Europe/Berlin springs forward at 02:00 -> 03:00 (CET -> CEST)
        let now = utc(2025, 3, 30, 12, 0);
        let start = local_day_start(&ch, now);
        // local midnight is 2025-03-30 00:00 CET = 2025-03-29 23:00 UTC
        assert_eq!(start, utc(2025, 3, 29, 23, 0));
        // and the day after the switch the offset is +2
        let next_day = local_day_start(&ch, utc(2025, 3, 31, 12, 0));
        assert_eq!(next_day, utc(2025, 3, 30, 22, 0));
    }

    #[test]
    fn due_state() {
        let mut ch = channel("09:00", "23:00", "UTC");
        let now = utc(2025, 5, 1, 12, 0);
        // fresh channel inside the window is due immediately
        assert!(is_due(&ch, now));
        ch.next_due_at = Some(utc(2025, 5, 1, 13, 0));
        assert!(!is_due(&ch, now));
        ch.next_due_at = Some(utc(2025, 5, 1, 11, 0));
        assert!(is_due(&ch, now));
        ch.paused = true;
        assert!(!is_due(&ch, now));
        ch.paused = false;
        ch.enabled = false;
        assert!(!is_due(&ch, now));
    }

    #[test]
    fn daily_cap_and_reasons() {
        let mut ch = channel("09:00", "23:00", "UTC");
        let now = utc(2025, 5, 1, 12, 0);
        assert!(within_daily_cap(&ch, 999));
        ch.daily_cap = 3;
        assert!(within_daily_cap(&ch, 2));
        assert!(!within_daily_cap(&ch, 3));
        assert!(slot_allowed(&ch, now, 2));
        assert!(!slot_allowed(&ch, now, 3));
        assert!(slot_reason(&ch, now, 3).contains("daily cap"));
        let outside = utc(2025, 5, 1, 3, 0);
        assert!(slot_reason(&ch, outside, 0).contains("outside active hours"));
        assert_eq!(slot_reason(&ch, now, 0), "due");
    }

    #[test]
    fn tick_age_reads_the_heartbeat() {
        let now = utc(2025, 5, 1, 12, 0);
        assert_eq!(tick_age(Some("2025-05-01T11:58:00+00:00"), now), Some(120));
        assert_eq!(tick_age(Some(" 2025-05-01T12:00:00Z "), now), Some(0));
        // a clock that went backwards must not read as "before the beginning"
        assert_eq!(tick_age(Some("2025-05-01T12:05:00Z"), now), Some(0));
        assert_eq!(tick_age(None, now), None);
        assert_eq!(tick_age(Some("not a timestamp"), now), None);
        // (STALE_SECS is documented next to the constant: 2 minutes)
    }
}
