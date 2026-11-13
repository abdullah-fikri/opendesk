//! Presentation of the command line: the banner, colors, the step list with its spinner and progress bar, and
//! the text shown for the service's update progress (`progress.json`). Everything here builds strings and is
//! unit-tested; `cli.rs` decides what to show and writes it through `infra::console`.

// The update view is used by the Windows update commands only; it is built and tested everywhere.
#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::VecDeque;
use std::time::Duration;

use crate::domain::edition::Edition;
use crate::service::update::store::{Phase, Progress};

const LOGO: [&str; 5] = [
    r"  ___                   ___          _",
    r" / _ \ _ __   ___ _ __ |   \ ___ ___| |__",
    r"| (_) | '_ \ / -_) '  \| |) / -_|_-<| / /",
    r" \___/| .__/ \___|_||_||___/\___/__/|_\_\",
    r"      |_|",
];
/// Column where the version line starts, under the logo.
const INFO_COLUMN: usize = 21;

const SPINNER_UNICODE: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const SPINNER_ASCII: [char; 4] = ['|', '/', '-', '\\'];
const BAR_WIDTH: usize = 24;
/// Download speed is averaged over this window.
const RATE_WINDOW_MS: u64 = 3_000;
/// A shorter span than this gives no speed (and no ETA) yet.
const RATE_MIN_SPAN_MS: u64 = 500;
/// Longest error text shown.
const MAX_ERROR_CHARS: usize = 300;

/// How text is drawn: with ANSI colors or plain, with Unicode symbols or ASCII only.
#[derive(Clone, Copy, Debug)]
pub struct Style {
    color: bool,
    unicode: bool,
}

impl Style {
    /// `color`: the output is a terminal that takes escape sequences and `NO_COLOR` is not set. `unicode`: the
    /// output displays block, check and braille characters.
    pub fn new(color: bool, unicode: bool) -> Style {
        Style { color, unicode }
    }

    fn paint(&self, sgr: &str, text: &str) -> String {
        if self.color && !text.is_empty() { format!("\x1b[{sgr}m{text}\x1b[0m") } else { text.to_string() }
    }

    fn red(&self, text: &str) -> String {
        self.paint("91", text)
    }

    fn green(&self, text: &str) -> String {
        self.paint("32", text)
    }

    fn cyan(&self, text: &str) -> String {
        self.paint("36", text)
    }

    fn dim(&self, text: &str) -> String {
        self.paint("2", text)
    }

    /// `text` in bold (headings of the help).
    pub fn bold(&self, text: &str) -> String {
        self.paint("1", text)
    }

    fn dot(&self) -> &'static str {
        if self.unicode { "·" } else { "-" }
    }

    fn ellipsis(&self) -> &'static str {
        if self.unicode { "…" } else { "..." }
    }
}

/// Name of the edition as users know it.
pub fn edition_label(edition: Edition) -> &'static str {
    match edition {
        Edition::Standard => "Standard",
        Edition::Admin => "Admin",
        Edition::Headless => "Host",
    }
}

/// The OpenDesk logo in red with `v<version> · <edition> · <desk id>` under it (the ID only when known).
pub fn banner(style: &Style, version: &str, edition: Edition, desk_id: Option<&str>) -> String {
    let dot = style.dot();
    let mut info = format!("v{version} {dot} {}", edition_label(edition));
    if let Some(id) = desk_id.filter(|id| !id.is_empty()) {
        info.push_str(&format!(" {dot} {id}"));
    }
    let mut lines: Vec<String> = LOGO.iter().map(|line| style.red(line)).collect();
    let last = LOGO[LOGO.len() - 1];
    let pad = " ".repeat(INFO_COLUMN.saturating_sub(last.len()));
    lines[LOGO.len() - 1] = format!("{}{pad}{info}", style.red(last));
    lines.join("\n")
}

/// State of one step of a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Done,
    Active,
    Failed,
    Pending,
}

