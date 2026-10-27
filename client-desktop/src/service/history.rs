//! Connection history use cases: remember, rename and forget devices, persisted on every change.

use parking_lot::Mutex;

use crate::domain::history::{self, HistoryRecord};
use crate::infra::{clock, storage};

/// The connection history, loaded once and saved after every change.
pub struct History {
    records: Mutex<Vec<HistoryRecord>>,
}

impl History {
    /// Loads the stored history.
    pub fn load() -> Self {
        Self { records: Mutex::new(storage::load_history()) }
    }

    /// Current records, most recent first.
    pub fn records(&self) -> Vec<HistoryRecord> {
        self.records.lock().clone()
    }

    fn update(&self, change: impl FnOnce(&mut Vec<HistoryRecord>)) -> Vec<HistoryRecord> {
        let mut records = self.records.lock();
        change(&mut records);
        storage::save_history(&records);
        records.clone()
    }

    /// Records a connection to `clean_id` today and returns the new list.
    pub fn touch(&self, clean_id: &str) -> Vec<HistoryRecord> {
        let today = clock::today();
        self.update(|r| history::touch(r, clean_id, &today))
    }

    /// Renames the entry shown as `desk_id` and returns the new list.
    pub fn rename(&self, desk_id: &str, alias: &str) -> Vec<HistoryRecord> {
        self.update(|r| history::rename(r, desk_id, alias))
    }

    /// Removes the entry shown as `desk_id` and returns the new list.
    pub fn remove(&self, desk_id: &str) -> Vec<HistoryRecord> {
        self.update(|r| history::remove(r, desk_id))
    }

    /// Alias of `clean_id`, or an empty string.
    pub fn alias_of(&self, clean_id: &str) -> String {
        history::alias_of(&self.records.lock(), clean_id)
    }
}
