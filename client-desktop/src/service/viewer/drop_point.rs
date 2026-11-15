//! Where each session's remote picture sits in the window, so files dropped on the window can be dropped at the
//! matching point of the host's screen (`file-drop`).

use std::collections::HashMap;

use parking_lot::Mutex;

/// Picture rectangle per session in client pixels: x, y, width, height.
static RECTS: Mutex<Option<HashMap<String, [i32; 4]>>> = Mutex::new(None);

/// The picture of session `id` moved or changed size.
pub fn set_rect(id: &str, rect: [i32; 4]) {
    RECTS.lock().get_or_insert_with(HashMap::new).insert(id.to_string(), rect);
}

/// Session `id` closed.
pub fn forget(id: &str) {
    if let Some(m) = RECTS.lock().as_mut() {
        m.remove(id);
    }
}

/// The point (client pixels) as 0..10000 of session `id`'s picture; `None` when it is outside the picture.
pub fn norm_at(id: &str, px: i32, py: i32) -> Option<(u32, u32)> {
    let [x, y, w, h] = RECTS.lock().as_ref()?.get(id).copied()?;
    if w <= 0 || h <= 0 || px < x || py < y || px >= x + w || py >= y + h {
        return None;
    }
    let scale = |v: i32, size: i32| ((v as i64 * 10_000) / size as i64) as u32;
    Some((scale(px - x, w), scale(py - y, h)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_client_points_onto_the_picture() {
        set_rect("t1", [100, 50, 800, 400]);
        assert_eq!(norm_at("t1", 100, 50), Some((0, 0)));
        assert_eq!(norm_at("t1", 500, 250), Some((5000, 5000)));
        assert_eq!(norm_at("t1", 899, 449), Some((9987, 9975)));
        // Outside the picture (toolbar, letterbox bars) or an unknown session: no drop point.
        assert_eq!(norm_at("t1", 99, 100), None);
        assert_eq!(norm_at("t1", 900, 100), None);
        assert_eq!(norm_at("nobody", 500, 250), None);
        forget("t1");
        assert_eq!(norm_at("t1", 500, 250), None);
    }
}
