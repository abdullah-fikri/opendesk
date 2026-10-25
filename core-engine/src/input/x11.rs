//! Mouse and keyboard injection through the XTEST extension. Text goes through the current keyboard mapping;
//! characters the layout cannot type are mapped onto a spare keycode for the moment of typing (like xdotool).

use std::error::Error;
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ConnectionExt as _, KEY_PRESS_EVENT, KEY_RELEASE_EVENT,
    MOTION_NOTIFY_EVENT, Window,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use super::keysym::{char_to_keysym, vk_to_keysym};
use super::{LAST_INJECTED_POS, pack_pos};

type Res<T> = Result<T, Box<dyn Error + Send + Sync>>;

const KEYSYM_SHIFT_L: u32 = 0xffe1;
const KEYSYM_CONTROL_L: u32 = 0xffe3;
const KEYSYM_ALT_L: u32 = 0xffe9;

/// One XTEST connection with a copy of the keyboard mapping.
struct Xtest {
    conn: RustConnection,
    root: Window,
    min_keycode: u8,
    keysyms_per_keycode: u8,
    keysyms: Vec<u32>,
}

impl Xtest {
    fn connect() -> Res<Self> {
        let (conn, screen) = x11rb::connect(None)?;
        conn.xtest_get_version(2, 2)?.reply()?;
        let setup = conn.setup();
        let (root, min_keycode, max_keycode) = (setup.roots[screen].root, setup.min_keycode, setup.max_keycode);
        let mut x = Self { conn, root, min_keycode, keysyms_per_keycode: 0, keysyms: Vec::new() };
        x.reload_mapping(max_keycode)?;
        Ok(x)
    }

    fn reload_mapping(&mut self, max_keycode: u8) -> Res<()> {
        let count = max_keycode - self.min_keycode + 1;
        let map = self.conn.get_keyboard_mapping(self.min_keycode, count)?.reply()?;
        self.keysyms_per_keycode = map.keysyms_per_keycode.max(1);
        self.keysyms = map.keysyms;
        Ok(())
    }

    /// Keycode and shift level (0 or 1) that produce `keysym` on the current layout.
    fn find(&self, keysym: u32) -> Option<(u8, usize)> {
        let per = self.keysyms_per_keycode as usize;
        for level in 0..per.min(2) {
            if let Some(i) = self.keysyms.chunks(per).position(|syms| syms.get(level) == Some(&keysym)) {
                return Some((self.min_keycode + i as u8, level));
            }
        }
        None
    }

    /// A keycode with no symbols, used to type characters the layout lacks.
    fn spare_keycode(&self) -> Option<u8> {
        let per = self.keysyms_per_keycode as usize;
        // Search from the top: low keycodes are more likely to be claimed by layout switches.
        let i = self.keysyms.chunks(per).rposition(|syms| syms.iter().all(|&s| s == 0))?;
        Some(self.min_keycode + i as u8)
    }

    fn fake(&self, kind: u8, detail: u8, x: i16, y: i16) -> Res<()> {
        self.conn.xtest_fake_input(kind, detail, x11rb::CURRENT_TIME, self.root, x, y, 0)?;
        Ok(())
    }

    fn key(&self, keycode: u8, down: bool) -> Res<()> {
        self.fake(if down { KEY_PRESS_EVENT } else { KEY_RELEASE_EVENT }, keycode, 0, 0)
    }

    fn tap(&self, keycode: u8) -> Res<()> {
        self.key(keycode, true)?;
        self.key(keycode, false)
    }

    fn keycode_for_vk(&self, vk: u8) -> Option<u8> {
        self.find(vk_to_keysym(vk)?).map(|(kc, _)| kc)
    }

    fn type_char(&mut self, ch: char) -> Res<()> {
        let keysym = char_to_keysym(ch);
        match self.find(keysym) {
            Some((kc, 0)) => self.tap(kc),
            Some((kc, _)) => {
                let shift = self.find(KEYSYM_SHIFT_L).map(|(s, _)| s);
                if let Some(s) = shift {
                    self.key(s, true)?;
                }
                self.tap(kc)?;
                if let Some(s) = shift {
                    self.key(s, false)?;
                }
                Ok(())
            }
            None => {
                let Some(spare) = self.spare_keycode() else { return Ok(()) };
                let per = self.keysyms_per_keycode;
                let syms = vec![keysym; per as usize];
                self.conn.change_keyboard_mapping(1, spare, per, &syms)?;
                // Round trip, then a moment for clients to process MappingNotify before the key arrives.
                self.conn.get_input_focus()?.reply()?;
                std::thread::sleep(Duration::from_millis(5));
                self.tap(spare)?;
                self.conn.get_input_focus()?.reply()?;
                self.conn.change_keyboard_mapping(1, spare, per, &vec![0; per as usize])?;
                Ok(())
            }
        }
    }
}

/// Injects mouse and keyboard input into the X display. Connects on first use and reconnects after errors.
pub struct InputInjector {
    x: Mutex<Option<Xtest>>,
}

impl Default for InputInjector {
    fn default() -> Self {
        Self::new()
    }
}