/// One line of the step list.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub mark: Mark,
    pub label: String,
    pub detail: String,
}

impl Step {
    fn new(mark: Mark, label: impl Into<String>, detail: impl Into<String>) -> Step {
        Step { mark, label: label.into(), detail: detail.into() }
    }
}

/// The step list as lines: a status icon (✓, spinner frame `frame`, ✗), the label padded to one column and
/// the detail, cut to `width` columns when given.
pub fn render_steps(steps: &[Step], style: &Style, frame: usize, width: Option<usize>) -> Vec<String> {
    let column = steps.iter().map(|s| s.label.chars().count()).max().unwrap_or(0) + 2;
    steps
        .iter()
        .map(|step| {
            let icon = match step.mark {
                Mark::Done => style.green(if style.unicode { "✓" } else { "+" }),
                Mark::Failed => style.red(if style.unicode { "✗" } else { "x" }),
                Mark::Active => style.cyan(&spinner(style, frame).to_string()),
                Mark::Pending => " ".to_string(),
            };
            let label = format!("{:<column$}", step.label);
            let room = width.map(|w| w.saturating_sub(3 + column));
            let detail = match room {
                Some(room) => truncate(&step.detail, room, style),
                None => step.detail.clone(),
            };
            let label = if step.mark == Mark::Pending { style.dim(&label) } else { label };
            format!("{icon} {label}{detail}").trim_end().to_string()
        })
        .collect()
}

fn spinner(style: &Style, frame: usize) -> char {
    if style.unicode {
        SPINNER_UNICODE[frame % SPINNER_UNICODE.len()]
    } else {
        SPINNER_ASCII[frame % SPINNER_ASCII.len()]
    }
}

/// `text` cut to `room` characters, ending with an ellipsis when cut.
fn truncate(text: &str, room: usize, style: &Style) -> String {
    if text.chars().count() <= room {
        return text.to_string();
    }
    let ellipsis = style.ellipsis();
    let keep = room.saturating_sub(ellipsis.chars().count());
    format!("{}{ellipsis}", text.chars().take(keep).collect::<String>())
}

/// `[██████░░░░]` (ASCII `[=====>----]`) filled to `done / total`.
pub fn bar(done: u64, total: u64, width: usize, style: &Style) -> String {
    let filled = if total == 0 { 0 } else { ((done.min(total) as u128 * width as u128) / total as u128) as usize };
    let inner = if style.unicode {
        format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
    } else if filled == 0 || filled == width {
        format!("{}{}", "=".repeat(filled), "-".repeat(width - filled))
    } else {
        format!("{}>{}", "=".repeat(filled - 1), "-".repeat(width - filled))
    };
    format!("[{inner}]")
}

/// Megabytes with one decimal (`17.2`).
fn megabytes(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1_000_000.0)
}

/// `3.1 MB/s` or `850 KB/s`.
pub fn speed(bytes_per_sec: f64) -> String {
    if bytes_per_sec >= 1_000_000.0 {
        format!("{:.1} MB/s", bytes_per_sec / 1_000_000.0)
    } else {
        format!("{:.0} KB/s", bytes_per_sec / 1_000.0)
    }
}

/// `4s`, `1m 05s`, `1h 02m`.
pub fn eta(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m", secs / 3600, secs % 3600 / 60),
    }
}

/// `[bar]  58%  17.2/29.6 MB  3.1 MB/s  ETA 4s` (speed and ETA once known).
pub fn download_line(bytes: u64, total: u64, rate: Option<f64>, style: &Style) -> String {
    let percent = (bytes.min(total) * 100).checked_div(total).unwrap_or(0);
    let mut line =
        format!("{} {percent:>3}%  {}/{} MB", bar(bytes, total, BAR_WIDTH, style), megabytes(bytes), megabytes(total));
    if let Some(rate) = rate.filter(|r| *r > 0.0) {
        let left = total.saturating_sub(bytes) as f64 / rate;
        line.push_str(&format!("  {}  ETA {}", speed(rate), eta(left.ceil() as u64)));
    }
    line
}

