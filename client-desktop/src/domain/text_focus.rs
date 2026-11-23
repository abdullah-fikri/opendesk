//! Text focus on the host (`text-focus`): whether keyboard focus sits in a control the user can type into, and
//! where it is on the streamed monitor, so a touch viewer can raise its keyboard by itself.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// How long a new reading must stay the same before it is reported: focus moving between two fields, or a
/// dialog opening, passes through short-lived states the viewer should not see.
pub const DEBOUNCE: Duration = Duration::from_millis(100);

/// What the focus probe saw, in virtual-desktop pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FocusReading {
    /// Focus is in a control that takes typed text.
    pub editable: bool,
    /// The control hides what is typed.
    #[serde(default)]
    pub password: bool,
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// What the viewer is told: the focused control's rectangle normalized to the streamed monitor (0..=1, rounded
/// to 1/1000 so pixel jitter does not count as a change). A control outside that monitor has an empty rect.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextFocus {
    pub editable: bool,
    pub password: bool,
    pub norm_x: f64,
    pub norm_y: f64,
    pub norm_w: f64,
    pub norm_h: f64,
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

/// Normalizes `reading` to the monitor at `(left, top)` of `width` x `height` pixels.
pub fn normalize(reading: FocusReading, left: i32, top: i32, width: i32, height: i32) -> TextFocus {
    if !reading.editable || width <= 0 || height <= 0 {
        return TextFocus::default();
    }
    let (w, h) = (f64::from(width), f64::from(height));
    let x0 = (f64::from(reading.left - left) / w).clamp(0.0, 1.0);
    let y0 = (f64::from(reading.top - top) / h).clamp(0.0, 1.0);
    let x1 = (f64::from(reading.right - left) / w).clamp(0.0, 1.0);
    let y1 = (f64::from(reading.bottom - top) / h).clamp(0.0, 1.0);
    if x1 <= x0 || y1 <= y0 {
        return TextFocus { editable: true, password: reading.password, ..TextFocus::default() };
    }
    TextFocus {
        editable: true,
        password: reading.password,
        norm_x: round3(x0),
        norm_y: round3(y0),
        norm_w: round3((x1 - x0).max(0.0)),
        norm_h: round3((y1 - y0).max(0.0)),
    }
}

/// Decides when to report: only a state that held for [`DEBOUNCE`] and differs from the last one reported. The
/// viewer starts out assuming nothing is focused, so that state is never reported first.
#[derive(Debug, Default)]
pub struct FocusTracker {
    pending: Option<(TextFocus, Instant)>,
    sent: TextFocus,
}

impl FocusTracker {
    /// Feeds the state read at `now`; returns it when it should be sent.
    pub fn update(&mut self, now: Instant, state: TextFocus) -> Option<TextFocus> {
        if state == self.sent {
            self.pending = None;
            return None;
        }
        match self.pending {
            Some((p, since)) if p == state => {
                if now.duration_since(since) >= DEBOUNCE {
                    self.pending = None;
                    self.sent = state;
                    return Some(state);
                }
            }
            _ => self.pending = Some((state, now)),
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(left: i32, top: i32, right: i32, bottom: i32) -> FocusReading {
        FocusReading { editable: true, password: false, left, top, right, bottom }
    }

    #[test]
    fn normalizes_to_the_streamed_monitor() {
        let f = normalize(field(1920 + 192, 108, 1920 + 960, 162), 1920, 0, 1920, 1080);
        assert!(f.editable);
        assert_eq!((f.norm_x, f.norm_y, f.norm_w, f.norm_h), (0.1, 0.1, 0.4, 0.05));
    }

    #[test]
    fn clamps_fields_that_leave_the_monitor() {
        let f = normalize(field(-100, 1000, 500, 1200), 0, 0, 1000, 1100);
        assert_eq!((f.norm_x, f.norm_y, f.norm_w), (0.0, 0.909, 0.5));
        assert_eq!(f.norm_h, 0.091);
        // Entirely on another monitor: still editable, no rect.
        let off = normalize(field(-800, 0, -100, 40), 0, 0, 1000, 1000);
        assert!(off.editable && off.norm_w == 0.0 && off.norm_h == 0.0);
    }

    #[test]
    fn nothing_editable_has_no_rect() {
        let r = FocusReading { editable: false, ..field(10, 10, 50, 50) };
        assert_eq!(normalize(r, 0, 0, 100, 100), TextFocus::default());
        assert_eq!(normalize(field(1, 1, 2, 2), 0, 0, 0, 0), TextFocus::default(), "no monitor size");
    }

    #[test]
    fn password_flag_passes_through() {
        let r = FocusReading { password: true, ..field(0, 0, 10, 10) };
        assert!(normalize(r, 0, 0, 100, 100).password);
    }

    #[test]
    fn reports_only_stable_changes() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let edit = normalize(field(0, 0, 50, 10), 0, 0, 100, 100);
        let mut t = FocusTracker::default();
        assert_eq!(t.update(t0, TextFocus::default()), None, "initial nothing-focused is not reported");
        assert_eq!(t.update(ms(10), edit), None, "new state waits");
        assert_eq!(t.update(ms(60), edit), None);
        assert_eq!(t.update(ms(150), edit), Some(edit));
        assert_eq!(t.update(ms(300), edit), None, "unchanged is not resent");
        // A blip of another state is dropped.
        assert_eq!(t.update(ms(310), TextFocus::default()), None);
        assert_eq!(t.update(ms(350), edit), None);
        assert_eq!(t.update(ms(600), edit), None);
        // Focus leaves for good.
        assert_eq!(t.update(ms(700), TextFocus::default()), None);
        assert_eq!(t.update(ms(850), TextFocus::default()), Some(TextFocus::default()));
    }

    #[test]
    fn a_new_state_restarts_the_wait() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let a = normalize(field(0, 0, 50, 10), 0, 0, 100, 100);
        let b = normalize(field(0, 20, 50, 30), 0, 0, 100, 100);
        let mut t = FocusTracker::default();
        t.update(t0, a);
        assert_eq!(t.update(ms(90), b), None);
        assert_eq!(t.update(ms(150), b), None, "b only seen for 60 ms");
        assert_eq!(t.update(ms(200), b), Some(b));
    }

    #[test]
    fn reading_round_trips_for_the_helper_pipe() {
        let r = FocusReading { password: true, ..field(1, 2, 3, 4) };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<FocusReading>(&json).unwrap(), r);
    }
}
