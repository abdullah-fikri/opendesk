//! Desk IDs: nine digits without a leading zero, displayed as "123 456 789".

/// Number of digits in a desk ID.
pub const LENGTH: usize = 9;

/// Keeps only the digits of `raw` ("123 456 789" -> "123456789").
pub fn clean(raw: &str) -> String {
    raw.chars().filter(|c| c.is_ascii_digit()).collect()
}

/// Display form "123 456 789"; shorter input comes back as bare digits.
pub fn format(raw: &str) -> String {
    let digits = clean(raw);
    if digits.len() >= LENGTH { format!("{} {} {}", &digits[0..3], &digits[3..6], &digits[6..9]) } else { digits }
}

/// True for a well-formed clean desk ID.
pub fn is_valid(clean_id: &str) -> bool {
    clean_id.len() == LENGTH && !clean_id.starts_with('0') && clean_id.bytes().all(|b| b.is_ascii_digit())
}

/// Maps a 64-bit device fingerprint to a desk ID in 100 000 000..=999 999 999.
pub fn from_fingerprint(hash: u64) -> String {
    (100_000_000u64 + hash % 900_000_000u64).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_format_and_validate() {
        assert_eq!(clean("123 456-789"), "123456789");
        assert_eq!(format("123456789"), "123 456 789");
        assert_eq!(format("12"), "12");
        assert!(is_valid("123456789") && !is_valid("012345678") && !is_valid("12345678"));
        assert!(is_valid(&from_fingerprint(u64::MAX)) && is_valid(&from_fingerprint(0)));
    }
}
