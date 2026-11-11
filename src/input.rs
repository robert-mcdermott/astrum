//! Re-encoding of semantic input events into the bytes a program inside a
//! pane expects, according to the modes that program has enabled.

use alacritty_terminal::term::TermMode;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

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

pub fn encode_key(key: &KeyEvent, mode: TermMode) -> Option<Vec<u8>> {
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
