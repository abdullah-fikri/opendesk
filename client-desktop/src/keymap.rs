//! Viewer keyboard forwarding: turns Slint key press/release events into host `keydown`/`keyup`
//! (real key state, so Ctrl+click, Ctrl+scroll, Shift+arrows and held modifiers work) while plain
//! typing still goes as unicode `text` (layout- and IME-independent). Also tracks keys the host
//! injected so a session end can never leave one stuck down.

use std::sync::Mutex;

use core_engine::input::InputInjector;

pub const VK_SHIFT: u8 = 0x10;
pub const VK_CONTROL: u8 = 0x11;
pub const VK_MENU: u8 = 0x12;
const VK_V: u8 = 0x56;

#[link(name = "user32")]
unsafe extern "system" {
    fn VkKeyScanW(ch: u16) -> i16;
}

/// Slint modifier key codes (i_slint_common::key_codes) mapped to the generic Win32 VK.
/// Meta is left out on purpose: the viewer's low-level hook already forwards the Win key
/// (and swallows it), AltGr/CapsLock are left out because typed text already carries their effect.
pub fn modifier_vk(text: &str) -> Option<u8> {
    let mut it = text.chars();
    let ch = it.next()?;
    if it.next().is_some() {
        return None;
    }
    match ch {
        '\u{0010}' | '\u{0015}' => Some(VK_SHIFT),   // Shift, ShiftR
        '\u{0011}' | '\u{0016}' => Some(VK_CONTROL), // Control, ControlR
        '\u{0012}' => Some(VK_MENU),                 // Alt
        _ => None,
    }
}

/// Slint key codes that never produce text, mapped to Win32 VK codes.
pub fn special_vk(text: &str) -> Option<u8> {
    let mut it = text.chars();
    let ch = it.next()?;
    if it.next().is_some() {
        return None;
    }
    Some(match ch {
        '\u{0008}' => 0x08,                       // Backspace
        '\u{0009}' | '\u{0019}' => 0x09,          // Tab, Backtab (Shift+Tab)
        '\r' | '\n' => 0x0D,                      // Enter
        '\u{001b}' => 0x1B,                       // Escape
        ' ' => 0x20,                              // Space (as a key so Ctrl/Shift+Space and holds work)
        '\u{007f}' => 0x2E,                       // Delete
        '\u{f700}' => 0x26,                       // UpArrow
        '\u{f701}' => 0x28,                       // DownArrow
        '\u{f702}' => 0x25,                       // LeftArrow
        '\u{f703}' => 0x27,                       // RightArrow
        c @ '\u{f704}'..='\u{f71b}' => 0x70 + (c as u32 - 0xf704) as u8, // F1..F24
        '\u{f727}' => 0x2D,                       // Insert
        '\u{f729}' => 0x24,                       // Home
        '\u{f72b}' => 0x23,                       // End
        '\u{f72c}' => 0x21,                       // PageUp
        '\u{f72d}' => 0x22,                       // PageDown
        '\u{f730}' => 0x13,                       // Pause
        '\u{f731}' => 0x2C,                       // SysReq / PrintScreen
        '\u{f735}' => 0x5D,                       // Menu (context menu key)
        _ => return None,
    })
}

/// VK for a printable character without asking the OS: letters and digits share their VK with
/// the ASCII upper-case code on every Windows layout.
pub fn ascii_vk(ch: char) -> Option<u8> {
    let up = ch.to_ascii_uppercase();
    up.is_ascii_alphanumeric().then_some(up as u8)
}

/// VK of the physical key that types `ch` on the viewer's current layout (Ctrl+'+', Ctrl+'-',
/// Ctrl+V on a Cyrillic layout...). Falls back to the ASCII rule.
fn char_vk(ch: char) -> Option<u8> {
    if let Some(vk) = ascii_vk(ch) {
        return Some(vk);
    }
    let mut buf = [0u16; 2];
    if ch.encode_utf16(&mut buf).len() != 1 {
        return None;
    }
    let r = unsafe { VkKeyScanW(buf[0]) };
    let vk = (r & 0xFF) as u8;
    (r != -1 && vk != 0 && vk != 0xFF).then_some(vk)
}

/// Modifier state Slint reports with the event.
#[derive(Clone, Copy, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum KeyOut {
    Down(u8),
    Up(u8),
    Text(String),
    /// First Ctrl+V press: the caller syncs the clipboard (or sends files) and then calls `paste_down`.
    Paste,
}

/// Keys this viewer currently holds down on the host.
#[derive(Default)]
pub struct KeyForwarder {
    held: Vec<u8>,
}

impl KeyForwarder {
    fn is_held(&self, vk: u8) -> bool {
        self.held.contains(&vk)
    }

    fn down(&mut self, vk: u8, out: &mut Vec<KeyOut>) {
        if !self.is_held(vk) {
            self.held.push(vk);
        }
        out.push(KeyOut::Down(vk));
    }

    fn up(&mut self, vk: u8, out: &mut Vec<KeyOut>) {
        if self.is_held(vk) {
            self.held.retain(|&k| k != vk);
            out.push(KeyOut::Up(vk));
        }
    }

