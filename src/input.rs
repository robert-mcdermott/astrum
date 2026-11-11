//! Re-encoding of semantic input events into the bytes a program inside a
//! pane expects, according to the modes that program has enabled.

use alacritty_terminal::term::TermMode;
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MediaKeyCode, ModifierKeyCode, MouseButton, MouseEvent, MouseEventKind,
};

/// xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
fn mod_param(m: KeyModifiers) -> u8 {
    let mut p = 1;
    if m.contains(KeyModifiers::SHIFT) {
        p += 1;
    }
    if m.contains(KeyModifiers::ALT) {
        p += 2;
    }
    if m.contains(KeyModifiers::CONTROL) {
        p += 4;
    }
    p
}

fn ctrl_byte(c: char) -> Option<u8> {
    Some(match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        ' ' | '@' | '2' => 0x00,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '/' | '7' | '-' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}

/// A key for a program that asked for enhanced key reports (the "kitty
/// keyboard protocol"): keys that the plain encoding cannot tell apart, or
/// cannot express at all, are sent as `CSI number ; modifiers u`.
///
/// What the program asked for is in `mode`: to disambiguate, to hear of
/// repeats and releases, to get every key as an escape code, and to get the
/// text a key produces alongside it.
fn encode_enhanced(key: &KeyEvent, mode: TermMode) -> Option<Vec<u8>> {
    let all = mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC);
    let events = mode.contains(TermMode::REPORT_EVENT_TYPES);
    let with_text = all && mode.contains(TermMode::REPORT_ASSOCIATED_TEXT);
    let release = key.kind == KeyEventKind::Release;
    if release && !events {
        return None;
    }

    let m = key.modifiers;
    let mut bits = 0u16;
    for (flag, bit) in [
        (KeyModifiers::SHIFT, 1),
        (KeyModifiers::ALT, 2),
        (KeyModifiers::CONTROL, 4),
        (KeyModifiers::SUPER, 8),
        (KeyModifiers::HYPER, 16),
        (KeyModifiers::META, 32),
    ] {
        if m.contains(flag) {
            bits |= bit;
        }
    }
    // Only Shift held, or nothing: the key still means its own text.
    let texty = bits & !1 == 0;
    let mut lock_bits = 0;
    if key.state.contains(KeyEventState::CAPS_LOCK) {
        lock_bits |= 64;
    }
    if key.state.contains(KeyEventState::NUM_LOCK) {
        lock_bits |= 128;
    }
    // The modifier field: one more than the bits, then the kind of event
    // where that was asked for and is not a plain press.
    let event = match key.kind {
        KeyEventKind::Press => 1,
        KeyEventKind::Repeat => 2,
        KeyEventKind::Release => 3,
    };
    let field = |bits: u16| -> String {
        let value = bits | lock_bits;
        match (events && event != 1, value) {
            (true, _) => format!("{}:{event}", value + 1),
            (false, 0) => String::new(),
            (false, _) => (value + 1).to_string(),
        }
    };
    // CSI number [; modifiers [; text]] final
    let csi = |number: u32, bits: u16, text: Option<u32>, last: char| -> Vec<u8> {
        let mods = field(bits);
        let mut out = format!("\x1b[{number}");
        match (mods.is_empty(), text) {
            (_, Some(t)) => out.push_str(&format!(";{mods};{t}")),
            (false, None) => out.push_str(&format!(";{mods}")),
            (true, None) => {}
        }
        out.push(last);
        out.into_bytes()
    };
    // Keys that keep their old form, with the modifiers as a parameter:
    // CSI 1 ; modifiers LETTER, or plain CSI LETTER (SS3 in application mode).
    let lettered = |letter: char| -> Vec<u8> {
        let mods = field(bits);
        if mods.is_empty() {
            let lead = if mode.contains(TermMode::APP_CURSOR) { "\x1bO" } else { "\x1b[" };
            format!("{lead}{letter}").into_bytes()
        } else {
            format!("\x1b[1;{mods}{letter}").into_bytes()
        }
    };

    // Keys that are plain bytes unless something has to be said about them.
    let legacy = |byte: u8, number: u32| -> Option<Vec<u8>> {
        if !all && bits == 0 {
            return (!release).then(|| vec![byte]);
        }
        Some(csi(number, bits, None, 'u'))
    };

    Some(match key.code {
        KeyCode::Char(c) => {
            if !all && texty {
                // Ordinary typing stays ordinary text; its release is not reported.
                if release {
                    return None;
                }
                let mut buf = [0u8; 4];
                return Some(c.encode_utf8(&mut buf).as_bytes().to_vec());
            }
            // The key is named by its unshifted character.
            let base = c.to_lowercase().next().unwrap_or(c);
            let mut bits = bits;
            if c.is_uppercase() && base != c {
                bits |= 1;
            }
            let text = (with_text && texty && !release).then_some(c as u32);
            let mods = field(bits);
            let mut out = format!("\x1b[{}", base as u32);
            match (mods.is_empty(), text) {
                (_, Some(t)) => out.push_str(&format!(";{mods};{t}")),
                (false, None) => out.push_str(&format!(";{mods}")),
                (true, None) => {}
            }
            out.push('u');
            out.into_bytes()
        }
        KeyCode::Enter => legacy(b'\r', 13)?,
        KeyCode::Tab => legacy(b'\t', 9)?,
        KeyCode::BackTab => csi(9, bits | 1, None, 'u'),
        KeyCode::Backspace => legacy(0x7f, 127)?,
        KeyCode::Esc => csi(27, bits, None, 'u'),
        KeyCode::Up => lettered('A'),
        KeyCode::Down => lettered('B'),
        KeyCode::Right => lettered('C'),
        KeyCode::Left => lettered('D'),
        KeyCode::Home => lettered('H'),
        KeyCode::End => lettered('F'),
        KeyCode::KeypadBegin => lettered('E'),
        KeyCode::Insert => csi(2, bits, None, '~'),
        KeyCode::Delete => csi(3, bits, None, '~'),
        KeyCode::PageUp => csi(5, bits, None, '~'),
        KeyCode::PageDown => csi(6, bits, None, '~'),
        KeyCode::F(1) => lettered('P'),
        KeyCode::F(2) => lettered('Q'),
        KeyCode::F(3) => csi(13, bits, None, '~'),
        KeyCode::F(4) => lettered('S'),
        KeyCode::F(n @ 5..=12) => csi([15, 17, 18, 19, 20, 21, 23, 24][(n - 5) as usize], bits, None, '~'),
        KeyCode::F(n @ 13..=35) => csi(57376 + (n as u32 - 13), bits, None, 'u'),
        KeyCode::PrintScreen => csi(57361, bits, None, 'u'),
        KeyCode::Pause => csi(57362, bits, None, 'u'),
        KeyCode::Menu => csi(57363, bits, None, 'u'),
        // Lock keys and the modifier keys themselves are only of interest
        // to a program that wants every key.
        KeyCode::CapsLock if all => csi(57358, bits, None, 'u'),
        KeyCode::ScrollLock if all => csi(57359, bits, None, 'u'),
        KeyCode::NumLock if all => csi(57360, bits, None, 'u'),
        KeyCode::Modifier(which) if all => {
            let number = match which {
                ModifierKeyCode::LeftShift => 57441,
                ModifierKeyCode::LeftControl => 57442,
                ModifierKeyCode::LeftAlt => 57443,
                ModifierKeyCode::LeftSuper => 57444,
                ModifierKeyCode::LeftHyper => 57445,
                ModifierKeyCode::LeftMeta => 57446,
                ModifierKeyCode::RightShift => 57447,
                ModifierKeyCode::RightControl => 57448,
                ModifierKeyCode::RightAlt => 57449,
                ModifierKeyCode::RightSuper => 57450,
                ModifierKeyCode::RightHyper => 57451,
                ModifierKeyCode::RightMeta => 57452,
                ModifierKeyCode::IsoLevel3Shift => 57453,
                ModifierKeyCode::IsoLevel5Shift => 57454,
            };
            csi(number, bits, None, 'u')
        }
        KeyCode::Media(which) => {
            let number = match which {
                MediaKeyCode::Play => 57428,
                MediaKeyCode::Pause => 57429,
                MediaKeyCode::PlayPause => 57430,
                MediaKeyCode::Reverse => 57431,
                MediaKeyCode::Stop => 57432,
                MediaKeyCode::FastForward => 57433,
                MediaKeyCode::Rewind => 57434,
                MediaKeyCode::TrackNext => 57435,
                MediaKeyCode::TrackPrevious => 57436,
                MediaKeyCode::Record => 57437,
                MediaKeyCode::LowerVolume => 57438,
                MediaKeyCode::RaiseVolume => 57439,
                MediaKeyCode::MuteVolume => 57440,
            };
            csi(number, bits, None, 'u')
        }
        _ => return None,
    })
}