impl InputInjector {
    /// Cheap to create; the X connection opens on the first injected event.
    pub fn new() -> Self {
        Self { x: Mutex::new(None) }
    }

    fn with(&self, f: impl FnOnce(&mut Xtest) -> Res<()>) {
        let mut slot = self.x.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Xtest::connect().ok();
        }
        let Some(x) = slot.as_mut() else { return };
        if f(x).and_then(|_| x.conn.flush().map_err(Into::into)).is_err() {
            *slot = None;
        }
    }

    /// Moves the pointer to (`x`, `y`) in root-window coordinates.
    pub fn set_cursor_pos(&self, x: i32, y: i32) {
        LAST_INJECTED_POS.store(pack_pos(x, y), Ordering::Relaxed);
        self.with(|t| t.fake(MOTION_NOTIFY_EVENT, 0, x as i16, y as i16));
    }

    fn button(&self, button: i32, down: bool) {
        let detail = match button {
            0 => 1,
            2 => 3,
            _ => return,
        };
        self.with(|t| t.fake(if down { BUTTON_PRESS_EVENT } else { BUTTON_RELEASE_EVENT }, detail, 0, 0));
    }

    /// Presses mouse button `button`: 0 left, 2 right; other values are ignored.
    pub fn mouse_down(&self, button: i32) {
        self.button(button, true);
    }

    /// Releases mouse button `button`: 0 left, 2 right; other values are ignored.
    pub fn mouse_up(&self, button: i32) {
        self.button(button, false);
    }

    /// Scrolls by `delta_y` (120 = one notch up; small values are notches, as on Windows).
    pub fn mouse_wheel(&self, delta_y: i32) {
        if delta_y == 0 {
            return;
        }
        let notches = if delta_y.abs() < 40 { delta_y.abs() } else { (delta_y.abs() / 120).max(1) };
        let detail = if delta_y > 0 { 4 } else { 5 };
        self.with(|t| {
            for _ in 0..notches.min(20) {
                t.fake(BUTTON_PRESS_EVENT, detail, 0, 0)?;
                t.fake(BUTTON_RELEASE_EVENT, detail, 0, 0)?;
            }
            Ok(())
        });
    }

    /// Presses virtual key `vk`.
    pub fn key_down(&self, vk: u8) {
        self.with(|t| t.keycode_for_vk(vk).map_or(Ok(()), |kc| t.key(kc, true)));
    }

    /// Releases virtual key `vk`.
    pub fn key_up(&self, vk: u8) {
        self.with(|t| t.keycode_for_vk(vk).map_or(Ok(()), |kc| t.key(kc, false)));
    }

    /// Presses and releases virtual key `vk`.
    pub fn key_tap(&self, vk: u8) {
        self.with(|t| t.keycode_for_vk(vk).map_or(Ok(()), |kc| t.tap(kc)));
    }

    /// Presses `vk` with the given modifiers held, then releases everything.
    pub fn send_key_combo(&self, ctrl: bool, alt: bool, shift: bool, vk: u8) {
        self.with(|t| {
            let mods: Vec<u8> = [(ctrl, KEYSYM_CONTROL_L), (alt, KEYSYM_ALT_L), (shift, KEYSYM_SHIFT_L)]
                .into_iter()
                .filter(|(on, _)| *on)
                .filter_map(|(_, sym)| t.find(sym).map(|(kc, _)| kc))
                .collect();
            for &m in &mods {
                t.key(m, true)?;
            }
            if vk > 0
                && let Some(kc) = t.keycode_for_vk(vk)
            {
                t.tap(kc)?;
            }
            for &m in mods.iter().rev() {
                t.key(m, false)?;
            }
            Ok(())
        });
    }

    /// Types `text` as characters, independent of which keys produce them.
    pub fn send_unicode_text(&self, text: &str) {
        self.with(|t| {
            for ch in text.chars() {
                t.type_char(ch)?;
            }
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs an X server with XTEST (e.g. `xvfb-run cargo test -- --ignored`).
    #[test]
    #[ignore]
    fn moves_the_pointer_and_types_live() {
        let (conn, screen) = x11rb::connect(None).unwrap();
        let root = conn.setup().roots[screen].root;
        // The injector uses its own connection, so the server may answer this query before applying the move.
        let pointer_reaches = |want: (i16, i16)| {
            (0..100).any(|_| {
                let p = conn.query_pointer(root).unwrap().reply().unwrap();
                let ok = (p.root_x, p.root_y) == want;
                if !ok {
                    std::thread::sleep(Duration::from_millis(10));
                }
                ok
            })
        };
        let inj = InputInjector::new();
        inj.set_cursor_pos(123, 45);
        assert!(pointer_reaches((123, 45)));
        // Keys and text (including a character no layout key produces) must not fail or wedge the connection.
        inj.send_key_combo(true, false, true, 0x1B);
        inj.send_unicode_text("aB\u{20ac}\n");
        inj.set_cursor_pos(7, 8);
        assert!(pointer_reaches((7, 8)));
    }
}