/// Download speed from the byte counts the CLI saw, averaged over the last few seconds.
#[derive(Default)]
pub struct RateMeter {
    samples: VecDeque<(u64, u64)>,
}

impl RateMeter {
    /// Records `bytes` downloaded at `at_ms`; a smaller count (a new download) starts over.
    pub fn push(&mut self, at_ms: u64, bytes: u64) {
        if self.samples.back().is_some_and(|&(_, b)| bytes < b) {
            self.samples.clear();
        }
        if self.samples.back().is_some_and(|&(_, b)| b == bytes) {
            // The service writes 4 times a second at most; an unchanged count is the same write read again.
            return;
        }
        self.samples.push_back((at_ms, bytes));
        while self.samples.front().is_some_and(|&(t, _)| at_ms.saturating_sub(t) > RATE_WINDOW_MS) {
            self.samples.pop_front();
        }
    }

    /// Bytes per second, once the samples span long enough.
    pub fn rate(&self) -> Option<f64> {
        let (&(t0, b0), &(t1, b1)) = (self.samples.front()?, self.samples.back()?);
        let span = t1.saturating_sub(t0);
        (span >= RATE_MIN_SPAN_MS).then(|| (b1 - b0) as f64 * 1000.0 / span as f64)
    }
}

/// `12 min ago`, `in 5 h 48 min`, `just now` for a time `delta` seconds from now (zero or negative: past).
pub fn relative(delta: i64) -> String {
    let past = delta <= 0;
    let secs = delta.unsigned_abs();
    let amount = match secs {
        0..60 => return if past { "just now".into() } else { "in under a minute".into() },
        60..3600 => format!("{} min", secs / 60),
        3600..86_400 => format!("{} h {:02} min", secs / 3600, secs % 3600 / 60),
        _ => match secs / 86_400 {
            1 => "1 day".to_string(),
            days => format!("{days} days"),
        },
    };
    if past { format!("{amount} ago") } else { format!("in {amount}") }
}

/// Text from the service with control characters (escape sequences included) replaced, at most
/// [`MAX_ERROR_CHARS`] long.
///
/// SECURITY: errors can quote a download server; a terminal must not interpret anything in them.
pub fn sanitize(text: &str) -> String {
    let clean: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).take(MAX_ERROR_CHARS).collect();
    clean.trim().to_string()
}

fn sessions(n: u32) -> String {
    if n == 1 { "1 session".into() } else { format!("{n} sessions") }
}

/// One line for `opendesk update status`: the last phase the service reported and how long ago.
pub fn phase_summary(p: &Progress, now_ms: u64) -> String {
    let v = p.version.as_deref().map(sanitize).unwrap_or_else(|| "?".into());
    let error = p.error.as_deref().map(sanitize);
    let text = match p.phase {
        Phase::Unknown => return "none yet".into(),
        Phase::Checking => "checking for a release".to_string(),
        Phase::Downloading => {
            let percent = (p.bytes.min(p.total) * 100).checked_div(p.total).unwrap_or(0);
            format!("downloading {v} ({percent}%)")
        }
        Phase::Verifying => format!("{v} downloaded and verified"),
        Phase::WaitingIdle => format!("{v} waits for {} to end", sessions(p.sessions_active.max(1))),
        Phase::Installing => format!("installing {v}"),
        Phase::Health => format!("{v} installed, checking that it reaches the relay"),
        Phase::Done => match &error {
            Some(e) => format!("running {v} ({e})"),
            None => format!("{v} installed"),
        },
        Phase::Failed => format!("failed: {}", error.as_deref().unwrap_or("unknown error")),
        Phase::UpToDate => format!("up to date (newest release {v})"),
    };
    let age = (p.updated_at / 1000) as i64 - (now_ms / 1000) as i64;
    format!("{text} - {}", relative(age.min(0)))
}

