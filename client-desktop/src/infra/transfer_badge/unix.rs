//! Outside Windows the host shows no transfer badge; the main window's transfer list still does when open.

use std::time::Duration;

/// Nothing to show.
pub fn show(_text: &str, _hide_after: Option<Duration>) {}

/// Nothing to hide.
pub fn hide() {}