pub fn encode_key(key: &KeyEvent, mode: TermMode) -> Option<Vec<u8>> {
    if mode.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL) {
        return encode_enhanced(key, mode);
    }
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let mods = key.modifiers & (KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL);
    let alt = mods.contains(KeyModifiers::ALT);
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let shift = mods.contains(KeyModifiers::SHIFT);
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let mut out = Vec::with_capacity(8);

    // CSI/SS3 keys that take the xterm modifier parameter.
    let csi_letter = |out: &mut Vec<u8>, letter: u8| {
        if mods.is_empty() {
            out.extend_from_slice(if app_cursor { b"\x1bO" } else { b"\x1b[" });
            out.push(letter);
        } else {
            out.extend_from_slice(format!("\x1b[1;{}", mod_param(mods)).as_bytes());
            out.push(letter);
        }
    };
    let csi_tilde = |out: &mut Vec<u8>, n: u8| {
        if mods.is_empty() {
            out.extend_from_slice(format!("\x1b[{n}~").as_bytes());
        } else {
            out.extend_from_slice(format!("\x1b[{n};{}~", mod_param(mods)).as_bytes());
        }
    };

    match key.code {
        KeyCode::Char(c) => {
            if alt {
                out.push(0x1b);
            }
            if ctrl {
                match ctrl_byte(c) {
                    Some(b) => out.push(b),
                    None => {
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                    }
                }
            } else {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
        KeyCode::Enter => {
            if alt {
                out.push(0x1b);
            }
            out.push(b'\r');
        }
        KeyCode::Tab => {
            if shift {
                out.extend_from_slice(b"\x1b[Z");
            } else {
                if alt {
                    out.push(0x1b);
                }
                out.push(b'\t');
            }
        }
        KeyCode::BackTab => out.extend_from_slice(b"\x1b[Z"),
        KeyCode::Backspace => {
            if alt {
                out.push(0x1b);
            }
            out.push(if ctrl { 0x08 } else { 0x7f });
        }
        KeyCode::Esc => {
            if alt {
                out.push(0x1b);
            }
            out.push(0x1b);
        }
        KeyCode::Up => csi_letter(&mut out, b'A'),
        KeyCode::Down => csi_letter(&mut out, b'B'),
        KeyCode::Right => csi_letter(&mut out, b'C'),
        KeyCode::Left => csi_letter(&mut out, b'D'),
        KeyCode::Home => csi_letter(&mut out, b'H'),
        KeyCode::End => csi_letter(&mut out, b'F'),
        KeyCode::Insert => csi_tilde(&mut out, 2),
        KeyCode::Delete => csi_tilde(&mut out, 3),
        KeyCode::PageUp => csi_tilde(&mut out, 5),
        KeyCode::PageDown => csi_tilde(&mut out, 6),
        KeyCode::F(n @ 1..=4) => {
            let letter = b'P' + (n - 1);
            if mods.is_empty() {
                out.extend_from_slice(b"\x1bO");
                out.push(letter);
            } else {
                out.extend_from_slice(format!("\x1b[1;{}", mod_param(mods)).as_bytes());
                out.push(letter);
            }
        }
        KeyCode::F(n @ 5..=12) => {
            let code = [15, 17, 18, 19, 20, 21, 23, 24][(n - 5) as usize];
            csi_tilde(&mut out, code);
        }
        _ => return None,
    }
    Some(out)
}

/// Encode pasted text, bracketed when the program asked for it.
pub fn encode_paste(text: &str, mode: TermMode) -> Vec<u8> {
    if mode.contains(TermMode::BRACKETED_PASTE) {
        // Never let the payload terminate the bracket early.
        let clean = text.replace("\x1b[201~", "");
        let mut out = Vec::with_capacity(clean.len() + 12);
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(clean.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// Does the program in the pane want this kind of mouse event?
pub fn wants_mouse(mode: TermMode) -> bool {
    mode.intersects(TermMode::MOUSE_MODE)
}

/// Encode a mouse event at pane-local cell (col, row), both zero-based.
pub fn encode_mouse(ev: &MouseEvent, col: u16, row: u16, mode: TermMode) -> Option<Vec<u8>> {
    let (mut code, release, motion) = match ev.kind {
        MouseEventKind::Down(b) => (button_code(b), false, false),
        MouseEventKind::Up(b) => (button_code(b), true, false),
        MouseEventKind::Drag(b) => (button_code(b) + 32, false, true),
        MouseEventKind::Moved => (3 + 32, false, true),
        MouseEventKind::ScrollUp => (64, false, false),
        MouseEventKind::ScrollDown => (65, false, false),
        MouseEventKind::ScrollLeft => (66, false, false),
        MouseEventKind::ScrollRight => (67, false, false),
    };
    if motion {
        let drag = matches!(ev.kind, MouseEventKind::Drag(_));
        let ok = mode.contains(TermMode::MOUSE_MOTION) || (drag && mode.contains(TermMode::MOUSE_DRAG));
        if !ok {
            return None;
        }
    }
    if ev.modifiers.contains(KeyModifiers::SHIFT) {
        code += 4;
    }
    if ev.modifiers.contains(KeyModifiers::ALT) {
        code += 8;
    }
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        code += 16;
    }
    if mode.contains(TermMode::SGR_MOUSE) {
        let fin = if release { 'm' } else { 'M' };
        Some(format!("\x1b[<{};{};{}{}", code, col + 1, row + 1, fin).into_bytes())
    } else {
        // Legacy X10 encoding; coordinates above 223 cannot be represented.
        if col >= 223 || row >= 223 {
            return None;
        }
        let code = if release { 3 + (code & !3) } else { code };
        Some(vec![0x1b, b'[', b'M', 32 + code as u8, 32 + col as u8 + 1, 32 + row as u8 + 1])
    }
}

fn button_code(b: MouseButton) -> u16 {
    match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn plain_and_control() {
        let m = TermMode::default();
        assert_eq!(encode_key(&k(KeyCode::Char('a'), KeyModifiers::NONE), m).unwrap(), b"a");
        assert_eq!(encode_key(&k(KeyCode::Char('c'), KeyModifiers::CONTROL), m).unwrap(), [0x03]);
        assert_eq!(encode_key(&k(KeyCode::Char('b'), KeyModifiers::ALT), m).unwrap(), b"\x1bb");
        assert_eq!(encode_key(&k(KeyCode::Char('é'), KeyModifiers::NONE), m).unwrap(), "é".as_bytes());
        assert_eq!(encode_key(&k(KeyCode::Backspace, KeyModifiers::NONE), m).unwrap(), [0x7f]);
        assert_eq!(encode_key(&k(KeyCode::Enter, KeyModifiers::SHIFT), m).unwrap(), b"\r");
    }

    #[test]
    fn enhanced_reports_say_what_plain_bytes_cannot() {
        let one = TermMode::DISAMBIGUATE_ESC_CODES;
        let enc = |code, mods, mode| String::from_utf8(encode_key(&k(code, mods), mode).unwrap()).unwrap();
        // Ordinary typing is still text; so are Enter, Tab and Backspace by themselves.
        assert_eq!(enc(KeyCode::Char('a'), KeyModifiers::NONE, one), "a");
        assert_eq!(enc(KeyCode::Char('A'), KeyModifiers::SHIFT, one), "A");
        assert_eq!(enc(KeyCode::Enter, KeyModifiers::NONE, one), "\r");
        assert_eq!(enc(KeyCode::Tab, KeyModifiers::NONE, one), "\t");
        assert_eq!(enc(KeyCode::Backspace, KeyModifiers::NONE, one), "\x7f");
        // What used to be ambiguous is spelled out: Ctrl+I is not Tab,
        // Shift+Enter is not Enter, Escape is not the start of a sequence.
        assert_eq!(enc(KeyCode::Char('i'), KeyModifiers::CONTROL, one), "\x1b[105;5u");
        assert_eq!(enc(KeyCode::Enter, KeyModifiers::SHIFT, one), "\x1b[13;2u");
        assert_eq!(enc(KeyCode::Tab, KeyModifiers::CONTROL, one), "\x1b[9;5u");
        assert_eq!(enc(KeyCode::BackTab, KeyModifiers::SHIFT, one), "\x1b[9;2u");
        assert_eq!(enc(KeyCode::Esc, KeyModifiers::NONE, one), "\x1b[27u");
        assert_eq!(enc(KeyCode::Char('b'), KeyModifiers::ALT, one), "\x1b[98;3u");
        assert_eq!(enc(KeyCode::Char('C'), KeyModifiers::CONTROL | KeyModifiers::SHIFT, one), "\x1b[99;6u");
        assert_eq!(enc(KeyCode::Char('1'), KeyModifiers::CONTROL | KeyModifiers::SHIFT, one), "\x1b[49;6u");
        assert_eq!(enc(KeyCode::Char('s'), KeyModifiers::SUPER, one), "\x1b[115;9u");
        // Keys that had a form keep it, with the modifiers as a parameter.
        assert_eq!(enc(KeyCode::Up, KeyModifiers::NONE, one), "\x1b[A");
        assert_eq!(enc(KeyCode::Up, KeyModifiers::NONE, one | TermMode::APP_CURSOR), "\x1bOA");
        assert_eq!(enc(KeyCode::Left, KeyModifiers::CONTROL | KeyModifiers::SHIFT, one), "\x1b[1;6D");
        assert_eq!(enc(KeyCode::Delete, KeyModifiers::SHIFT, one), "\x1b[3;2~");
        assert_eq!(enc(KeyCode::F(1), KeyModifiers::NONE, one), "\x1b[P");
        assert_eq!(enc(KeyCode::F(3), KeyModifiers::ALT, one), "\x1b[13;3~");
        assert_eq!(enc(KeyCode::F(5), KeyModifiers::NONE, one), "\x1b[15~");
        assert_eq!(enc(KeyCode::F(13), KeyModifiers::NONE, one), "\x1b[57376u");
        // A release is only reported when asked for, and not for plain typing.
        let release = |code, mods| KeyEvent::new_with_kind(code, mods, KeyEventKind::Release);
        assert_eq!(encode_key(&release(KeyCode::Char('i'), KeyModifiers::CONTROL), one), None);
        let two = one | TermMode::REPORT_EVENT_TYPES;
        assert_eq!(encode_key(&release(KeyCode::Char('i'), KeyModifiers::CONTROL), two).unwrap(), b"\x1b[105;5:3u");
        assert_eq!(encode_key(&release(KeyCode::Char('a'), KeyModifiers::NONE), two), None);
        assert_eq!(encode_key(&release(KeyCode::Up, KeyModifiers::NONE), two).unwrap(), b"\x1b[1;1:3A");
        let repeat = KeyEvent::new_with_kind(KeyCode::Esc, KeyModifiers::NONE, KeyEventKind::Repeat);
        assert_eq!(encode_key(&repeat, two).unwrap(), b"\x1b[27;1:2u");
        // Every key as an escape code, with the text it stands for.
        let every = one | TermMode::REPORT_ALL_KEYS_AS_ESC;
        assert_eq!(enc(KeyCode::Char('a'), KeyModifiers::NONE, every), "\x1b[97u");
        assert_eq!(enc(KeyCode::Char('A'), KeyModifiers::SHIFT, every), "\x1b[97;2u");
        assert_eq!(enc(KeyCode::Enter, KeyModifiers::NONE, every), "\x1b[13u");
        assert_eq!(encode_key(&release(KeyCode::Char('a'), KeyModifiers::NONE), every | TermMode::REPORT_EVENT_TYPES).unwrap(), b"\x1b[97;1:3u");
        let texted = every | TermMode::REPORT_ASSOCIATED_TEXT;
        assert_eq!(enc(KeyCode::Char('a'), KeyModifiers::NONE, texted), "\x1b[97;;97u");
        assert_eq!(enc(KeyCode::Char('A'), KeyModifiers::SHIFT, texted), "\x1b[97;2;65u");
        assert_eq!(enc(KeyCode::Char('a'), KeyModifiers::CONTROL, texted), "\x1b[97;5u");
        // The modifier keys themselves, for a program that wants every key.
        assert_eq!(encode_key(&k(KeyCode::Modifier(ModifierKeyCode::LeftShift), KeyModifiers::SHIFT), one), None);
        assert_eq!(enc(KeyCode::Modifier(ModifierKeyCode::LeftShift), KeyModifiers::SHIFT, every), "\x1b[57441;2u");
        // Without the program asking, nothing changes.
        assert_eq!(enc(KeyCode::Char('i'), KeyModifiers::CONTROL, TermMode::default()), "\t");
    }

    #[test]
    fn cursor_keys_follow_app_mode() {
        let up = k(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(encode_key(&up, TermMode::default()).unwrap(), b"\x1b[A");
        assert_eq!(encode_key(&up, TermMode::APP_CURSOR).unwrap(), b"\x1bOA");
        let ctrl_right = k(KeyCode::Right, KeyModifiers::CONTROL);
        assert_eq!(encode_key(&ctrl_right, TermMode::APP_CURSOR).unwrap(), b"\x1b[1;5C");
        assert_eq!(encode_key(&k(KeyCode::Delete, KeyModifiers::NONE), TermMode::default()).unwrap(), b"\x1b[3~");
        assert_eq!(encode_key(&k(KeyCode::F(5), KeyModifiers::NONE), TermMode::default()).unwrap(), b"\x1b[15~");
    }

    #[test]
    fn paste_and_mouse() {
        assert_eq!(encode_paste("a\nb", TermMode::default()), b"a\rb");
        assert_eq!(encode_paste("x", TermMode::BRACKETED_PASTE), b"\x1b[200~x\x1b[201~");
        let ev = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(encode_mouse(&ev, 4, 2, mode).unwrap(), b"\x1b[<0;5;3M");
    }
}
