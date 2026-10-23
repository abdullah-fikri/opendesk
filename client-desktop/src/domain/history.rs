//! Recently contacted devices, shown on the dashboard.

use serde::{Deserialize, Serialize};

use super::desk_id;

/// One remembered device. `last_connected` is an ISO date (`YYYY-MM-DD`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct HistoryRecord {
    pub desk_id: String,
    pub alias: String,
    pub last_connected: String,
}

/// Records a connection to `clean_id` on `today`: an existing entry is updated, a new one goes first.
pub fn touch(list: &mut Vec<HistoryRecord>, clean_id: &str, today: &str) {
    if clean_id.is_empty() {
        return;
    }
    let formatted = desk_id::format(clean_id);
    match list.iter_mut().find(|r| r.desk_id == formatted) {
        Some(item) => item.last_connected = today.to_string(),
        None => list
            .insert(0, HistoryRecord { desk_id: formatted, alias: String::new(), last_connected: today.to_string() }),
    }
}

/// Sets the alias of the entry shown as `desk_id`.
pub fn rename(list: &mut [HistoryRecord], desk_id: &str, alias: &str) {
    if let Some(item) = list.iter_mut().find(|r| r.desk_id == desk_id) {
        item.alias = alias.to_string();
    }
}

/// Removes the entry shown as `desk_id`.
pub fn remove(list: &mut Vec<HistoryRecord>, desk_id: &str) {
    list.retain(|r| r.desk_id != desk_id);
}

/// Alias of `clean_id`, or an empty string.
pub fn alias_of(list: &[HistoryRecord], clean_id: &str) -> String {
    let formatted = desk_id::format(clean_id);
    list.iter().find(|r| r.desk_id == formatted).map(|r| r.alias.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_rename_remove() {
        let mut list = Vec::new();
        touch(&mut list, "123456789", "2026-01-01");
        touch(&mut list, "987654321", "2026-01-02");
        touch(&mut list, "123456789", "2026-01-03");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].desk_id, "987 654 321");
        assert_eq!(list[1].last_connected, "2026-01-03");
        rename(&mut list, "123 456 789", "Office");
        assert_eq!(alias_of(&list, "123456789"), "Office");
        remove(&mut list, "987 654 321");
        assert_eq!(list.len(), 1);
    }
}
