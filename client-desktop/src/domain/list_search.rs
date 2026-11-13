//! Dashboard list search and keyboard selection: which rows match the search box, and which row an arrow key
//! moves the selection to.

/// What the user typed in a list's search box, split into lower-cased words that must all match.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    words: Vec<String>,
}

impl Query {
    /// Splits `text` on whitespace. Case is ignored when matching.
    pub fn parse(text: &str) -> Self {
        Self { words: text.split_whitespace().map(str::to_lowercase).collect() }
    }

    /// True when nothing but whitespace was typed: every row matches.
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// True when every word occurs in `desk_id` or in one of `fields`. The desk ID is compared without its
    /// spaces, so "418541" finds "418 541 168".
    pub fn matches(&self, desk_id: &str, fields: &[&str]) -> bool {
        let compact_id: String = desk_id.chars().filter(|c| !c.is_whitespace()).collect();
        let fields: Vec<String> = fields.iter().map(|f| f.to_lowercase()).collect();
        self.words
            .iter()
            .all(|word| compact_id.contains(word.as_str()) || fields.iter().any(|f| f.contains(word.as_str())))
    }
}

/// Row to select in a list of `len` rows after moving `delta` rows from `current` (the selected row, if any is
/// shown). Without a selection, moving down selects the first row and moving up the last. Stops at either end;
/// `None` when the list is empty.
pub fn step(len: usize, current: Option<usize>, delta: i32) -> Option<usize> {
    let last = len.checked_sub(1)?;
    Some(match current {
        None if delta >= 0 => 0,
        None => last,
        Some(i) => i.saturating_add_signed(delta as isize).min(last),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_matches(query: &str) -> bool {
        // desk ID, then alias, device name, platform
        Query::parse(query).matches("418 541 168", &["Front desk", "RECEPTION-PC", "Windows 11"])
    }

    #[test]
    fn empty_query_matches_everything() {
        assert!(Query::parse("").is_empty());
        assert!(Query::parse("   ").is_empty());
        assert!(Query::parse("  ").matches("", &[]));
        assert!(host_matches(""));
    }

    #[test]
    fn desk_id_matches_with_or_without_spaces() {
        assert!(host_matches("418541"));
        assert!(host_matches("418 541 168"));
        assert!(host_matches("418541168"));
        assert!(host_matches("1 168"));
        assert!(Query::parse("418541").matches("418541168", &[]));
        assert!(!host_matches("419"));
    }

    #[test]
    fn every_word_must_match_somewhere_ignoring_case() {
        assert!(host_matches("front"));
        assert!(host_matches("reception windows"));
        assert!(host_matches("DESK win 418"));
        assert!(!host_matches("reception linux"));
        assert!(!host_matches("front back"));
    }

    #[test]
    fn empty_alias_falls_back_to_other_fields() {
        let q = Query::parse("reception");
        assert!(q.matches("418 541 168", &["", "RECEPTION-PC", "Windows 11"]));
        assert!(!Query::parse("desk").matches("418 541 168", &["", "RECEPTION-PC", "Windows 11"]));
    }

    #[test]
    fn step_moves_within_bounds() {
        assert_eq!(step(0, None, 1), None);
        assert_eq!(step(3, None, 1), Some(0));
        assert_eq!(step(3, None, -1), Some(2));
        assert_eq!(step(3, Some(0), 1), Some(1));
        assert_eq!(step(3, Some(2), 1), Some(2));
        assert_eq!(step(3, Some(0), -1), Some(0));
        assert_eq!(step(3, Some(1), -1), Some(0));
    }
}
