//! Win32 virtual-key codes (the wire format of `keydown`/`keyup`) to X11 keysyms, and characters to keysyms.

/// The X keysym for Win32 virtual key `vk`, for keys a viewer can send. Letters map to their lower-case
/// keysym (the key, not the character), digits and OEM keys to their US-layout keysym.
pub fn vk_to_keysym(vk: u8) -> Option<u32> {
    Some(match vk {
        0x08 => 0xff08,                             // BackSpace
        0x09 => 0xff09,                             // Tab
        0x0C => 0xff0b,                             // Clear
        0x0D => 0xff0d,                             // Return
        0x10 | 0xA0 => 0xffe1,                      // Shift_L
        0xA1 => 0xffe2,                             // Shift_R
        0x11 | 0xA2 => 0xffe3,                      // Control_L
        0xA3 => 0xffe4,                             // Control_R
        0x12 | 0xA4 => 0xffe9,                      // Alt_L
        0xA5 => 0xffea,                             // Alt_R
        0x13 => 0xff13,                             // Pause
        0x14 => 0xffe5,                             // Caps_Lock
        0x1B => 0xff1b,                             // Escape
        0x20 => 0x0020,                             // space
        0x21 => 0xff55,                             // Prior (Page Up)
        0x22 => 0xff56,                             // Next (Page Down)
        0x23 => 0xff57,                             // End
        0x24 => 0xff50,                             // Home
        0x25 => 0xff51,                             // Left
        0x26 => 0xff52,                             // Up
        0x27 => 0xff53,                             // Right
        0x28 => 0xff54,                             // Down
        0x2C => 0xff61,                             // Print
        0x2D => 0xff63,                             // Insert
        0x2E => 0xffff,                             // Delete
        0x30..=0x39 => vk as u32,                   // 0-9
        0x41..=0x5A => vk as u32 + 0x20,            // a-z
        0x5B => 0xffeb,                             // Super_L
        0x5C => 0xffec,                             // Super_R
        0x5D => 0xff67,                             // Menu
        0x60..=0x69 => 0xffb0 + (vk - 0x60) as u32, // KP_0-KP_9
        0x6A => 0xffaa,                             // KP_Multiply
        0x6B => 0xffab,                             // KP_Add
        0x6D => 0xffad,                             // KP_Subtract
        0x6E => 0xffae,                             // KP_Decimal
        0x6F => 0xffaf,                             // KP_Divide
        0x70..=0x87 => 0xffbe + (vk - 0x70) as u32, // F1-F24
        0x90 => 0xff7f,                             // Num_Lock
        0x91 => 0xff14,                             // Scroll_Lock
        0xAD => 0x1008_ff12,                        // XF86AudioMute
        0xAE => 0x1008_ff11,                        // XF86AudioLowerVolume
        0xAF => 0x1008_ff13,                        // XF86AudioRaiseVolume
        0xB0 => 0x1008_ff17,                        // XF86AudioNext
        0xB1 => 0x1008_ff16,                        // XF86AudioPrev
        0xB3 => 0x1008_ff14,                        // XF86AudioPlay
        0xBA => 0x003b,                             // semicolon
        0xBB => 0x003d,                             // equal
        0xBC => 0x002c,                             // comma
        0xBD => 0x002d,                             // minus
        0xBE => 0x002e,                             // period
        0xBF => 0x002f,                             // slash
        0xC0 => 0x0060,                             // grave
        0xDB => 0x005b,                             // bracketleft
        0xDC => 0x005c,                             // backslash
        0xDD => 0x005d,                             // bracketright
        0xDE => 0x0027,                             // apostrophe
        0xE2 => 0x003c,                             // less (102nd key)
        _ => return None,
    })
}

/// The keysym that types `ch`: Latin-1 characters are their own keysym, everything else is Unicode
/// (`0x0100_0000 + code point`). Control characters map to their function keys.
pub fn char_to_keysym(ch: char) -> u32 {
    match ch {
        '\n' | '\r' => 0xff0d,
        '\t' => 0xff09,
        '\u{8}' => 0xff08,
        c if (' '..='~').contains(&c) || ('\u{a0}'..='\u{ff}').contains(&c) => c as u32,
        c => 0x0100_0000 + c as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_keys_map_to_keysyms() {
        assert_eq!(vk_to_keysym(b'A'), Some(0x61));
        assert_eq!(vk_to_keysym(b'7'), Some(0x37));
        assert_eq!(vk_to_keysym(0x0D), Some(0xff0d));
        assert_eq!(vk_to_keysym(0x5B), Some(0xffeb));
        assert_eq!(vk_to_keysym(0x70), Some(0xffbe));
        assert_eq!(vk_to_keysym(0x87), Some(0xffbe + 23));
        assert_eq!(vk_to_keysym(0xFF), None);
    }

    #[test]
    fn characters_map_to_latin1_or_unicode_keysyms() {
        assert_eq!(char_to_keysym('a'), 0x61);
        assert_eq!(char_to_keysym('é'), 0xe9);
        assert_eq!(char_to_keysym('€'), 0x0100_20ac);
        assert_eq!(char_to_keysym('\n'), 0xff0d);
    }
}
