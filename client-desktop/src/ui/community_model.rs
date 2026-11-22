//! Communities on the window (admin edition, `docs/features/communities.md` at the repository root): keeps the
//! relay's latest `community-list` and host list, and derives everything the dashboard's community page, the
//! "Add to Group" sheet and Settings › Communities show. Selections live in window properties the UI sets; after
//! each change the UI calls `community_view_changed` / `manage_changed`, which run [`refresh`]. A selection that
//! disappeared (another administrator deleted it) falls back here. UI thread only.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use slint::{ComponentHandle, ModelRc, SharedString, VecModel, Weak};

use super::{CommunityItem, GroupItem, GroupPick, HostItem, MainWindow, MemberPick, dashboard_model};
use crate::domain::desk_id;
use crate::domain::list_search::Query;
use crate::protocol::CommunityEntry;
use crate::protocol::message::GroupEntry;

/// The host-list tab the community page falls back to when its community is gone.
const HOSTS_TAB: i32 = 1;

struct State {
    window: Weak<MainWindow>,
    communities: Vec<CommunityEntry>,
    hosts: Vec<HostItem>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Starts empty. Call once, on the UI thread, before the window runs.
pub fn install(window: &MainWindow) {
    STATE.with(|s| {
        *s.borrow_mut() = Some(State { window: window.as_weak(), communities: Vec::new(), hosts: Vec::new() })
    });
}

/// The relay sent its communities.
pub fn set_communities(communities: Vec<CommunityEntry>) {
    update(|s| s.communities = communities);
}

/// The relay sent its host list (names and presence for counts and the member editor).
pub fn set_hosts(hosts: Vec<HostItem>) {
    update(|s| s.hosts = hosts);
}

/// A selection or the member search changed.
pub fn refresh() {
    update(|_| {});
}

fn update(f: impl FnOnce(&mut State)) {
    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            f(state);
            show(state);
        }
    });
}

fn model<T: Clone + 'static>(rows: Vec<T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(rows)))
}

/// Presence of every listed host by clean ID.
fn presence(hosts: &[HostItem]) -> HashMap<String, bool> {
    hosts.iter().map(|h| (desk_id::clean(&h.desk_id), h.online)).collect()
}

/// (online, total) among `members`, counting only devices the host list knows (each once).
fn counts<'a>(members: impl IntoIterator<Item = &'a String>, online: &HashMap<String, bool>) -> (i32, i32) {
    let known: HashSet<&String> = members.into_iter().filter(|m| online.contains_key(*m)).collect();
    let on = known.iter().filter(|m| online[**m]).count();
    (on as i32, known.len() as i32)
}

/// Every device of any group of `c`, each once.
fn community_members(c: &CommunityEntry) -> HashSet<String> {
    c.groups.iter().flat_map(|g| g.members.iter().cloned()).collect()
}

fn community_item(c: &CommunityEntry, online: &HashMap<String, bool>) -> CommunityItem {
    let members = community_members(c);
    let (on, total) = counts(&members, online);
    CommunityItem {
        id: c.id.as_str().into(),
        name: c.name.as_str().into(),
        icon: c.icon.as_str().into(),
        color: c.color.as_str().into(),
        groups: c.groups.len() as i32,
        online: on,
        total,
    }
}

fn group_item(g: &GroupEntry, online: &HashMap<String, bool>) -> GroupItem {
    let (on, total) = counts(&g.members, online);
    GroupItem { id: g.id.as_str().into(), name: g.name.as_str().into(), online: on, total }
}

/// The group column: "All Members" (the community's devices, ID "") and then every group.
fn group_column(c: &CommunityEntry, online: &HashMap<String, bool>) -> Vec<GroupItem> {
    let (on, total) = counts(&community_members(c), online);
    let all = GroupItem { id: SharedString::new(), name: "All Members".into(), online: on, total };
    std::iter::once(all).chain(c.groups.iter().map(|g| group_item(g, online))).collect()
}

/// Every group, under its community, with whether `device` (clean ID) is a member.
fn group_picks(communities: &[CommunityEntry], device: &str) -> Vec<GroupPick> {
    communities
        .iter()
        .flat_map(|c| {
            c.groups.iter().enumerate().map(move |(i, g)| GroupPick {
                id: g.id.as_str().into(),
                name: g.name.as_str().into(),
                community: c.name.as_str().into(),
                icon: c.icon.as_str().into(),
                color: c.color.as_str().into(),
                first: i == 0,
                member: g.members.iter().any(|m| m == device),
            })
        })
        .collect()
}

/// The member editor: every host matching `query`, members first, then the host list's order (online first).
fn member_picks(hosts: &[HostItem], members: &[String], query: &Query) -> Vec<MemberPick> {
    let members: HashSet<&str> = members.iter().map(String::as_str).collect();
    let mut picks: Vec<MemberPick> = hosts
        .iter()
        .filter(|h| query.matches(&h.desk_id, &[&h.alias, &h.device_name, &h.platform]))
        .map(|h| MemberPick {
            desk_id: h.desk_id.clone(),
            name: if h.alias.is_empty() { h.device_name.clone() } else { h.alias.clone() },
            platform: h.platform.clone(),
            online: h.online,
            member: members.contains(desk_id::clean(&h.desk_id).as_str()),
        })
        .collect();
    // Stable: keeps the online-first order inside both parts.
    picks.sort_by_key(|p| !p.member);
    picks
}