/// What a command asked the service to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Job {
    /// `update` (no target: the newest release) or `update <version>`.
    Update {
        target: Option<String>,
    },
    /// `rollback [version]`.
    Rollback {
        target: Option<String>,
    },
    Pin {
        version: String,
    },
    Unpin,
}

/// What the CLI has seen of its request so far.
pub struct Seen<'a> {
    /// The service removed the request file.
    pub picked_up: bool,
    /// `progress.json` written after the request was queued.
    pub progress: Option<&'a Progress>,
    /// Download speed measured by the CLI.
    pub rate: Option<f64>,
    /// Version of this (installed) executable.
    pub current: &'a str,
    /// Step that was active on the previous poll; a failure is shown there.
    pub last_active: usize,
}

/// Whether the command keeps watching.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Watching,
    Finished { ok: bool, message: String },
}

/// The steps to draw and whether the command is finished.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub steps: Vec<Step>,
    /// Index of the step in progress (or that failed).
    pub active: usize,
    pub outcome: Outcome,
}

const SEND_REQUEST: &str = "Send request to the service";
/// The service polls every 2 s; a request still queued after this means it is not running.
pub const PICKUP_TIMEOUT: Duration = Duration::from_secs(20);
/// The service reports a phase as soon as it acts on a request; nothing after this means it cannot act yet.
pub const PROGRESS_TIMEOUT: Duration = Duration::from_secs(20);

/// Why watching `job` should stop without an outcome: `waited` since the request was queued, `since_pickup`
/// since the service took it, `has_progress` once it reported a phase for it.
pub fn stalled(job: &Job, waited: Duration, since_pickup: Option<Duration>, has_progress: bool) -> Option<String> {
    match since_pickup {
        None if waited >= PICKUP_TIMEOUT => Some(format!(
            "The OpenDesk service has not taken the request after {} s; it stays queued until the service runs.\n\
             Check the service with: sc query OpenDeskService",
            PICKUP_TIMEOUT.as_secs()
        )),
        Some(d)
            if d >= PROGRESS_TIMEOUT && !has_progress && matches!(job, Job::Update { .. } | Job::Rollback { .. }) =>
        {
            Some(
                "The service took the request but reports no progress yet. It checks for updates only while the \
                 OpenDesk app runs in a signed-in session.\nLook again later with: opendesk update status"
                    .into(),
            )
        }
        _ => None,
    }
}

/// The step list and outcome of `job` for what was seen.
pub fn view(job: &Job, seen: &Seen, style: &Style) -> View {
    match job {
        Job::Update { target } => update_view(target.as_deref(), seen, style),
        Job::Rollback { target } => {
            let label = format!("Install {}", target.as_deref().unwrap_or("the previous version"));
            let started = |v: &str| {
                format!(
                    "Rolling back to {v}{} check the result with `opendesk update status` in 1-2 minutes.",
                    style.ellipsis()
                )
            };
            install_job_view(label, seen, started)
        }
        Job::Pin { version } => instant_view(
            format!("Pin to {version}"),
            seen,
            format!("Pinned to {version}: automatic updates install only that version (opendesk unpin to undo)."),
        ),
        Job::Unpin => instant_view("Follow new releases".into(), seen, "Following new releases again.".into()),
    }
}

/// Steps of `pin` and `unpin`: done once the service took the request.
fn instant_view(label: String, seen: &Seen, message: String) -> View {
    if seen.picked_up {
        let steps = vec![Step::new(Mark::Done, SEND_REQUEST, ""), Step::new(Mark::Done, label, "")];
        return View { steps, active: 1, outcome: Outcome::Finished { ok: true, message } };
    }
    let steps = vec![Step::new(Mark::Active, SEND_REQUEST, ""), Step::new(Mark::Pending, label, "")];
    View { steps, active: 0, outcome: Outcome::Watching }
}

