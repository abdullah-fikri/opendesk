//! Monitor wall (admin edition, see `docs/features/monitor-wall.md` at the repository root): a read-only grid of
//! many hosts' screens, paged 4, 9 or 16 at a time.
//!
//! The relay keeps the watch list. This service works out which hosts are on the visible page and sends exactly
//! those in `monitor-set`, so other pages cost the hosts, the relay and this machine nothing. One worker thread,
//! alive only while the wall is open, decodes the newest thumbnail of each visible tile (JPEG, already tile-sized
//! by the host), hands the batch to the UI at most ten times a second and rotates pages. Nothing here sends input,
//! clipboard, files or voice, and none of it touches viewer sessions.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use crate::domain::desk_id;
use crate::domain::monitor_wall::{self, Rotation, WallHost};
use crate::domain::settings::MonitorSettings;
use crate::infra::{logger, storage};
use crate::protocol::ClientMessage;
use crate::service::context::{AppContext, Ctx};
use crate::service::ui_port::{RemoteHost, TileState, WallPick, WallTile, WallView};
use crate::service::viewer::decoder;

/// Worker tick: caps tile repaints at 10 per second whatever the hosts send.
const TICK: Duration = Duration::from_millis(100);

/// Monitor-wall state shared by the UI thread, the relay connection and the worker.
pub struct MonitorWall {
    state: Mutex<WallState>,
    /// Newest undecoded thumbnail per visible host; filled by the relay connection, drained by the worker.
    inbox: Mutex<Inbox>,
}

#[derive(Default)]
struct Inbox {
    visible: HashSet<String>,
    latest: HashMap<String, Vec<u8>>,
}

struct WallState {
    open: bool,
    relay_online: bool,
    relay_supported: bool,
    /// The relay's device registry (this device excluded).
    hosts: Vec<WallHost>,
    settings: MonitorSettings,
    page: usize,
    /// Hosts sent in the last `monitor-set` on this connection; `None` until one is sent.
    sent: Option<Vec<String>>,
    states: HashMap<String, TileState>,
    rotation: Rotation,
    worker: bool,
}

impl Default for MonitorWall {
    fn default() -> Self {
        Self {
            state: Mutex::new(WallState {
                open: false,
                relay_online: false,
                relay_supported: false,
                hosts: Vec::new(),
                settings: MonitorSettings::default(),
                page: 0,
                sent: None,
                states: HashMap::new(),
                rotation: Rotation::new(Instant::now()),
                worker: false,
            }),
            inbox: Mutex::default(),
        }
    }
}

impl TileState {
    fn from_wire(state: &str) -> Self {
        match state {
            "live" => TileState::Live,
            "offline" => TileState::Offline,
            "unsupported" => TileState::Unsupported,
            "denied" => TileState::Denied,
            _ => TileState::Waiting,
        }
    }
}

// ---- Opening and closing ----

/// Shows the wall: watches the first page and starts the worker.
pub fn open(ctx: &Ctx) {
    let settings = storage::load_settings().monitor;
    let spawn = {
        let mut st = ctx.wall.state.lock().unwrap();
        if st.open {
            return;
        }
        st.open = true;
        st.settings = settings;
        st.page = 0;
        st.rotation.reset(Instant::now());
        !std::mem::replace(&mut st.worker, true)
    };
    logger::info("monitor", "Monitor wall opened.");
    // A fresh registry; the relay pushes changes to administrators after that.
    ctx.send(ClientMessage::GetHosts);
    refresh(ctx);
    if spawn {
        let ctx = std::sync::Arc::clone(ctx);
        crate::infra::thread::spawn_named("monitor-wall", move || run_worker(&ctx));
    }
}

/// Hides the wall: the relay stops every thumbnail stream it started for this machine.
pub fn close(ctx: &AppContext) {
    {
        let mut st = ctx.wall.state.lock().unwrap();
        if !st.open {
            return;
        }
        st.open = false;
        st.states.clear();
        if st.sent.as_ref().is_some_and(|s| !s.is_empty()) {
            ctx.send(ClientMessage::MonitorSet { targets: Vec::new() });
            st.sent = Some(Vec::new());
        }
    }
    let mut inbox = ctx.wall.inbox.lock().unwrap();
    inbox.visible.clear();
    inbox.latest.clear();
    logger::info("monitor", "Monitor wall closed.");
}