fn find<'a>(communities: &'a [CommunityEntry], id: &str) -> Option<&'a CommunityEntry> {
    if id.is_empty() { None } else { communities.iter().find(|c| c.id == id) }
}

fn find_group<'a>(c: Option<&'a CommunityEntry>, id: &str) -> Option<&'a GroupEntry> {
    c.and_then(|c| c.groups.iter().find(|g| g.id == id))
}

fn show(s: &State) {
    let Some(w) = s.window.upgrade() else { return };
    let online = presence(&s.hosts);
    w.set_communities(model(s.communities.iter().map(|c| community_item(c, &online)).collect()));

    // Dashboard community page.
    let open = find(&s.communities, &w.get_active_community());
    if open.is_none() && !w.get_active_community().is_empty() {
        w.set_active_community(SharedString::new());
        w.set_active_group(SharedString::new());
        if w.get_active_dashboard_tab() == dashboard_model::COMMUNITY_TAB {
            w.set_active_dashboard_tab(HOSTS_TAB);
        }
    }
    let group = find_group(open, &w.get_active_group());
    if group.is_none() && !w.get_active_group().is_empty() {
        w.set_active_group(SharedString::new());
    }
    w.set_active_community_name(open.map(|c| c.name.as_str()).unwrap_or_default().into());
    w.set_active_community_icon(open.map(|c| c.icon.as_str()).unwrap_or_default().into());
    w.set_active_community_color(open.map(|c| c.color.as_str()).unwrap_or_default().into());
    w.set_active_group_name(group.map(|g| g.name.as_str()).unwrap_or_default().into());
    w.set_community_groups(model(open.map(|c| group_column(c, &online)).unwrap_or_default()));
    dashboard_model::set_members(open.map(|c| match group {
        Some(g) => g.members.iter().cloned().collect(),
        None => community_members(c),
    }));

    // Settings › Communities.
    let managed = find(&s.communities, &w.get_manage_community());
    if managed.is_none() {
        w.set_manage_community(SharedString::new());
    }
    let managed_group = find_group(managed, &w.get_manage_group());
    if managed_group.is_none() {
        w.set_manage_group(SharedString::new());
    }
    w.set_manage_community_name(managed.map(|c| c.name.as_str()).unwrap_or_default().into());
    w.set_manage_community_icon(managed.map(|c| c.icon.as_str()).unwrap_or_default().into());
    w.set_manage_community_color(managed.map(|c| c.color.as_str()).unwrap_or_default().into());
    w.set_manage_group_name(managed_group.map(|g| g.name.as_str()).unwrap_or_default().into());
    w.set_manage_groups(model(
        managed.map(|c| c.groups.iter().map(|g| group_item(g, &online)).collect()).unwrap_or_default(),
    ));
    let members = managed_group.map(|g| g.members.as_slice()).unwrap_or_default();
    w.set_member_count(counts(members, &online).1);
    let picks = if managed_group.is_some() {
        member_picks(&s.hosts, members, &Query::parse(&w.get_member_search()))
    } else {
        Vec::new()
    };
    w.set_member_picks(model(picks));

    // "Add to Group" sheet.
    let device = desk_id::clean(&w.get_group_picker_device());
    w.set_group_picks(model(if device.is_empty() { Vec::new() } else { group_picks(&s.communities, &device) }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(id: &str, online: bool) -> HostItem {
        HostItem {
            desk_id: desk_id::format(id).into(),
            device_name: format!("PC {id}").into(),
            online,
            ..Default::default()
        }
    }

    fn community() -> CommunityEntry {
        CommunityEntry {
            id: "c".into(),
            name: "Shops".into(),
            groups: vec![
                GroupEntry {
                    id: "g1".into(),
                    name: "Front".into(),
                    members: vec!["111111111".into(), "222222222".into()],
                },
                // 999999999 was deleted from the registry by another administrator a moment ago.
                GroupEntry {
                    id: "g2".into(),
                    name: "Back".into(),
                    members: vec!["222222222".into(), "999999999".into()],
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn counts_include_each_known_device_once() {
        let hosts = [host("111111111", true), host("222222222", false), host("333333333", true)];
        let online = presence(&hosts);
        let c = community();
        let item = community_item(&c, &online);
        assert_eq!((item.online, item.total, item.groups), (1, 2, 2));
        let column = group_column(&c, &online);
        assert_eq!(column[0].name, "All Members");
        assert_eq!((column[1].online, column[1].total), (1, 2));
        assert_eq!((column[2].online, column[2].total), (0, 1), "unknown member not counted");
    }

    #[test]
    fn group_picks_and_member_editor() {
        let picks = group_picks(&[community()], "222222222");
        assert_eq!(picks.len(), 2);
        assert!(picks[0].first && !picks[1].first && picks.iter().all(|p| p.member));
        assert!(group_picks(&[community()], "333333333").iter().all(|p| !p.member));

        let hosts = [host("333333333", true), host("111111111", true), host("222222222", false)];
        let picks = member_picks(&hosts, &community().groups[0].members, &Query::parse(""));
        let order: Vec<_> = picks.iter().map(|p| (desk_id::clean(&p.desk_id), p.member)).collect();
        assert_eq!(
            order,
            [("111111111".into(), true), ("222222222".into(), true), ("333333333".into(), false)],
            "members first, host order kept"
        );
        let found = member_picks(&hosts, &[], &Query::parse("333 333"));
        assert_eq!(found.len(), 1);
    }
}
