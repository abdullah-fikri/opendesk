//! The per-session decoder thread: drains the inbox, decodes, hands frames to the UI and reports stats.

use super::*;

/// Decoder-thread counters for one `viewer.perf` window (fields documented in `domain::perf_stats`).
#[derive(Default)]
pub(super) struct ViewerPerf {
    pub(super) process: Stage,
    pub(super) handoff: Stage,
    pub(super) shown: Counter,
    pub(super) bytes: Counter,
    pub(super) batches: Counter,
}

impl ViewerPerf {
    /// The window's log line; resets the counters.
    pub(super) fn take_line(&mut self, session: &str, secs: f64) -> String {
        format!(
            "session={session} fps={:.1} process avg/max={} handoff={} kbps={} batches={}",
            self.shown.take() as f64 / secs,
            self.process.take_summary(),
            self.handoff.take_summary(),
            kbps(self.bytes.take(), secs),
            self.batches.take(),
        )
    }

    /// The overlay's numbers since the previous call, independent of the log window.
    pub(super) fn take_stats(&mut self, secs: f64) -> ViewerStats {
        self.process.take_lap();
        self.handoff.take_lap();
        self.batches.take_lap();
        ViewerStats {
            fps: (self.shown.take_lap() as f64 / secs) as f32,
            kbps: kbps(self.bytes.take_lap(), secs),
            usage: None,
        }
    }
}

/// Sends a `stats-ping` and notes when it left, for the RTT row and the clock offset.
pub(super) fn ping(sess: &Session, outbox: &Sender<Outgoing>) {
    let t = crate::service::file_transfer::now_ms();
    sess.latency.lock().ping_sent(t, latency::mono_us(Instant::now()));
    let _ = outbox.send(Outgoing::Message(ClientMessage::input(&sess.id, InputEvent::StatsPing { t })));
}

pub(super) fn request_key(outbox: &Sender<Outgoing>, id: &str) {
    let _ = outbox.send(Outgoing::Message(ClientMessage::input(id, InputEvent::VideoKeyframe)));
}

/// The session's current path label for the overlay and the tab.
pub(super) fn path_of(sess: &Session) -> &'static str {
    path_label(sess.p2p_active.load(Ordering::Relaxed), sess.p2p_relayed.load(Ordering::Relaxed))
}

