//! Local date and time formatting through Win32 (no time-zone database needed).

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
pub fn today() -> String {
    let mut t = SystemTime::default();
    // SAFETY: GetLocalTime only writes the struct it is given.
    unsafe { GetLocalTime(&mut t) };
    format!("{:04}-{:02}-{:02}", t.year, t.month, t.day)
}

/// Formats an RFC 3339 UTC timestamp (`2026-01-31T13:45:00Z`) as local `HH:MM · DD/MM`; unparsable input is
/// returned unchanged.
pub fn local_short(rfc3339: &str) -> String {
    let Some(utc) = parse_utc(rfc3339) else { return rfc3339.to_string() };
    let mut local = SystemTime::default();
    // SAFETY: null selects the active time zone; both structs are valid for the call.
    if unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) } == 0 {
        return rfc3339.to_string();
    }
    format!("{:02}:{:02} · {:02}/{:02}", local.hour, local.minute, local.day, local.month)
}

fn parse_utc(s: &str) -> Option<SystemTime> {
    let num = |range: std::ops::Range<usize>| s.get(range)?.parse::<u16>().ok();
    if s.len() < 19 || s.as_bytes()[10] != b'T' {
        return None;
    }
    Some(SystemTime {
        year: num(0..4)?,
        month: num(5..7)?,
        day: num(8..10)?,
        hour: num(11..13)?,
        minute: num(14..16)?,
        second: num(17..19)?,
        ..SystemTime::default()
    })
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
}
