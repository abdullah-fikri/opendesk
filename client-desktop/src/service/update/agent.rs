//! Agent side of the update channel: the agent (SYSTEM, started by the service) tells the service through
//! the update directory whether a session is active, whether the relay is reachable and what the user's
//! settings say, and marks itself `healthy` once it reached the relay.

use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use super::store::{AgentReport, Store};
use super::{auto_update_env, now_unix, update_url_env};
use crate::infra::{storage, system_service};
use crate::service::context::{AppContext, Ctx};

const INTERVAL: Duration = Duration::from_secs(10);

/// Starts the heartbeat when this process is the service's agent; any other copy cannot write the directory.
pub fn spawn(ctx: Ctx) {
    if !(system_service::is_local_system() && system_service::installed()) {
        return;
    }
    crate::infra::thread::spawn_named("update-health", move || {
        // The service creates the directory with its ACL; never create it here.
        let store = Store::new(super::secure_dir::root());
        let mut healthy_written = false;
        loop {
            if store.root().is_dir() {
                let online = ctx.relay.online.load(Ordering::SeqCst);
                let _ = store.write("agent.json", &report(&ctx, online));
                if online && !healthy_written {
                    healthy_written = std::fs::write(store.path("healthy"), env!("CARGO_PKG_VERSION")).is_ok();
                }
            }
            thread::sleep(INTERVAL);
        }
    });
}

fn report(ctx: &AppContext, online: bool) -> AgentReport {
    let settings = storage::load_settings();
    let url = update_url_env().unwrap_or(settings.update_url);
    AgentReport {
        version: env!("CARGO_PKG_VERSION").to_string(),
        desk_id: ctx.local_id.clone(),
        idle: !ctx.is_hosting() && ctx.viewers.is_empty(),
        online,
        auto_update: settings.auto_update && auto_update_env(),
        update_url: url,
        app_dir: storage::app_dir().to_string_lossy().into_owned(),
        at: now_unix(),
    }
}
