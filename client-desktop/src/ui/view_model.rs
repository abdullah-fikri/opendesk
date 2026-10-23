//! Maps domain and service values to the Slint structs the window displays.

use std::rc::Rc;

use slint::{ModelRc, VecModel};

use super::{HistoryItem, HostItem, MonitorItem};
use crate::domain::history::HistoryRecord;
use crate::domain::monitor::Monitor;
use crate::infra::clock;
use crate::service::ui_port::RemoteHost;

/// History rows; today's entries read "Today".
pub fn history_items(records: &[HistoryRecord]) -> ModelRc<HistoryItem> {
    let today = clock::today();
    let items: Vec<HistoryItem> = records
        .iter()
        .map(|r| HistoryItem {
            desk_id: r.desk_id.clone().into(),
            alias: r.alias.clone().into(),
            last_connected: if r.last_connected == today { "Today".into() } else { r.last_connected.clone().into() },
        })
        .collect();
    ModelRc::from(Rc::new(VecModel::from(items)))
}

/// Administrator host list with local last-seen times.
pub fn host_items(hosts: Vec<RemoteHost>) -> ModelRc<HostItem> {
    let items: Vec<HostItem> = hosts
        .into_iter()
        .map(|h| HostItem {
            desk_id: h.desk_id.into(),
            device_name: h.device_name.into(),
            platform: h.platform.into(),
            online: h.online,
            last_seen: clock::local_short(&h.last_seen).into(),
        })
        .collect();
    ModelRc::from(Rc::new(VecModel::from(items)))
}

/// Monitor picker entries, labeled 1, 2, ...
pub fn monitor_items(monitors: Vec<Monitor>) -> ModelRc<MonitorItem> {
    let items: Vec<MonitorItem> = monitors
        .into_iter()
        .map(|m| MonitorItem {
            index: m.index,
            label: (m.index + 1).to_string().into(),
            width: m.width,
            height: m.height,
            primary: m.primary,
        })
        .collect();
    ModelRc::from(Rc::new(VecModel::from(items)))
}