// ---- Relay events ----

/// The relay's device registry changed.
pub fn set_hosts(ctx: &AppContext, hosts: &[RemoteHost]) {
    let hosts = hosts
        .iter()
        .map(|h| WallHost { id: desk_id::clean(&h.desk_id), name: h.display_name().to_string(), online: h.online })
        .collect();
    let open = {
        let mut st = ctx.wall.state.lock().unwrap();
        st.hosts = hosts;
        st.open
    };
    if open {
        refresh(ctx);
    }
}

/// Registered with the relay: watches belong to the connection, so the visible page is sent again.
pub fn relay_registered(ctx: &AppContext, supported: bool) {
    let open = {
        let mut st = ctx.wall.state.lock().unwrap();
        (st.relay_online, st.relay_supported, st.sent) = (true, supported, None);
        st.open
    };
    if open {
        refresh(ctx);
    }
}

/// The relay connection ended; every watch ended with it.
pub fn relay_lost(ctx: &AppContext) {
    let open = {
        let mut st = ctx.wall.state.lock().unwrap();
        (st.relay_online, st.sent) = (false, None);
        st.states.clear();
        st.open
    };
    if open {
        refresh(ctx);
    }
}

/// A `monitor-state` for one tile.
pub fn on_state(ctx: &AppContext, id: &str, state: &str) {
    let shown = {
        let mut st = ctx.wall.state.lock().unwrap();
        let shown = st.open && st.sent.as_ref().is_some_and(|s| s.iter().any(|v| v == id));
        if shown {
            st.states.insert(id.to_string(), TileState::from_wire(state));
        }
        shown
    };
    if shown {
        refresh(ctx);
    }
}

/// A thumbnail from `id`; only the newest per tile is kept until the worker's next tick.
pub fn on_frame(ctx: &AppContext, id: &str, jpeg: &[u8]) {
    let mut inbox = ctx.wall.inbox.lock().unwrap();
    if inbox.visible.contains(id) {
        inbox.latest.insert(id.to_string(), jpeg.to_vec());
    }
}

// ---- User actions ----

/// Previous (-1) or next (+1) page, wrapping; restarts the rotation countdown.
pub fn step_page(ctx: &AppContext, delta: i32) {
    {
        let mut st = ctx.wall.state.lock().unwrap();
        let pages = monitor_wall::page_count(
            monitor_wall::wall_hosts(&st.hosts, &st.settings).len(),
            monitor_wall::per_page(&st.settings),
        );
        st.page = monitor_wall::step_page(st.page, delta, pages);
        st.rotation.reset(Instant::now());
    }
    refresh(ctx);
}

/// 2, 3 or 4 tiles per row.
pub fn set_grid(ctx: &AppContext, grid: i32) {
    update_settings(ctx, |s| s.grid = grid.clamp(2, 4) as u8);
}

/// Automatic page rotation on or off, every `secs` seconds.
pub fn set_rotation(ctx: &AppContext, on: bool, secs: i32) {
    update_settings(ctx, |s| {
        s.rotate = on;
        s.rotate_secs = (secs.max(0) as u32).clamp(monitor_wall::ROTATE_MIN_SECS, monitor_wall::ROTATE_MAX_SECS);
    });
}

/// Every online device (`true`) or the hand-picked ones.
pub fn set_all(ctx: &AppContext, all: bool) {
    update_settings(ctx, |s| s.all = all);
}

/// Picks or unpicks one device (`desk_id` in any formatting).
pub fn toggle_host(ctx: &AppContext, desk_id: &str) {
    let id = desk_id::clean(desk_id);
    if desk_id::is_valid(&id) {
        update_settings(ctx, |s| monitor_wall::toggle_selected(s, &id));
    }
}

/// Changes and saves the settings, then shows the result.
fn update_settings(ctx: &AppContext, change: impl FnOnce(&mut MonitorSettings)) {
    let monitor = {
        let mut st = ctx.wall.state.lock().unwrap();
        change(&mut st.settings);
        st.rotation.reset(Instant::now());
        st.settings.clone()
    };
    let mut stored = storage::load_settings();
    stored.monitor = monitor;
    storage::save_settings(&stored);
    refresh(ctx);
}