    /// Ctrl/Alt held on the host turn printable keys into shortcuts instead of text.
    fn command_held(&self) -> bool {
        self.is_held(VK_CONTROL) || self.is_held(VK_MENU)
    }

    /// A modifier released while the canvas did not see it (e.g. focus moved) is released on the host too.
    fn reconcile(&mut self, m: Mods, out: &mut Vec<KeyOut>) {
        for (pressed, vk) in [(m.ctrl, VK_CONTROL), (m.alt, VK_MENU), (m.shift, VK_SHIFT)] {
            if !pressed {
                self.up(vk, out);
            }
        }
    }

    pub fn on_press(&mut self, text: &str, repeat: bool, m: Mods) -> Vec<KeyOut> {
        let mut out = Vec::new();
        if text.is_empty() {
            return out;
        }
        if let Some(vk) = modifier_vk(text) {
            // Auto-repeat of a held modifier adds nothing on the host.
            if !self.is_held(vk) {
                self.down(vk, &mut out);
            }
            return out;
        }
        if is_unforwarded(text) {
            return out;
        }
        self.reconcile(m, &mut out);
        if let Some(vk) = special_vk(text) {
            self.down(vk, &mut out);
            return out;
        }
        let mut chars = text.chars();
        let single = chars.next().filter(|_| chars.next().is_none());
        match single.filter(|_| self.command_held()).and_then(char_vk) {
            Some(VK_V) if self.is_held(VK_CONTROL) && !self.is_held(VK_MENU) => {
                if !repeat {
                    out.push(KeyOut::Paste);
                } else if self.is_held(VK_V) {
                    // Held Ctrl+V repeats like a local paste; after a file send nothing is held.
                    out.push(KeyOut::Down(VK_V));
                }
            }
            Some(vk) => self.down(vk, &mut out),
            None => out.push(KeyOut::Text(text.to_string())),
        }
        out
    }

    /// Called after a `Paste` once the clipboard text was synced, so the host pastes it.
    pub fn paste_down(&mut self) -> Vec<KeyOut> {
        let mut out = Vec::new();
        self.down(VK_V, &mut out);
        out
    }

    pub fn on_release(&mut self, text: &str, m: Mods) -> Vec<KeyOut> {
        let mut out = Vec::new();
        if let Some(vk) = modifier_vk(text) {
            self.up(vk, &mut out);
            return out;
        }
        if text.is_empty() || is_unforwarded(text) {
            return out;
        }
        self.reconcile(m, &mut out);
        let mut chars = text.chars();
        let vk = special_vk(text).or_else(|| chars.next().filter(|_| chars.next().is_none()).and_then(char_vk));
        if let Some(vk) = vk {
            // Keys typed as text were never pressed on the host, so they have nothing to release.
            self.up(vk, &mut out);
        }
        out
    }

    /// Releases everything (focus lost, window deactivated). Non-modifiers first, like a real hand.
    pub fn release_all(&mut self) -> Vec<KeyOut> {
        let mut keys = std::mem::take(&mut self.held);
        keys.sort_by_key(|&k| matches!(k, VK_SHIFT | VK_CONTROL | VK_MENU));
        keys.into_iter().map(KeyOut::Up).collect()
    }

    /// Forgets held keys without sending anything (the session they belonged to is gone).
    pub fn reset(&mut self) {
        self.held.clear();
    }
}

/// Keys the canvas sees but deliberately does not forward (AltGr, CapsLock, Meta, ScrollLock).
fn is_unforwarded(text: &str) -> bool {
    matches!(text, "\u{0013}" | "\u{0014}" | "\u{0017}" | "\u{0018}" | "\u{f72f}")
}

// --- Host side -------------------------------------------------------------------------------

/// Keys injected as `keydown` and not yet released, so a session end can release them.
static HOST_HELD: Mutex<Vec<u8>> = Mutex::new(Vec::new());

pub fn host_key_down(injector: &InputInjector, vk: u8) {
    injector.key_down(vk);
    let mut held = HOST_HELD.lock().unwrap();
    if !held.contains(&vk) {
        held.push(vk);
    }
}

pub fn host_key_up(injector: &InputInjector, vk: u8) {
    injector.key_up(vk);
    HOST_HELD.lock().unwrap().retain(|&k| k != vk);
}

