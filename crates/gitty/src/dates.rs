//! Commit dates as compact relative text (now, 12m, 5h, 3d) turning absolute after a week, and
//! author identity helpers (initials, a stable colour slot per email).

const MINUTE: i64 = 60;
const HOUR: i64 = 3600;
const DAY: i64 = 86_400;
const WEEK: i64 = 7 * DAY;
/// Keeps civil-date math far from overflow; ±300k years is plenty.
const CLAMP: i64 = 10_000_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DateMode {
    #[default]
    Relative,
    Absolute,
    Both,
}

impl DateMode {
    pub fn next(self) -> DateMode {
        match self {
            DateMode::Relative => DateMode::Absolute,
            DateMode::Absolute => DateMode::Both,
            DateMode::Both => DateMode::Relative,
        }
    }
}

fn relative(age: i64) -> Option<String> {
    Some(match age {
        a if a < MINUTE => "now".to_string(),
        a if a < HOUR => format!("{}m", a / MINUTE),
        a if a < DAY => format!("{}h", a / HOUR),
        a if a < WEEK => format!("{}d", a / DAY),
        _ => return None,
    })
}

/// (year, month 1-12, day 1-31) of a day count since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

fn absolute(t: i64, offset_secs: i32, now: i64) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let local = t.clamp(-CLAMP, CLAMP) + i64::from(offset_secs);
    let (y, m, d) = civil_from_days(local.div_euclid(DAY));
    let (now_y, _, _) = civil_from_days(now.clamp(-CLAMP, CLAMP).div_euclid(DAY));
    let mon = MONTHS[(m - 1) as usize];
    if y == now_y { format!("{mon} {d}") } else { format!("{mon} {d} {y}") }
}

/// Formats commit time `t` (with its UTC offset) as seen at `now`.
pub fn format_date(t: i64, offset_secs: i32, now: i64, mode: DateMode) -> String {
    let age = now.saturating_sub(t).max(0);
    match mode {
        DateMode::Relative => relative(age).unwrap_or_else(|| absolute(t, offset_secs, now)),
        DateMode::Absolute => absolute(t, offset_secs, now),
        DateMode::Both => match relative(age) {
            Some(r) => format!("{r} · {}", absolute(t, offset_secs, now)),
            None => absolute(t, offset_secs, now),
        },
    }
}

/// The next time at which `t`'s relative text changes, or None once it is absolute.
pub fn next_threshold(t: i64, now: i64) -> Option<i64> {
    let age = now.saturating_sub(t);
    let unit = match age {
        a if a < 0 => return Some(t.saturating_add(MINUTE)),
        a if a < HOUR => MINUTE,
        a if a < DAY => HOUR,
        a if a < WEEK => DAY,
        _ => return None,
    };
    Some(t.saturating_add(unit * (age / unit + 1)))
}

/// First letters of the first and last words, uppercased: "Vedang Patel" → "VP".
pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    let first = |w: &str| w.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default();
    match words.as_slice() {
        [] => "?".to_string(),
        [one] => first(one),
        [a, .., b] => first(a) + &first(b),
    }
}

/// A stable slot 0..8 for an author, from an FNV-1a hash of the lowercased email.
pub fn identity_hue(email: &str) -> u8 {
    let mut h: u32 = 0x811c_9dc5;
    for b in email.bytes() {
        h ^= u32::from(b.to_ascii_lowercase());
        h = h.wrapping_mul(0x0100_0193);
    }
    (h % 8) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-10-02 00:00:00 UTC
    const NOW: i64 = 1_790_899_200;

    #[test]
    fn relative_thresholds() {
        let r = |ago: i64| format_date(NOW - ago, 0, NOW, DateMode::Relative);
        assert_eq!(r(0), "now");
        assert_eq!(r(59), "now");
        assert_eq!(r(60), "1m");
        assert_eq!(r(3599), "59m");
        assert_eq!(r(3600), "1h");
        assert_eq!(r(86_399), "23h");
        assert_eq!(r(86_400), "1d");
        assert_eq!(r(7 * 86_400 - 1), "6d");
        assert_eq!(r(7 * 86_400), "Sep 25");
    }

    #[test]
    fn future_is_now() {
        assert_eq!(format_date(NOW + 5000, 0, NOW, DateMode::Relative), "now");
    }

    #[test]
    fn absolute_same_and_other_year() {
        let sep14 = NOW - 18 * 86_400;
        assert_eq!(format_date(sep14, 0, NOW, DateMode::Absolute), "Sep 14");
        let y2023 = 1_694_649_600; // 2023-09-14
        assert_eq!(format_date(y2023, 0, NOW, DateMode::Relative), "Sep 14 2023");
    }

    #[test]
    fn offset_applied() {
        let t = 1_694_649_600 + 23 * 3600 + 1800; // 2023-09-14 23:30 UTC
        assert_eq!(format_date(t, 3600, NOW, DateMode::Absolute), "Sep 15 2023");
        assert_eq!(format_date(t, -3600, NOW, DateMode::Absolute), "Sep 14 2023");
    }

    #[test]
    fn epoch_zero_and_negative_dont_panic() {
        assert_eq!(format_date(0, 0, NOW, DateMode::Absolute), "Jan 1 1970");
        let _ = format_date(-1_000_000_000_000, 0, NOW, DateMode::Both);
        let _ = format_date(i64::MAX, i32::MAX, NOW, DateMode::Both);
        let _ = format_date(i64::MIN, i32::MIN, NOW, DateMode::Both);
    }

    #[test]
    fn both_mode() {
        assert_eq!(format_date(NOW - 4 * 86_400, 0, NOW, DateMode::Both), "4d · Sep 28");
        assert_eq!(format_date(NOW - 30 * 86_400, 0, NOW, DateMode::Both), "Sep 2");
    }

    #[test]
    fn mode_cycles() {
        assert_eq!(DateMode::Relative.next(), DateMode::Absolute);
        assert_eq!(DateMode::Absolute.next(), DateMode::Both);
        assert_eq!(DateMode::Both.next(), DateMode::Relative);
    }

    #[test]
    fn next_threshold_values() {
        assert_eq!(next_threshold(NOW - 90, NOW), Some(NOW + 30));
        assert_eq!(next_threshold(NOW - 10, NOW), Some(NOW + 50));
        assert_eq!(next_threshold(NOW - 3600 - 5, NOW), Some(NOW + 3595));
        assert_eq!(next_threshold(NOW - 86_400 * 2, NOW), Some(NOW + 86_400));
        assert_eq!(next_threshold(NOW - 86_400 * 8, NOW), None);
        assert_eq!(next_threshold(NOW + 100, NOW), Some(NOW + 160));
    }

    #[test]
    fn initials_cases() {
        assert_eq!(initials("Vedang Patel"), "VP");
        assert_eq!(initials("linus"), "L");
        assert_eq!(initials(""), "?");
        assert_eq!(initials("  a  b  c "), "AC");
        assert_eq!(initials("élodie durand"), "ÉD");
    }

    #[test]
    fn hue_stable_and_case_insensitive() {
        assert_eq!(identity_hue("A@x.org"), identity_hue("a@X.ORG"));
        assert!(identity_hue("someone@example.com") < 8);
        let distinct: std::collections::HashSet<u8> =
            (0..64).map(|i| identity_hue(&format!("user{i}@example.com"))).collect();
        assert!(distinct.len() >= 6);
    }
}