// ---- Applying the state ----

/// Recomputes the visible page, tells the relay when it changed and repaints the wall.
fn refresh(ctx: &AppContext) {
    {
        let mut st = ctx.wall.state.lock().unwrap();
        if !st.open {
            return;
        }
        let list = monitor_wall::wall_hosts(&st.hosts, &st.settings);
        let per_page = monitor_wall::per_page(&st.settings);
        let pages = monitor_wall::page_count(list.len(), per_page);
        st.page = st.page.min(pages - 1);
        let page = monitor_wall::page_of(&list, per_page, st.page);
        let ids: Vec<String> = page.iter().map(|h| h.id.clone()).collect();
        if st.relay_online && st.relay_supported && st.sent.as_ref() != Some(&ids) {
            ctx.send(ClientMessage::MonitorSet { targets: ids.clone() });
            st.sent = Some(ids.clone());
        }
        st.states.retain(|id, _| ids.contains(id));
        let tiles = page
            .iter()
            .map(|h| WallTile {
                id: h.id.clone(),
                title: h.name.clone(),
                subtitle: desk_id::format(&h.id),
                state: st.states.get(&h.id).copied().unwrap_or(TileState::Waiting),
            })
            .collect();
        let mut picks: Vec<WallPick> = st
            .hosts
            .iter()
            .map(|h| WallPick {
                id: h.id.clone(),
                name: h.name.clone(),
                online: h.online,
                selected: st.settings.selected.contains(&h.id),
            })
            .collect();
        picks.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
        let view = WallView {
            tiles,
            picks,
            page: st.page,
            pages,
            total: list.len(),
            settings: st.settings.clone(),
            relay_supported: st.relay_supported || !st.relay_online,
        };
        // Still under the state lock (lock order: state, then inbox), so concurrent refreshes reach the inbox and
        // the UI in the order they were computed. Neither call blocks.
        {
            let mut inbox = ctx.wall.inbox.lock().unwrap();
            inbox.visible = ids.into_iter().collect();
            let Inbox { visible, latest } = &mut *inbox;
            latest.retain(|id, _| visible.contains(id));
        }
        ctx.ui.wall_changed(view);
    }
}