/// Releases every key a viewer left pressed (disconnect, lost relay, viewer crash).
pub fn host_release_all(injector: &InputInjector) {
    let keys: Vec<u8> = std::mem::take(&mut *HOST_HELD.lock().unwrap());
    for vk in keys.into_iter().rev() {
        injector.key_up(vk);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Mods = Mods { ctrl: false, alt: false, shift: false };
    const CTRL: Mods = Mods { ctrl: true, alt: false, shift: false };
    const SHIFT: Mods = Mods { ctrl: false, alt: false, shift: true };

    #[test]
    fn vk_mapping_covers_modifiers_specials_and_ascii() {
        assert_eq!(modifier_vk("\u{0011}"), Some(VK_CONTROL));
        assert_eq!(modifier_vk("\u{0016}"), Some(VK_CONTROL)); // ControlR, not Ctrl+V
        assert_eq!(modifier_vk("\u{0015}"), Some(VK_SHIFT));
        assert_eq!(modifier_vk("\u{0012}"), Some(VK_MENU));
        assert_eq!(modifier_vk("\u{0017}"), None); // Meta goes through the low-level hook
        assert_eq!(special_vk("\u{f702}"), Some(0x25));
        assert_eq!(special_vk("\n"), Some(0x0D));
        assert_eq!(special_vk("\u{0019}"), Some(0x09));
        assert_eq!(special_vk("\u{f704}"), Some(0x70)); // F1
        assert_eq!(special_vk("\u{f70f}"), Some(0x7B)); // F12
        assert_eq!(special_vk("\u{f71b}"), Some(0x87)); // F24
        assert_eq!(special_vk(" "), Some(0x20));
        assert_eq!(special_vk("a"), None);
        assert_eq!(special_vk("ab"), None);
        assert_eq!(ascii_vk('c'), Some(b'C'));
        assert_eq!(ascii_vk('7'), Some(b'7'));
        assert_eq!(ascii_vk('é'), None);
    }

    #[test]
    fn ctrl_click_hold_sends_real_key_state() {
        let mut f = KeyForwarder::default();
        assert_eq!(f.on_press("\u{0011}", false, CTRL), vec![KeyOut::Down(VK_CONTROL)]);
        assert!(f.on_press("\u{0011}", true, CTRL).is_empty()); // auto-repeat
        assert_eq!(f.on_release("\u{0011}", NONE), vec![KeyOut::Up(VK_CONTROL)]);
        assert!(f.on_release("\u{0011}", NONE).is_empty()); // never released twice
    }

    #[test]
    fn typing_stays_unicode_and_shortcuts_become_keys() {
        let mut f = KeyForwarder::default();
        assert_eq!(f.on_press("é", false, NONE), vec![KeyOut::Text("é".into())]);
        assert!(f.on_release("é", NONE).is_empty());
        // Shift + letter: Shift is held for real, the letter still goes as text.
        f.on_press("\u{0010}", false, SHIFT);
        assert_eq!(f.on_press("A", false, SHIFT), vec![KeyOut::Text("A".into())]);
        assert_eq!(f.on_press("\u{f702}", false, SHIFT), vec![KeyOut::Down(0x25)]);
        assert_eq!(f.on_release("\u{f702}", SHIFT), vec![KeyOut::Up(0x25)]);
        f.on_release("\u{0010}", NONE);
        // Ctrl + c: a real key, released even if Ctrl went up first.
        f.on_press("\u{0011}", false, CTRL);
        assert_eq!(f.on_press("c", false, CTRL), vec![KeyOut::Down(b'C')]);
        assert_eq!(f.on_release("\u{0011}", NONE), vec![KeyOut::Up(VK_CONTROL)]);
        assert_eq!(f.on_release("c", NONE), vec![KeyOut::Up(b'C')]);
    }

    #[test]
    fn ctrl_v_asks_for_paste_once() {
        let mut f = KeyForwarder::default();
        f.on_press("\u{0011}", false, CTRL);
        assert_eq!(f.on_press("v", false, CTRL), vec![KeyOut::Paste]);
        // Files were sent instead: repeats and the release do nothing.
        assert!(f.on_press("v", true, CTRL).is_empty());
        assert!(f.on_release("v", CTRL).is_empty());
        // Text paste: V goes down after the clipboard sync and comes back up.
        assert_eq!(f.on_press("v", false, CTRL), vec![KeyOut::Paste]);
        assert_eq!(f.paste_down(), vec![KeyOut::Down(VK_V)]);
        assert_eq!(f.on_press("v", true, CTRL), vec![KeyOut::Down(VK_V)]);
        assert_eq!(f.on_release("v", CTRL), vec![KeyOut::Up(VK_V)]);
    }

    #[test]
    fn nothing_stays_stuck() {
        let mut f = KeyForwarder::default();
        f.on_press("\u{0011}", false, CTRL);
        f.on_press("\u{0010}", false, Mods { shift: true, ..CTRL });
        f.on_press("\u{f701}", false, Mods { shift: true, ..CTRL });
        assert_eq!(f.release_all(), vec![KeyOut::Up(0x28), KeyOut::Up(VK_CONTROL), KeyOut::Up(VK_SHIFT)]);
        assert!(f.release_all().is_empty());
        // A Ctrl release the canvas missed is fixed by the next key event.
        f.on_press("\u{0011}", false, CTRL);
        assert_eq!(f.on_press("x", false, NONE), vec![KeyOut::Up(VK_CONTROL), KeyOut::Text("x".into())]);
        // AltGr/CapsLock are not forwarded; AltGr text is plain unicode.
        assert!(f.on_press("\u{0013}", false, NONE).is_empty());
        assert_eq!(f.on_press("@", false, Mods { ctrl: true, alt: true, shift: false }), vec![KeyOut::Text("@".into())]);
    }
}