/// Steps of `rollback`: the request, then the installer of the target.
fn install_job_view(label: String, seen: &Seen, started: impl Fn(&str) -> String) -> View {
    let request = Step::new(if seen.picked_up { Mark::Done } else { Mark::Active }, SEND_REQUEST, "");
    let p = seen.progress;
    let version = p.and_then(|p| p.version.as_deref()).map(sanitize).unwrap_or_default();
    match p.map(|p| p.phase) {
        Some(Phase::Installing) => View {
            steps: vec![request, Step::new(Mark::Done, label, "installer started")],
            active: 1,
            outcome: Outcome::Finished { ok: true, message: started(&version) },
        },
        Some(Phase::Failed) => {
            let error = failure(p);
            View {
                steps: vec![request, Step::new(Mark::Failed, label, error.clone())],
                active: 1,
                outcome: Outcome::Finished { ok: false, message: error },
            }
        }
        _ => {
            let mark = if seen.picked_up { Mark::Active } else { Mark::Pending };
            View {
                steps: vec![request, Step::new(mark, label, "")],
                active: usize::from(seen.picked_up),
                outcome: Outcome::Watching,
            }
        }
    }
}

fn failure(p: Option<&Progress>) -> String {
    p.and_then(|p| p.error.as_deref()).map(sanitize).unwrap_or_else(|| "unknown error".into())
}