/// Decodes the newest thumbnails and rotates pages while the wall is open.
fn run_worker(ctx: &Ctx) {
    loop {
        let tick = Instant::now();
        let rotate = {
            let mut st = ctx.wall.state.lock().unwrap();
            if !st.open {
                st.worker = false;
                return;
            }
            let pages = monitor_wall::page_count(
                monitor_wall::wall_hosts(&st.hosts, &st.settings).len(),
                monitor_wall::per_page(&st.settings),
            );
            let every = monitor_wall::rotate_interval(&st.settings);
            if st.rotation.due(every, pages, tick) {
                st.page = monitor_wall::step_page(st.page, 1, pages);
                true
            } else {
                false
            }
        };
        if rotate {
            refresh(ctx);
        }

        let batch = std::mem::take(&mut ctx.wall.inbox.lock().unwrap().latest);
        let frames: Vec<_> = batch
            .into_iter()
            .filter_map(|(id, jpeg)| {
                let (rgba, w, h) = decoder::decode_jpeg_rgba(&jpeg)?;
                Some((id, decoder::to_pixel_buffer(&rgba, w, h)))
            })
            .collect();
        if !frames.is_empty() {
            ctx.ui.wall_frames(frames);
        }
        if let Some(rest) = TICK.checked_sub(tick.elapsed()) {
            thread::sleep(rest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Outgoing;
    use crate::service::test_support::context_with_outbox;
    use std::sync::mpsc::Receiver;

    fn sets(rx: &Receiver<Outgoing>) -> Vec<Vec<String>> {
        rx.try_iter()
            .filter_map(|o| match o {
                Outgoing::Message(ClientMessage::MonitorSet { targets }) => Some(targets),
                _ => None,
            })
            .collect()
    }

    fn hosts(n: usize) -> Vec<RemoteHost> {
        (0..n)
            .map(|i| RemoteHost {
                desk_id: desk_id::format(&(200_000_000 + i).to_string()),
                device_name: format!("pc-{i:02}"),
                alias: String::new(),
                platform: "windows-pc".into(),
                online: true,
                last_seen: String::new(),
            })
            .collect()
    }

    /// Opens a wall with `n` online hosts and 3x3 pages, without the worker or the settings file.
    fn open_wall(n: usize) -> (Ctx, Receiver<Outgoing>) {
        let (ctx, rx) = context_with_outbox();
        set_hosts(&ctx, &hosts(n));
        {
            let mut st = ctx.wall.state.lock().unwrap();
            st.open = true;
            st.settings = MonitorSettings { grid: 3, ..Default::default() };
        }
        relay_registered(&ctx, true);
        (ctx, rx)
    }

    #[test]
    fn only_the_visible_page_is_watched() {
        let (ctx, rx) = open_wall(20);
        let first = sets(&rx);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].len(), 9);
        assert_eq!(first[0][0], "200000000");
        // Next page: the relay is told to swap the watch list (it stops the hidden hosts).
        {
            let mut st = ctx.wall.state.lock().unwrap();
            st.page = 2;
        }
        refresh(&ctx);
        let last = sets(&rx);
        assert_eq!(last, [vec!["200000018".to_string(), "200000019".to_string()]]);
        // Nothing changed: nothing is sent.
        refresh(&ctx);
        assert!(sets(&rx).is_empty());
        // Thumbnails of hidden hosts are dropped at once.
        on_frame(&ctx, "200000000", &[1]);
        on_frame(&ctx, "200000019", &[2]);
        assert_eq!(ctx.wall.inbox.lock().unwrap().latest.len(), 1);
    }

    #[test]
    fn closing_stops_every_stream_and_reconnecting_resends_the_page() {
        let (ctx, rx) = open_wall(3);
        assert_eq!(sets(&rx).len(), 1);
        relay_lost(&ctx);
        relay_registered(&ctx, true);
        assert_eq!(sets(&rx).len(), 1, "a new relay connection starts without watches");
        on_state(&ctx, "200000001", "live");
        on_state(&ctx, "999999999", "live");
        assert_eq!(ctx.wall.state.lock().unwrap().states.len(), 1, "states only for tiles on the page");
        close(&ctx);
        assert_eq!(sets(&rx), [Vec::<String>::new()]);
        on_frame(&ctx, "200000001", &[1]);
        assert!(ctx.wall.inbox.lock().unwrap().latest.is_empty());
    }

    /// Viewer-side cost of a full 4x4 page: `cargo test --release --features admin bench_wall_decode -- --ignored
    /// --nocapture`. Decodes a real 640 px desktop thumbnail the way the worker does.
    #[test]
    #[ignore]
    fn bench_wall_decode() {
        let bounds = core_engine::capture::enumerate_monitors()[0];
        let mut cap = core_engine::capture::ThumbnailCapturer::new_for_monitor(bounds, 640);
        cap.capture_frame();
        let jpeg = core_engine::codec::JpegEncoder::new(50)
            .encode_bgra(cap.pixels(), cap.width, cap.height, cap.width as usize)
            .unwrap();
        let n = 160u32;
        let t = Instant::now();
        for _ in 0..n {
            let (rgba, w, h) = decoder::decode_jpeg_rgba(&jpeg).unwrap();
            std::hint::black_box(decoder::to_pixel_buffer(&rgba, w, h));
        }
        let per = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
        println!(
            "\nthumbnail {}x{} ({:.1} KB): decode {:.2} ms; 16 tiles x 2 fps = {:.1} % of one core",
            cap.width,
            cap.height,
            jpeg.len() as f64 / 1024.0,
            per,
            per * 32.0 / 10.0
        );
    }

    #[test]
    fn a_relay_without_the_wall_is_never_asked() {
        let (ctx, rx) = context_with_outbox();
        ctx.wall.state.lock().unwrap().open = true;
        set_hosts(&ctx, &hosts(2));
        relay_registered(&ctx, false);
        assert!(sets(&rx).is_empty());
    }
}