/// Starts the session's decoder thread once. `show` runs for every decoded frame; `show_stats` once a second
/// with the overlay's (labels, values) while the session is visible and its stats overlay is on.
/// Hidden sessions (background tabs) are not decoded at all: their messages are dropped together with the
/// decoder state, and showing the session again asks the host for a key frame. `on_panic` runs if the
/// thread panics.
pub fn spawn<F, S, P>(sess: Arc<Session>, outbox: Sender<Outgoing>, show: F, show_stats: S, on_panic: P)
where
    F: Fn(&Arc<Session>, Frame) + Send + 'static,
    S: Fn(&Arc<Session>, (String, String)) + Send + 'static,
    P: FnOnce() + Send + 'static,
{
    if sess.decoder_started.swap(true, Ordering::SeqCst) {
        return;
    }
    let name = format!("viewer-decoder-{}", sess.id);
    crate::infra::thread::spawn_supervised(&name, on_panic, move || {
        let _mmcss = crate::infra::power::mmcss(crate::infra::power::Mmcss::Games);
        let mut frame_count = 0u32;
        let mut fps_timer = Instant::now();
        let mut current_fps = 0u32;
        let mut decoder: Option<ViewerDecoder> = None;
        let mut last_key_request: Option<Instant> = None;
        // Key frames asked for because the delta chain broke (loss), for the overlay.
        let mut key_requests = EventRate::default();
        let mut was_visible = sess.visible.load(Ordering::SeqCst);
        let mut perf = ViewerPerf::default();
        let mut perf_window = PerfWindow::new(Instant::now());
        let mut stats_window = PerfWindow::every(Instant::now(), STATS_INTERVAL);
        let (mut perf_usage, mut stats_usage) = (UsageSampler::new(), UsageSampler::new());
        perf_usage.sample(Instant::now());
        stats_usage.sample(Instant::now());

        while sess.alive.load(Ordering::SeqCst) {
            let visible = sess.visible.load(Ordering::SeqCst);
            if visible && !was_visible {
                // Everything sent while hidden was dropped: resync from a key frame.
                last_key_request = Some(Instant::now());
                request_key(&outbox, &sess.id);
            }
            was_visible = visible;

            // Sleeps until a message arrives; close and visibility changes wake it too (`Registry`), and the
            // timeout bounds the wait should a wake ever be missed.
            let batch = sess.inbox.drain_wait(VideoInbox::IDLE_WAIT);
            let stats_on = visible && sess.stats_on.load(Ordering::Relaxed);
            if let Some(secs) = perf_window.due(Instant::now()) {
                let usage = perf_usage.sample(Instant::now());
                let latency = sess.latency.lock().take_line();
                // Only windows that decoded something: hidden or idle sessions stay quiet.
                if perf.batches.in_window() > 0 {
                    let usage = usage.map_or_else(String::new, |u| format!(" {u}"));
                    logger::info("viewer.perf", &format!("{} {latency}{usage}", perf.take_line(&sess.id, secs)));
                    // Keeps the clock offset of the line's network and total stages fresh while the overlay,
                    // which pings every second, is off.
                    if !stats_on {
                        ping(&sess, &outbox);
                    }
                }
            }
            if let Some(secs) = stats_window.due(Instant::now()) {
                let mut viewer = perf.take_stats(secs);
                viewer.usage = stats_usage.sample(Instant::now());
                let latency = sess.latency.lock().take_lap();
                let loss = sess.udp_loss.lock().take_lap();
                if stats_on {
                    ping(&sess, &outbox);
                    let columns = {
                        let mut stats = sess.stats.lock();
                        stats.viewer = viewer;
                        stats.latency = latency;
                        stats.loss = sess.p2p_active.load(Ordering::Relaxed).then_some(loss);
                        stats.key_requests = key_requests.per_minute(Instant::now());
                        stats.columns(path_of(&sess), sess.e2e_active())
                    };
                    show_stats(&sess, columns);
                }
            }
            let Batch { msgs: batch, received } = batch;
            if batch.is_empty() {
                continue;
            }
            if !visible {
                decoder = None; // frees the framebuffer and the H.264 decoder while the tab is in the background
                continue;
            }
            perf.batches += 1;
            perf.bytes += batch.iter().map(|m| m.len() as u64).sum::<u64>();
            // The picture shows the newest message; its stamp times the whole frame.
            let stamp = batch.last().and_then(|m| read_stamp(m));
            let process_start = Instant::now();
            let out = decoder.get_or_insert_with(ViewerDecoder::new).process(batch);
            let decoded = Instant::now();
            perf.process.record(decoded - process_start);
            let (pixels, width, height, rows) = match out {
                ViewerOutput::Frame(buf, w, h, rows) => (buf, w, h, rows),
                ViewerOutput::Unchanged => continue,
                ViewerOutput::NeedKey => {
                    if last_key_request.is_none_or(|t| t.elapsed() >= KEY_REQUEST_INTERVAL) {
                        last_key_request = Some(Instant::now());
                        key_requests.record(Instant::now());
                        logger::info("viewer.video", "Delta chain broken; requesting a key frame.");
                        request_key(&outbox, &sess.id);
                    }
                    continue;
                }
            };
            frame_count += 1;
            let elapsed = fps_timer.elapsed();
            if elapsed >= Duration::from_secs(1) {
                current_fps = ((frame_count as f64 / elapsed.as_secs_f64()).round() as u32).max(1);
                frame_count = 0;
                fps_timer = Instant::now();
            }
            let path = path_of(&sess);
            let handoff_start = Instant::now();
            let latency = FrameLatency {
                meter: Arc::clone(&sess.latency),
                stamp,
                received: received.unwrap_or(process_start),
                decoded,
            };
            show(&sess, Frame { pixels, rows, width, height, fps: current_fps, path, latency });
            perf.handoff.record(handoff_start.elapsed());
            perf.shown += 1;
        }
    });
}