/// Steps of `update [version]`: check, download, verify, wait for sessions, install.
fn update_view(target: Option<&str>, seen: &Seen, style: &Style) -> View {
    let check_label = match target {
        Some(v) => format!("Check release {v}"),
        None => "Check for latest release".to_string(),
    };
    let labels =
        [check_label.as_str(), "Download", "Verify signature", "Wait for sessions to end", "Install"].map(String::from);
    let p = seen.progress;
    let version = p.and_then(|p| p.version.as_deref()).map(sanitize);
    let current = seen.current;
    let mut steps: Vec<Step> = labels.iter().map(|l| Step::new(Mark::Pending, l.clone(), "")).collect();
    let available = |v: &str| format!("{v} available (now {current})");

    let Some(p) = p else {
        steps[0].mark = Mark::Active;
        steps[0].detail =
            if seen.picked_up { "asking the release server" } else { "waiting for the OpenDesk service" }.to_string();
        return View { steps, active: 0, outcome: Outcome::Watching };
    };
    let active = match p.phase {
        Phase::Checking | Phase::Unknown => 0,
        Phase::Downloading => 1,
        Phase::Verifying => 2,
        Phase::WaitingIdle => 3,
        Phase::Installing => 4,
        Phase::UpToDate => {
            let message = match version.as_deref() {
                Some(v) if v == current => format!("OpenDesk {current} is up to date."),
                Some(v) => format!("OpenDesk {current} is up to date (newest release {v})."),
                None => format!("OpenDesk {current} is up to date."),
            };
            let steps = vec![Step::new(Mark::Done, check_label, "no newer release")];
            return View { steps, active: 0, outcome: Outcome::Finished { ok: true, message } };
        }
        Phase::Failed => {
            let error = failure(Some(p));
            let at = seen.last_active.min(steps.len() - 1);
            for step in &mut steps[..at] {
                step.mark = Mark::Done;
            }
            steps[at].mark = Mark::Failed;
            steps[at].detail = error.clone();
            return View {
                steps,
                active: at,
                outcome: Outcome::Finished { ok: false, message: format!("Update failed: {error}") },
            };
        }
        Phase::Health | Phase::Done => {
            for step in &mut steps {
                step.mark = Mark::Done;
            }
            let v = version.unwrap_or_default();
            let message = format!("OpenDesk {v} is installed.");
            return View { steps, active: 4, outcome: Outcome::Finished { ok: true, message } };
        }
    };
    for step in &mut steps[..active] {
        step.mark = Mark::Done;
    }
    steps[active].mark = Mark::Active;
    if active > 0
        && let Some(v) = &version
    {
        steps[0].detail = available(v);
    }
    let v = version.unwrap_or_default();
    let ell = style.ellipsis();
    match p.phase {
        Phase::Checking | Phase::Unknown => steps[0].detail = "asking the release server".into(),
        Phase::Downloading => steps[1].detail = download_line(p.bytes, p.total, seen.rate, style),
        Phase::Verifying => steps[2].detail = "size and SHA-256 match the signed manifest".into(),
        Phase::WaitingIdle => {
            steps[2].detail = "OK".into();
            steps[3].detail = format!("{} active; installs when they end", sessions(p.sessions_active.max(1)));
        }
        _ => {}
    }
    if p.phase == Phase::Installing {
        steps[2].detail = "OK".into();
        steps[4].mark = Mark::Done;
        steps[4].detail = "installer started".into();
        let message = format!("Installing {v}{ell} check the result with `opendesk update status` in 1-2 minutes.");
        return View { steps, active, outcome: Outcome::Finished { ok: true, message } };
    }
    View { steps, active, outcome: Outcome::Watching }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: Style = Style { color: false, unicode: true };
    const ASCII: Style = Style { color: false, unicode: false };

    fn progress(phase: Phase) -> Progress {
        Progress { phase, version: Some("1.3.0".into()), ..Progress::default() }
    }

    fn seen(progress: Option<&Progress>) -> Seen<'_> {
        Seen { picked_up: true, progress, rate: None, current: "1.2.1", last_active: 0 }
    }

    fn marks(v: &View) -> Vec<Mark> {
        v.steps.iter().map(|s| s.mark).collect()
    }

    #[test]
    fn banner_shows_version_edition_and_id_without_color_when_off() {
        let text = banner(&PLAIN, "1.3.0", Edition::Admin, Some("418 541 168"));
        let last = text.lines().last().unwrap();
        assert_eq!(last, "      |_|            v1.3.0 · Admin · 418 541 168");
        assert!(!text.contains('\x1b'));
        assert_eq!(text.lines().count(), 5);
        let ascii = banner(&ASCII, "1.3.0", Edition::Headless, None);
        assert!(ascii.ends_with("v1.3.0 - Host"));
        assert!(ascii.is_ascii());
        let red = banner(&Style::new(true, true), "1.3.0", Edition::Standard, None);
        assert!(red.starts_with("\x1b[91m"));
    }

    #[test]
    fn bar_fills_proportionally_in_both_character_sets() {
        assert_eq!(bar(0, 100, 10, &PLAIN), "[░░░░░░░░░░]");
        assert_eq!(bar(58, 100, 10, &PLAIN), "[█████░░░░░]");
        assert_eq!(bar(100, 100, 10, &PLAIN), "[██████████]");
        assert_eq!(bar(50, 100, 10, &ASCII), "[====>-----]");
        assert_eq!(bar(100, 100, 10, &ASCII), "[==========]");
        assert_eq!(bar(5, 0, 4, &ASCII), "[----]");
        assert_eq!(bar(200, 100, 4, &ASCII), "[====]");
    }

    #[test]
    fn durations_speeds_and_sizes_are_short() {
        assert_eq!(eta(4), "4s");
        assert_eq!(eta(65), "1m 05s");
        assert_eq!(eta(3720), "1h 02m");
        assert_eq!(speed(3_100_000.0), "3.1 MB/s");
        assert_eq!(speed(850_000.0), "850 KB/s");
        assert_eq!(
            download_line(17_200_000, 29_600_000, Some(3_100_000.0), &ASCII),
            "[============>-----------]  58%  17.2/29.6 MB  3.1 MB/s  ETA 4s"
        );
        assert_eq!(download_line(0, 0, None, &ASCII), format!("[{}]   0%  0.0/0.0 MB", "-".repeat(24)));
    }

    #[test]
    fn rate_is_averaged_over_recent_samples() {
        let mut m = RateMeter::default();
        m.push(0, 0);
        assert_eq!(m.rate(), None);
        m.push(250, 250_000);
        assert_eq!(m.rate(), None, "too short a span");
        m.push(1000, 1_000_000);
        m.push(1100, 1_000_000);
        assert_eq!(m.rate(), Some(1_000_000.0));
        m.push(5000, 2_000_000);
        assert_eq!(m.rate(), None, "older samples left the window");
        m.push(6000, 3_000_000);
        assert_eq!(m.rate(), Some(1_000_000.0));
        m.push(6500, 10);
        assert_eq!(m.rate(), None, "a new download starts over");
    }

    #[test]
    fn relative_times_read_naturally() {
        assert_eq!(relative(-5), "just now");
        assert_eq!(relative(30), "in under a minute");
        assert_eq!(relative(-12 * 60), "12 min ago");
        assert_eq!(relative(5 * 3600 + 48 * 60), "in 5 h 48 min");
        assert_eq!(relative(-86_400), "1 day ago");
        assert_eq!(relative(3 * 86_400), "in 3 days");
    }

    #[test]
    fn service_text_cannot_carry_escape_sequences() {
        assert_eq!(sanitize("bad\x1b[2Jtext\r\n"), "bad [2Jtext");
        assert_eq!(sanitize(&"x".repeat(1000)).len(), MAX_ERROR_CHARS);
    }

    #[test]
    fn steps_render_icons_aligned_labels_and_cut_details() {
        let steps = vec![
            Step::new(Mark::Done, "Check for latest release", "1.3.0 available (now 1.2.1)"),
            Step::new(Mark::Active, "Download", "[bar]"),
            Step::new(Mark::Failed, "Verify signature", "bad hash"),
            Step::new(Mark::Pending, "Install", ""),
        ];
        let lines = render_steps(&steps, &PLAIN, 2, None);
        assert_eq!(lines[0], "✓ Check for latest release  1.3.0 available (now 1.2.1)");
        assert_eq!(lines[1], "⠹ Download                  [bar]");
        assert_eq!(lines[2], "✗ Verify signature          bad hash");
        assert_eq!(lines[3], "  Install");
        let ascii = render_steps(&steps, &ASCII, 1, Some(40));
        assert_eq!(ascii[0], "+ Check for latest release  1.3.0 av...");
        assert_eq!(ascii[1], "/ Download                  [bar]");
        assert!(ascii.iter().all(|l| l.chars().count() < 40));
        let colored = render_steps(&steps, &Style::new(true, true), 0, None);
        assert!(colored[0].starts_with("\x1b[32m✓\x1b[0m"));
        assert!(colored[3].contains("\x1b[2m"));
    }

    #[test]
    fn update_steps_follow_the_service_phases() {
        let job = Job::Update { target: None };
        let waiting = Seen { picked_up: false, ..seen(None) };
        let v = view(&job, &waiting, &PLAIN);
        assert_eq!(v.steps[0].detail, "waiting for the OpenDesk service");
        assert_eq!(v.outcome, Outcome::Watching);

        let p = Progress { bytes: 50, total: 100, ..progress(Phase::Downloading) };
        let v = view(&job, &seen(Some(&p)), &PLAIN);
        assert_eq!(marks(&v), [Mark::Done, Mark::Active, Mark::Pending, Mark::Pending, Mark::Pending]);
        assert_eq!(v.steps[0].detail, "1.3.0 available (now 1.2.1)");
        assert!(v.steps[1].detail.contains(" 50%"));
        assert_eq!(v.active, 1);

        let p = Progress { sessions_active: 2, ..progress(Phase::WaitingIdle) };
        let v = view(&job, &seen(Some(&p)), &PLAIN);
        assert_eq!(v.steps[3].detail, "2 sessions active; installs when they end");
        assert_eq!(v.outcome, Outcome::Watching);

        let v = view(&job, &seen(Some(&progress(Phase::Installing))), &ASCII);
        assert_eq!(marks(&v), [Mark::Done; 5]);
        assert_eq!(
            v.outcome,
            Outcome::Finished {
                ok: true,
                message: "Installing 1.3.0... check the result with `opendesk update status` in 1-2 minutes.".into()
            }
        );
    }

    #[test]
    fn update_ends_on_up_to_date_and_on_failure() {
        let job = Job::Update { target: Some("1.3.0".into()) };
        let p = Progress { version: Some("1.2.1".into()), ..progress(Phase::UpToDate) };
        let v = view(&job, &seen(Some(&p)), &PLAIN);
        assert_eq!(v.steps.len(), 1);
        assert_eq!(v.steps[0].label, "Check release 1.3.0");
        assert_eq!(v.outcome, Outcome::Finished { ok: true, message: "OpenDesk 1.2.1 is up to date.".into() });

        let p = Progress { error: Some("network down".into()), ..progress(Phase::Failed) };
        let v = view(&job, &Seen { last_active: 1, ..seen(Some(&p)) }, &PLAIN);
        assert_eq!(marks(&v), [Mark::Done, Mark::Failed, Mark::Pending, Mark::Pending, Mark::Pending]);
        assert_eq!(v.steps[1].detail, "network down");
        assert_eq!(v.outcome, Outcome::Finished { ok: false, message: "Update failed: network down".into() });
    }

    #[test]
    fn rollback_pin_and_unpin_use_the_same_steps() {
        let rollback = Job::Rollback { target: None };
        let v = view(&rollback, &seen(None), &PLAIN);
        assert_eq!(marks(&v), [Mark::Done, Mark::Active]);
        assert_eq!(v.steps[1].label, "Install the previous version");
        let p = Progress { version: Some("1.2.0".into()), ..progress(Phase::Installing) };
        assert!(matches!(view(&rollback, &seen(Some(&p)), &PLAIN).outcome, Outcome::Finished { ok: true, .. }));
        let p = Progress { error: Some("no kept installer".into()), ..progress(Phase::Failed) };
        let v = view(&rollback, &seen(Some(&p)), &PLAIN);
        assert_eq!(v.outcome, Outcome::Finished { ok: false, message: "no kept installer".into() });

        let pin = Job::Pin { version: "1.2.0".into() };
        let v = view(&pin, &Seen { picked_up: false, ..seen(None) }, &PLAIN);
        assert_eq!((marks(&v), v.outcome), (vec![Mark::Active, Mark::Pending], Outcome::Watching));
        assert!(matches!(view(&Job::Unpin, &seen(None), &PLAIN).outcome, Outcome::Finished { ok: true, .. }));
    }

    #[test]
    fn watching_stops_with_a_hint_when_the_service_does_not_act() {
        let update = Job::Update { target: None };
        let s = Duration::from_secs;
        assert_eq!(stalled(&update, s(5), None, false), None);
        assert!(stalled(&update, s(20), None, false).unwrap().contains("sc query OpenDeskService"));
        assert_eq!(stalled(&update, s(30), Some(s(10)), false), None);
        assert!(stalled(&update, s(30), Some(s(20)), false).unwrap().contains("opendesk update status"));
        assert_eq!(stalled(&update, s(30), Some(s(20)), true), None, "progress arrived");
        assert_eq!(stalled(&Job::Unpin, s(30), Some(s(20)), false), None, "unpin needs no progress");
    }

    #[test]
    fn status_summarises_the_last_phase() {
        let now = 10_000_000;
        let p = Progress { bytes: 25, total: 100, updated_at: now - 120_000, ..progress(Phase::Downloading) };
        assert_eq!(phase_summary(&p, now), "downloading 1.3.0 (25%) - 2 min ago");
        let p = Progress { error: Some("x\x1b".into()), updated_at: now, ..progress(Phase::Failed) };
        assert_eq!(phase_summary(&p, now), "failed: x - just now");
        assert_eq!(phase_summary(&Progress::default(), now), "none yet");
    }
}
