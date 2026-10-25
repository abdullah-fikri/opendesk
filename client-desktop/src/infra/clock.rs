//! Local date and time formatting through the OS (no time-zone database needed): Win32 on Windows,
//! `localtime_r` elsewhere.

#[cfg(windows)]
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct SystemTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLocalTime(time: *mut SystemTime);
    fn SystemTimeToTzSpecificLocalTime(
        tz: *const std::ffi::c_void,
        utc: *const SystemTime,
        local: *mut SystemTime,
    ) -> i32;
}

/// Today's local date as `YYYY-MM-DD`.
#[cfg(windows)]
pub fn today() -> String {
    let mut t = SystemTime::default();
    // SAFETY: GetLocalTime only writes the struct it is given.
    unsafe { GetLocalTime(&mut t) };
    format!("{:04}-{:02}-{:02}", t.year, t.month, t.day)
}

/// Formats an RFC 3339 UTC timestamp (`2026-01-31T13:45:00Z`) as local `HH:MM · DD/MM`; unparsable input is
/// returned unchanged.
#[cfg(windows)]
pub fn local_short(rfc3339: &str) -> String {
    let Some(utc) = parse_utc(rfc3339) else { return rfc3339.to_string() };
    let mut local = SystemTime::default();
    // SAFETY: null selects the active time zone; both structs are valid for the call.
    if unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) } == 0 {
        return rfc3339.to_string();
    }
    format!("{:02}:{:02} · {:02}/{:02}", local.hour, local.minute, local.day, local.month)
}

#[cfg(windows)]
fn parse_utc(s: &str) -> Option<SystemTime> {
    let (year, month, day, hour, minute, second) = parse_fields(s)?;
    Some(SystemTime { year, month, day, hour, minute, second, ..SystemTime::default() })
}

/// (year, month, day, hour, minute, second) of `YYYY-MM-DDTHH:MM:SS...`.
fn parse_fields(s: &str) -> Option<(u16, u16, u16, u16, u16, u16)> {
    let num = |range: std::ops::Range<usize>| s.get(range)?.parse::<u16>().ok();
    if s.len() < 19 || s.as_bytes()[10] != b'T' {
        return None;
    }
    Some((num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?))
}

/// Local broken-down time of a Unix timestamp.
#[cfg(not(windows))]
fn local_tm(secs: i64) -> Option<libc::tm> {
    let t = secs as libc::time_t;
    let mut tm = std::mem::MaybeUninit::<libc::tm>::zeroed();
    // SAFETY: localtime_r writes only the struct it is given and is thread-safe.
    let ok = !unsafe { libc::localtime_r(&t, tm.as_mut_ptr()) }.is_null();
    // SAFETY: zero-initialised and, on success, filled in by localtime_r.
    ok.then(|| unsafe { tm.assume_init() })
}

/// Today's local date as `YYYY-MM-DD`.
#[cfg(not(windows))]
pub fn today() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    match local_tm(now) {
        Some(tm) => format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday),
        None => "1970-01-01".into(),
    }
}

/// Formats an RFC 3339 UTC timestamp (`2026-01-31T13:45:00Z`) as local `HH:MM · DD/MM`; unparsable input is
/// returned unchanged.
#[cfg(not(windows))]
pub fn local_short(rfc3339: &str) -> String {
    let Some(secs) = parse_fields(rfc3339).map(unix_seconds) else { return rfc3339.to_string() };
    match local_tm(secs) {
        Some(tm) => format!("{:02}:{:02} · {:02}/{:02}", tm.tm_hour, tm.tm_min, tm.tm_mday, tm.tm_mon + 1),
        None => rfc3339.to_string(),
    }
}

/// Seconds since the Unix epoch of a UTC date and time (Howard Hinnant's days_from_civil).
#[cfg(any(not(windows), test))]
fn unix_seconds((y, mo, d, h, mi, s): (u16, u16, u16, u16, u16, u16)) -> i64 {
    let (y, mo) = (y as i64 - i64::from(mo <= 2), mo as i64);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d as i64 - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    days * 86_400 + h as i64 * 3600 + mi as i64 * 60 + s as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_parse_and_format() {
        assert_eq!(today().len(), 10);
        assert!(local_short("2026-01-31T13:45:00Z").contains('·'));
        assert_eq!(local_short("Online"), "Online");
    }

    #[test]
    fn utc_fields_convert_to_unix_time() {
        assert_eq!(unix_seconds((1970, 1, 1, 0, 0, 0)), 0);
        assert_eq!(unix_seconds((2000, 3, 1, 0, 0, 0)), 951_868_800);
        assert_eq!(unix_seconds((2024, 2, 29, 23, 59, 0)), 1_709_251_140);
    }
}
