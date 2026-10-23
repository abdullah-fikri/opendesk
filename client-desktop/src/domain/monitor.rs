//! A host's monitors as seen by the viewer.

/// Most monitors a viewer accepts from one host.
const MAX_MONITORS: usize = 16;
/// Highest monitor index a viewer accepts.
const MAX_INDEX: u64 = 63;

/// One host monitor in the viewer's picker.
#[derive(Clone, Debug, PartialEq)]
pub struct Monitor {
    pub index: i32,
    pub width: i32,
    pub height: i32,
    pub primary: bool,
}

/// Bounds a host's announcement of `(index, width, height, primary)` entries and the streamed index.
/// Values from the wire are never trusted.
pub fn from_announcement(list: impl IntoIterator<Item = (u64, i64, i64, bool)>, active: u64) -> (Vec<Monitor>, i32) {
    let monitors = list
        .into_iter()
        .take(MAX_MONITORS)
        .map(|(index, width, height, primary)| Monitor {
            index: index.min(MAX_INDEX) as i32,
            width: width.clamp(0, 65_535) as i32,
            height: height.clamp(0, 65_535) as i32,
            primary,
        })
        .collect();
    (monitors, active.min(MAX_INDEX) as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announcements_are_bounded() {
        let (monitors, active) = from_announcement((0..20).map(|i| (i * 10, 1 << 20, -5, i == 0)), 99);
        assert_eq!(monitors.len(), 16);
        assert_eq!((monitors[15].index, monitors[0].width, monitors[0].height), (63, 65_535, 0));
        assert_eq!(active, 63);
    }
}
