//! Pictures that programs print.
//!
//! A program shows a picture by writing it to the terminal in one of three
//! unrelated ways: iTerm2's (a file, in an OSC 1337 sequence), kitty's
//! (pixels or a PNG, in APC sequences, with commands to place and delete)
//! and sixel (a DCS sequence of six-pixel-high bands). The emulator Astrum
//! uses drops all three. So they are taken out of the stream before it,
//! here; the pane keeps each picture and marks the cells it covers (see
//! `PLACEHOLDER`); and when a frame is drawn, a picture whose cells are all
//! on screen is sent to each terminal in a way that terminal understands.
//!
//! Nothing here decodes a picture. A file is passed on as the file it is,
//! and raw pixels are wrapped in a PNG without being compressed, so what
//! can be shown where depends on the pair: see `Image::how`.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use std::ops::Range;

/// The character in every cell a picture covers. It is in a private-use
/// area no font or program is known to use. The cell's colors are not
/// colors: the text color is the picture's number and the background is
/// the cell's row and column within the picture (see `mark` and `unmark`).
/// So the emulator carries pictures along as it scrolls, clears, wraps and
/// keeps history, and nothing has to follow them separately.
pub const PLACEHOLDER: char = '\u{10FFFD}';

/// The most a single picture may take, as it arrives.
pub const MAX_PICTURE: usize = 24 * 1024 * 1024;
/// The most one pane keeps, and how many.
pub const MAX_KEPT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_KEPT: usize = 96;

/// Which of the three ways a terminal understands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caps {
    #[serde(default)]
    pub kitty: bool,
    #[serde(default)]
    pub iterm: bool,
    #[serde(default)]
    pub sixel: bool,
}

impl Caps {
    pub fn any(self) -> bool {
        self.kitty || self.iterm || self.sixel
    }

    /// From a list such as `kitty,sixel`; `none` (or nothing known) is none.
    pub fn named(list: &str) -> Caps {
        let has = |name: &str| list.split(',').any(|part| part.trim().eq_ignore_ascii_case(name));
        Caps { kitty: has("kitty"), iterm: has("iterm"), sixel: has("sixel") }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Png,
    Jpeg,
    Gif,
    Other,
}

impl FileKind {
    pub fn name(self) -> &'static str {
        match self {
            FileKind::Png => "PNG",
            FileKind::Jpeg => "JPEG",
            FileKind::Gif => "GIF",
            FileKind::Other => "image",
        }
    }
}

/// A picture as it was sent, still in base64 where it came that way.
pub enum Data {
    /// A whole image file.
    File { b64: Vec<u8>, kind: FileKind },
    /// Raw pixels, three or four bytes each, possibly zlib-compressed.
    Pixels { b64: Vec<u8>, alpha: bool, compressed: bool },
    /// The inside of a sixel sequence: its parameters, `q`, and the bands.
    Sixel { body: Vec<u8> },
}

pub struct Image {
    /// Astrum's own number for it, at most 24 bits (it is kept in a color).
    pub id: u32,
    pub data: std::sync::Arc<Data>,
    /// Its size in pixels; (0, 0) when the file did not say in a way known here.
    pub px: (u32, u32),
    /// The cells it covers.
    pub cells: (u16, u16),
    /// Sixel cannot be scaled: it fits its cells only on a terminal whose
    /// cells are the size the picture was measured against.
    pub cell_px: (u16, u16),
}

/// How a picture can be sent to a terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    Kitty,
    Iterm,
    Sixel,
}

impl Data {
    pub fn bytes(&self) -> usize {
        match self {
            Data::File { b64, .. } | Data::Pixels { b64, .. } => b64.len(),
            Data::Sixel { body } => body.len(),
        }
    }
}

impl Image {
    pub fn bytes(&self) -> usize {
        self.data.bytes()
    }

    /// What a line of text says about it, where it cannot be shown.
    pub fn label(&self) -> String {
        let what = match &*self.data {
            Data::File { kind, .. } => kind.name(),
            Data::Pixels { .. } => "image",
            Data::Sixel { .. } => "sixel image",
        };
        if self.px.0 > 0 && self.px.1 > 0 { format!("{what} {}×{}", self.px.0, self.px.1) } else { what.to_string() }
    }

    /// The way to show this picture on a terminal that understands `caps`,
    /// if there is one. A file can go to kitty's protocol only as a PNG;
    /// raw pixels can go to iTerm2's only uncompressed (they are wrapped in
    /// a PNG); sixel can only stay sixel.
    pub fn how(&self, caps: Caps) -> Option<How> {
        match &*self.data {
            Data::File { kind, .. } => {
                if caps.iterm {
                    Some(How::Iterm)
                } else if caps.kitty && *kind == FileKind::Png {
                    Some(How::Kitty)
                } else {
                    None
                }
            }
            Data::Pixels { compressed, .. } => {
                if caps.kitty {
                    Some(How::Kitty)
                } else if caps.iterm && !*compressed {
                    Some(How::Iterm)
                } else {
                    None
                }
            }
            Data::Sixel { .. } => caps.sixel.then_some(How::Sixel),
        }
    }

    /// The bytes that draw the picture where the cursor is, in iTerm2's way.
    pub fn iterm(&self, out: &mut Vec<u8>) {
        let (cols, rows) = self.cells;
        let head = format!("\x1b]1337;File=inline=1;width={cols};height={rows};preserveAspectRatio=1:");
        match &*self.data {
            Data::File { b64, .. } => {
                out.extend_from_slice(head.as_bytes());
                out.extend_from_slice(b64);
                out.push(0x07);
            }
            Data::Pixels { b64, alpha, compressed: false } => {
                let Ok(raw) = decode(b64) else { return };
                let Some(png) = png_from_pixels(&raw, self.px, *alpha) else { return };
                out.extend_from_slice(head.as_bytes());
                out.extend_from_slice(STANDARD.encode(png).as_bytes());
                out.push(0x07);
            }
            _ => {}
        }
    }

    /// The bytes that hand the picture to a terminal in kitty's way, to be
    /// placed afterwards with `kitty_place`.
    pub fn kitty_send(&self, out: &mut Vec<u8>) {
        let (b64, keys) = match &*self.data {
            Data::File { b64, kind: FileKind::Png } => (b64, "f=100".to_string()),
            Data::Pixels { b64, alpha, compressed } => {
                let format = if *alpha { 32 } else { 24 };
                let packed = if *compressed { ",o=z" } else { "" };
                (b64, format!("f={format},s={},v={}{packed}", self.px.0, self.px.1))
            }
            _ => return,
        };
        // In pieces, as the protocol asks; each is whole groups of four.
        let mut chunks = b64.chunks(4096).peekable();
        let mut first = true;
        while let Some(chunk) = chunks.next() {
            let more = u8::from(chunks.peek().is_some());
            out.extend_from_slice(b"\x1b_G");
            if first {
                out.extend_from_slice(format!("a=t,i={},{keys},q=2,", self.id).as_bytes());
                first = false;
            }
            out.extend_from_slice(format!("m={more};").as_bytes());
            out.extend_from_slice(chunk);
            out.extend_from_slice(b"\x1b\\");
        }
    }

    /// Put a picture already sent where the cursor is, over these cells.
    pub fn kitty_place(&self, placement: u32, out: &mut Vec<u8>) {
        let (cols, rows) = self.cells;
        out.extend_from_slice(format!("\x1b_Ga=p,i={},p={placement},c={cols},r={rows},C=1,q=2\x1b\\", self.id).as_bytes());
    }

    /// The bytes that draw the picture where the cursor is, as sixel.
    pub fn sixel(&self, out: &mut Vec<u8>) {
        if let Data::Sixel { body } = &*self.data {
            out.extend_from_slice(b"\x1bP");
            out.extend_from_slice(body);
            out.extend_from_slice(b"\x1b\\");
        }
    }
}

/// Take one placement of a picture off a kitty terminal's screen.
pub fn kitty_unplace(id: u32, placement: u32, out: &mut Vec<u8>) {
    out.extend_from_slice(format!("\x1b_Ga=d,d=i,i={id},p={placement},q=2\x1b\\").as_bytes());
}

/// Have a kitty terminal forget a picture altogether.
pub fn kitty_forget(id: u32, out: &mut Vec<u8>) {
    out.extend_from_slice(format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\").as_bytes());
}

/// Have a kitty terminal forget every picture.
pub fn kitty_forget_all(out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b_Ga=d,d=A,q=2\x1b\\");
}

/// A number for a new picture: never zero, and never more than 24 bits.
pub fn next_id() -> u32 {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 0xff_fffe + 1
}

// ---- the marks in the grid ---------------------------------------------------

/// The two "colors" of the cell at `row`, `col` of picture `id`.
pub fn mark(id: u32, row: u16, col: u16) -> ([u8; 3], [u8; 3]) {
    let (row, col) = (row.min(0xfff), col.min(0xfff));
    ([(id >> 16) as u8, (id >> 8) as u8, id as u8], [(row >> 4) as u8, ((row & 0xf) << 4) as u8 | (col >> 8) as u8, col as u8])
}

/// The picture, row and column a marked cell's "colors" stand for.
pub fn unmark(fg: [u8; 3], bg: [u8; 3]) -> (u32, u16, u16) {
    let id = (fg[0] as u32) << 16 | (fg[1] as u32) << 8 | fg[2] as u32;
    let row = (bg[0] as u16) << 4 | (bg[1] as u16) >> 4;
    let col = ((bg[1] & 0xf) as u16) << 8 | bg[2] as u16;
    (id, row, col)
}

// ---- taking pictures out of the stream ---------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    /// iTerm2's, whole in one sequence.
    Iterm,
    /// iTerm2's in several: what the file is, pieces of it, and the end.
    ItermBegin,
    ItermPart,
    ItermEnd,
    Kitty,
    Sixel,
}

impl Protocol {
    /// Its sequences are OSC ones, which may end with a bell.
    fn osc(self) -> bool {
        !matches!(self, Protocol::Kitty | Protocol::Sixel)
    }
}

/// One stretch of what a program wrote.
#[derive(Debug, PartialEq)]
pub enum Piece {
    /// Ordinary output: this part of the bytes that were fed in.
    Text(Range<usize>),
    /// Ordinary output that had been held back to see whether it was the
    /// start of a picture, and was not.
    Held(Vec<u8>),
    /// A picture sequence, without its opening and closing bytes.
    Picture(Protocol, Vec<u8>),
    /// "How large is one cell, in pixels?" (`CSI 16 t`): asked by programs
    /// working out how big to draw, and not answered by the emulator.
    AsksCellSize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum St {
    #[default]
    Ground,
    Esc,
    /// After `ESC [`, while what follows could still be `16t`.
    Csi,
    OscHead,
    DcsHead,
    Apc,
    Body(Protocol),
    BodyEsc(Protocol),
    /// A picture too large to keep: its bytes are let go as they arrive.
    Drop(Protocol),
    DropEsc(Protocol),
}

/// How each of iTerm2's file sequences starts, after `ESC ]`.
/// What follows `ESC [` in the one such sequence looked for.
const CELL_SIZE: &[u8] = b"16t";

const ITERM_HEADS: [(&[u8], Protocol); 4] =
    [(b"1337;File=", Protocol::Iterm), (b"1337;MultipartFile=", Protocol::ItermBegin), (b"1337;FilePart=", Protocol::ItermPart), (b"1337;FileEnd", Protocol::ItermEnd)];

/// Splits a program's output into text and pictures, across reads.
#[derive(Default)]
pub struct Splitter {
    st: St,
    buf: Vec<u8>,
}

impl Splitter {
    pub fn feed(&mut self, data: &[u8]) -> Vec<Piece> {
        let mut pieces = Vec::new();
        // The usual case: nothing of interest anywhere.
        if self.st == St::Ground && !data.contains(&0x1b) {
            if !data.is_empty() {
                pieces.push(Piece::Text(0..data.len()));
            }
            return pieces;
        }
        let mut i = 0;
        while i < data.len() {
            match self.st {
                St::Ground => {
                    // Up to the next sequence that could be one looked for.
                    // Every other one (colors, cursor moves: nearly all of
                    // them) stays part of the text, with no stop made for it.
                    let mut end = i;
                    let mut next = St::Ground;
                    while let Some(n) = data[end..].iter().position(|&b| b == 0x1b) {
                        end += n;
                        let after = &data[end + 1..];
                        next = match after.first() {
                            None => St::Esc,
                            Some(b']' | b'P' | b'_') => St::Esc,
                            Some(b'[') if CELL_SIZE.starts_with(&after[1..after.len().min(1 + CELL_SIZE.len())]) => St::Esc,
                            Some(_) => St::Ground,
                        };
                        if next == St::Esc {
                            break;
                        }
                        end += 1;
                    }
                    if next == St::Ground {
                        end = data.len();
                    }
                    if end > i {
                        pieces.push(Piece::Text(i..end));
                    }
                    i = end;
                    if i < data.len() {
                        self.st = St::Esc;
                        i += 1;
                    }
                }
                St::Esc => match data[i] {
                    b']' => {
                        self.buf.clear();
                        self.st = St::OscHead;
                        i += 1;
                    }
                    b'P' => {
                        self.buf.clear();
                        self.st = St::DcsHead;
                        i += 1;
                    }
                    b'_' => {
                        self.st = St::Apc;
                        i += 1;
                    }
                    b'[' => {
                        self.buf.clear();
                        self.st = St::Csi;
                        i += 1;
                    }
                    // Anything else is the emulator's. (Another ESC is looked at again.)
                    _ => {
                        pieces.push(Piece::Held(vec![0x1b]));
                        self.st = St::Ground;
                    }
                },
                St::Csi => {
                    if data[i] == CELL_SIZE[self.buf.len()] {
                        self.buf.push(data[i]);
                        i += 1;
                        if self.buf.len() == CELL_SIZE.len() {
                            self.buf.clear();
                            pieces.push(Piece::AsksCellSize);
                            self.st = St::Ground;
                        }
                    } else {
                        self.give_back(b"\x1b[", &mut pieces);
                    }
                }
                St::OscHead => {
                    // Still the start of one of the sequences looked for?
                    let so_far = self.buf.len();
                    let goes_on = |head: &[u8]| head.len() > so_far && head[..so_far] == self.buf[..] && head[so_far] == data[i];
                    if ITERM_HEADS.iter().any(|(head, _)| goes_on(head)) {
                        self.buf.push(data[i]);
                        i += 1;
                        if let Some((_, protocol)) = ITERM_HEADS.iter().find(|(head, _)| *head == &self.buf[..]) {
                            self.buf.clear();
                            self.st = St::Body(*protocol);
                        }
                    } else {
                        self.give_back(b"\x1b]", &mut pieces);
                    }
                }
                St::DcsHead => match data[i] {
                    b'0'..=b'9' | b';' if self.buf.len() < 32 => {
                        self.buf.push(data[i]);
                        i += 1;
                    }
                    b'q' => {
                        self.buf.push(b'q');
                        i += 1;
                        self.st = St::Body(Protocol::Sixel);
                    }
                    _ => self.give_back(b"\x1bP", &mut pieces),
                },
                St::Apc => {
                    if data[i] == b'G' {
                        self.buf.clear();
                        self.st = St::Body(Protocol::Kitty);
                        i += 1;
                    } else {
                        self.give_back(b"\x1b_", &mut pieces);
                    }
                }
                St::Body(protocol) | St::Drop(protocol) => {
                    let keeping = matches!(self.st, St::Body(_));
                    let bell = protocol.osc();
                    let end = data[i..].iter().position(|&b| b == 0x1b || (bell && b == 0x07)).map_or(data.len(), |n| i + n);
                    if keeping {
                        if self.buf.len() + (end - i) > MAX_PICTURE {
                            self.buf.clear();
                            self.st = St::Drop(protocol);
                        } else {
                            self.buf.extend_from_slice(&data[i..end]);
                        }
                    }
                    i = end;
                    if i < data.len() {
                        let keeping = matches!(self.st, St::Body(_));
                        if data[i] == 0x07 {
                            self.finish(protocol, keeping, &mut pieces);
                        } else {
                            self.st = if keeping { St::BodyEsc(protocol) } else { St::DropEsc(protocol) };
                        }
                        i += 1;
                    }
                }
                St::BodyEsc(protocol) | St::DropEsc(protocol) => {
                    let keeping = matches!(self.st, St::BodyEsc(_));
                    if data[i] == b'\\' {
                        self.finish(protocol, keeping, &mut pieces);
                        i += 1;
                    } else {
                        // Cut short by another sequence: the picture is lost,
                        // and that sequence is looked at for what it is.
                        self.buf.clear();
                        self.st = St::Esc;
                    }
                }
            }
        }
        pieces
    }

    /// What was held back was not a picture after all.
    fn give_back(&mut self, opening: &[u8], pieces: &mut Vec<Piece>) {
        let mut held = opening.to_vec();
        held.append(&mut self.buf);
        pieces.push(Piece::Held(held));
        self.st = St::Ground;
    }

    fn finish(&mut self, protocol: Protocol, keeping: bool, pieces: &mut Vec<Piece>) {
        let body = std::mem::take(&mut self.buf);
        if keeping {
            pieces.push(Piece::Picture(protocol, body));
        }
        self.st = St::Ground;
    }
}

// ---- what a picture sequence says --------------------------------------------

/// A length a program asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dim {
    Auto,
    Cells(u32),
    Pixels(u32),
    Percent(u32),
}

fn dim(text: &str) -> Dim {
    let number = |digits: &str| digits.trim().parse::<u32>().ok().filter(|n| *n > 0);
    if let Some(n) = text.strip_suffix("px").and_then(number) {
        Dim::Pixels(n)
    } else if let Some(n) = text.strip_suffix('%').and_then(number) {
        Dim::Percent(n.min(100))
    } else {
        number(text).map_or(Dim::Auto, Dim::Cells)
    }
}

/// An iTerm2 file sequence that asks to be shown.
pub struct ItermFile {
    pub b64: Vec<u8>,
    pub kind: FileKind,
    pub px: (u32, u32),
    pub width: Dim,
    pub height: Dim,
}

/// Read `key=value;...:base64`. `None` for a file meant to be saved, not
/// shown, or one with nothing in it.
pub fn parse_iterm(body: &[u8]) -> Option<ItermFile> {
    let colon = body.iter().position(|&b| b == b':')?;
    let args = std::str::from_utf8(&body[..colon]).ok()?;
    let (mut inline, mut width, mut height) = (false, Dim::Auto, Dim::Auto);
    for (key, value) in args.split(';').filter_map(|arg| arg.split_once('=')) {
        match key {
            "inline" => inline = value == "1",
            "width" => width = dim(value),
            "height" => height = dim(value),
            _ => {}
        }
    }
    let b64: Vec<u8> = body[colon + 1..].iter().copied().filter(|b| !b.is_ascii_whitespace()).collect();
    if !inline || b64.is_empty() || !is_base64(&b64) {
        return None;
    }
    let (kind, px) = file_size(&b64);
    Some(ItermFile { b64, kind, px, width, height })
}

/// Whether this is base64 and nothing else. What a program sent is passed
/// on to the user's terminal inside a sequence of our own; it must not be
/// able to end that sequence early, or carry anything a terminal would act on.
pub fn is_base64(bytes: &[u8]) -> bool {
    bytes.iter().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
}

/// The same for the inside of a sixel sequence: printable characters only.
pub fn is_sixel(bytes: &[u8]) -> bool {
    bytes.iter().all(|b| matches!(b, 0x20..=0x7e | b'\r' | b'\n'))
}

/// One kitty graphics command: its keys, and what came after the `;`.
#[derive(Debug, Default, Clone)]
pub struct KittyCmd {
    keys: Vec<(u8, String)>,
    pub payload: Vec<u8>,
}

impl KittyCmd {
    pub fn parse(body: &[u8]) -> KittyCmd {
        let (control, payload) = match body.iter().position(|&b| b == b';') {
            Some(at) => (&body[..at], &body[at + 1..]),
            None => (body, &[][..]),
        };
        let keys = String::from_utf8_lossy(control)
            .split(',')
            .filter_map(|pair| pair.split_once('='))
            .filter(|(key, _)| key.len() == 1)
            .map(|(key, value)| (key.as_bytes()[0], value.to_string()))
            .collect();
        KittyCmd { keys, payload: payload.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect() }
    }

    pub fn text(&self, key: char) -> Option<&str> {
        self.keys.iter().find(|(k, _)| *k == key as u8).map(|(_, v)| v.as_str())
    }

    pub fn number(&self, key: char) -> u32 {
        self.text(key).and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    /// A key whose value is one letter, such as the action.
    pub fn letter(&self, key: char, default: char) -> char {
        self.text(key).and_then(|v| v.chars().next()).unwrap_or(default)
    }
}

/// The size in pixels of a sixel picture: what it declares, or failing
/// that what its bands come to.
pub fn sixel_size(body: &[u8]) -> (u32, u32) {
    let data = match body.iter().position(|&b| b == b'q') {
        Some(at) => &body[at + 1..],
        None => return (0, 0),
    };
    // "Pan;Pad;Ph;Pv straight after the introducer.
    if data.first() == Some(&b'"') {
        let end = data[1..].iter().position(|b| !(b.is_ascii_digit() || *b == b';')).map_or(data.len(), |n| n + 1);
        let numbers: Vec<u32> = String::from_utf8_lossy(&data[1..end]).split(';').map(|n| n.parse().unwrap_or(0)).collect();
        if let [_, _, w, h, ..] = numbers[..] {
            if w > 0 && h > 0 {
                return (w, h);
            }
        }
    }
    let (mut x, mut width, mut bands, mut any) = (0u32, 0u32, 0u32, false);
    let mut i = 0;
    while i < data.len() {
        match data[i] {
            b'!' => {
                // !<count><character>
                let digits = data[i + 1..].iter().take_while(|b| b.is_ascii_digit()).count();
                let count: u32 = String::from_utf8_lossy(&data[i + 1..i + 1 + digits]).parse().unwrap_or(1);
                i += digits + 1;
                if data.get(i).is_some_and(|b| (b'?'..=b'~').contains(b)) {
                    x = x.saturating_add(count);
                    any = true;
                }
            }
            b'#' | b'"' => {
                // A color, or attributes: numbers and semicolons to skip.
                i += data[i + 1..].iter().take_while(|b| b.is_ascii_digit() || **b == b';').count();
            }
            b'$' => {
                width = width.max(x);
                x = 0;
            }
            b'-' => {
                width = width.max(x);
                x = 0;
                bands += 1;
                any = false;
            }
            b'?'..=b'~' => {
                x = x.saturating_add(1);
                any = true;
            }
            _ => {}
        }
        i += 1;
    }
    width = width.max(x);
    (width, (bands + u32::from(any)) * 6)
}

// ---- sizes --------------------------------------------------------------------

fn decode(b64: &[u8]) -> Result<Vec<u8>, base64::DecodeError> {
    // Padding is not always sent.
    let trimmed = b64.iter().rposition(|&b| b != b'=').map_or(&b64[..0], |last| &b64[..=last]);
    base64::engine::general_purpose::STANDARD_NO_PAD.decode(trimmed)
}

/// What kind of file this is and its size in pixels, from its first bytes.
pub fn file_size(b64: &[u8]) -> (FileKind, (u32, u32)) {
    // Enough to get past the notes a camera puts before a JPEG's size.
    let take = b64.len().min(512 * 1024) / 4 * 4;
    let Ok(head) = decode(&b64[..take]) else { return (FileKind::Other, (0, 0)) };
    let be32 = |at: usize| head.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let be16 = |at: usize| head.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as u32);
    let le16 = |at: usize| head.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as u32);
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        return (FileKind::Png, (be32(16).unwrap_or(0), be32(20).unwrap_or(0)));
    }
    if head.starts_with(b"GIF8") {
        return (FileKind::Gif, (le16(6).unwrap_or(0), le16(8).unwrap_or(0)));
    }
    if head.starts_with(&[0xff, 0xd8]) {
        // Segments: FF, a marker, a two-byte length that counts itself. The
        // size is in the "start of frame" one.
        let mut at = 2;
        while let (Some(&0xff), Some(&marker)) = (head.get(at), head.get(at + 1)) {
            if marker == 0xff {
                at += 1;
                continue;
            }
            if (0xc0..=0xcf).contains(&marker) && ![0xc4, 0xc8, 0xcc].contains(&marker) {
                return (FileKind::Jpeg, (be16(at + 7).unwrap_or(0), be16(at + 5).unwrap_or(0)));
            }
            let Some(len) = be16(at + 2) else { break };
            at += 2 + len as usize;
        }
        return (FileKind::Jpeg, (0, 0));
    }
    (FileKind::Other, (0, 0))
}

/// How many cells a picture takes. `px` is its own size (zero if unknown),
/// `cell` the size of one cell in pixels (zero if unknown), `want` what the
/// program asked for and `room` the columns and rows there are. It keeps
/// its shape, and is made smaller if it would not fit.
pub fn footprint(px: (u32, u32), cell: (u16, u16), want: (Dim, Dim), room: (u16, u16)) -> (u16, u16) {
    let cell = if cell.0 == 0 || cell.1 == 0 { (8.0, 16.0) } else { (cell.0 as f64, cell.1 as f64) };
    let room = (room.0.max(1) as f64, room.1.max(1) as f64);
    // A picture of unknown size is taken to be about a third of the width, and 4 by 3.
    let own = if px.0 == 0 || px.1 == 0 { ((room.0 * cell.0 / 3.0).max(cell.0), (room.0 * cell.0 / 4.0).max(cell.1)) } else { (px.0 as f64, px.1 as f64) };
    let length = |want: Dim, cell: f64, room: f64| match want {
        Dim::Auto => None,
        Dim::Cells(n) => Some(n as f64 * cell),
        Dim::Pixels(n) => Some(n as f64),
        Dim::Percent(n) => Some(room * cell * n as f64 / 100.0),
    };
    let (mut w, mut h) = match (length(want.0, cell.0, room.0), length(want.1, cell.1, room.1)) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, w * own.1 / own.0),
        (None, Some(h)) => (h * own.0 / own.1, h),
        (None, None) => own,
    };
    let fit = (room.0 * cell.0 / w).min(room.1 * cell.1 / h).min(1.0);
    w *= fit;
    h *= fit;
    let cells = |length: f64, cell: f64, room: f64| ((length / cell).ceil().max(1.0)).min(room) as u16;
    (cells(w, cell.0, room.0), cells(h, cell.1, room.1))
}

/// The cells a sixel picture covers, which is not ours to choose: it is
/// drawn pixel for pixel. `None` if it does not fit in the room there is.
pub fn sixel_footprint(px: (u32, u32), cell: (u16, u16), room: (u16, u16)) -> Option<(u16, u16)> {
    if px.0 == 0 || px.1 == 0 || cell.0 == 0 || cell.1 == 0 {
        return None;
    }
    let (cols, rows) = (px.0.div_ceil(cell.0 as u32), px.1.div_ceil(cell.1 as u32));
    (cols <= room.0 as u32 && rows <= room.1 as u32).then_some((cols as u16, rows as u16))
}

// ---- a PNG around raw pixels -------------------------------------------------

fn crc32(chunks: &[&[u8]]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for (n, slot) in table.iter_mut().enumerate() {
            let mut c = n as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *slot = c;
        }
        table
    });
    let mut crc = 0xffff_ffffu32;
    for &byte in chunks.iter().flat_map(|chunk| chunk.iter()) {
        crc = table[((crc ^ byte as u32) & 0xff) as usize] ^ (crc >> 8);
    }
    !crc
}

/// A PNG file holding these pixels as they are: the format's "stored"
/// (uncompressed) blocks, which take no compressor to write.
pub fn png_from_pixels(raw: &[u8], px: (u32, u32), alpha: bool) -> Option<Vec<u8>> {
    let (w, h) = (px.0 as usize, px.1 as usize);
    let bpp = if alpha { 4 } else { 3 };
    let stride = w.checked_mul(bpp)?;
    if w == 0 || h == 0 || raw.len() < stride.checked_mul(h)? {
        return None;
    }
    // Each row of pixels goes after a byte saying it is not filtered.
    let mut lines = Vec::with_capacity((stride + 1) * h);
    for row in raw.chunks_exact(stride).take(h) {
        lines.push(0);
        lines.extend_from_slice(row);
    }
    let mut zlib = vec![0x78, 0x01];
    let mut blocks = lines.chunks(0xffff).peekable();
    while let Some(block) = blocks.next() {
        zlib.push(u8::from(blocks.peek().is_none()));
        zlib.extend_from_slice(&(block.len() as u16).to_le_bytes());
        zlib.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in lines.chunks(5552) {
        for &byte in chunk {
            a += byte as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    zlib.extend_from_slice(&(b << 16 | a).to_be_bytes());

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8; 4], body: &[u8]| {
        png.extend_from_slice(&(body.len() as u32).to_be_bytes());
        png.extend_from_slice(kind);
        png.extend_from_slice(body);
        png.extend_from_slice(&crc32(&[kind, body]).to_be_bytes());
    };
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&px.0.to_be_bytes());
    header.extend_from_slice(&px.1.to_be_bytes());
    header.extend_from_slice(&[8, if alpha { 6 } else { 2 }, 0, 0, 0]);
    chunk(b"IHDR", &header);
    chunk(b"IDAT", &zlib);
    chunk(b"IEND", &[]);
    Some(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pieces(splitter: &mut Splitter, data: &[u8]) -> Vec<String> {
        splitter
            .feed(data)
            .into_iter()
            .map(|piece| match piece {
                Piece::Text(range) => format!("text:{}", String::from_utf8_lossy(&data[range]).escape_default()),
                Piece::Held(bytes) => format!("held:{}", String::from_utf8_lossy(&bytes).escape_default()),
                Piece::Picture(protocol, body) => format!("{protocol:?}:{}", String::from_utf8_lossy(&body)),
                Piece::AsksCellSize => "asks-cell-size".to_string(),
            })
            .collect()
    }

    #[test]
    fn pictures_are_taken_out_and_everything_else_is_left_as_it_was() {
        let mut s = Splitter::default();
        assert_eq!(pieces(&mut s, b"plain text"), ["text:plain text"]);
        // Each of the three, ended either way.
        assert_eq!(pieces(&mut s, b"a\x1b]1337;File=inline=1:QUJD\x07b"), ["text:a", "Iterm:inline=1:QUJD", "text:b"]);
        assert_eq!(pieces(&mut s, b"\x1b]1337;File=inline=1:QUJD\x1b\\"), ["Iterm:inline=1:QUJD"]);
        assert_eq!(pieces(&mut s, b"\x1b_Ga=T,f=24,s=1,v=1;AAAA\x1b\\z"), ["Kitty:a=T,f=24,s=1,v=1;AAAA", "text:z"]);
        assert_eq!(pieces(&mut s, b"\x1bP0;1;0q\"1;1;2;6#0~~\x1b\\"), ["Sixel:0;1;0q\"1;1;2;6#0~~"]);
        assert_eq!(pieces(&mut s, b"\x1bPq~\x1b\\"), ["Sixel:q~"]);
        // iTerm2's file in several sequences.
        assert_eq!(
            pieces(&mut s, b"\x1b]1337;MultipartFile=inline=1\x07\x1b]1337;FilePart=QUJD\x07\x1b]1337;FilePart=REVG\x1b\\\x1b]1337;FileEnd\x07x"),
            ["ItermBegin:inline=1", "ItermPart:QUJD", "ItermPart:REVG", "ItermEnd:", "text:x"]
        );

        // What only looks like one at first is handed on whole, in order.
        assert_eq!(pieces(&mut s, b"\x1b[31mred\x1b[0m\x1bM\x1b[1;1H"), ["text:\\u{1b}[31mred\\u{1b}[0m\\u{1b}M\\u{1b}[1;1H"]);
        // The one question taken out of the stream, and its near misses.
        assert_eq!(pieces(&mut s, b"a\x1b[16tb"), ["text:a", "asks-cell-size", "text:b"]);
        assert_eq!(pieces(&mut s, b"\x1b[16;1H\x1b[1t\x1b[18t\x1b[160t"), ["text:\\u{1b}[16;1H\\u{1b}[1t\\u{1b}[18t\\u{1b}[160t"]);
        // Cut off after "16" by the end of a read, and then not it after all.
        assert_eq!(pieces(&mut s, b"\x1b[16"), Vec::<String>::new());
        assert_eq!(pieces(&mut s, b";1H"), ["held:\\u{1b}[16", "text:;1H"]);
        assert_eq!(pieces(&mut s, b"\x1b]0;title\x07"), ["held:\\u{1b}]", "text:0;title\\u{7}"]);
        assert_eq!(pieces(&mut s, b"\x1b]1337;SetMark\x07"), ["held:\\u{1b}]1337;", "text:SetMark\\u{7}"]);
        assert_eq!(pieces(&mut s, b"\x1b]1337;FileX\x07"), ["held:\\u{1b}]1337;File", "text:X\\u{7}"]);
        assert_eq!(pieces(&mut s, b"\x1bP+q544e\x1b\\"), ["held:\\u{1b}P", "text:+q544e\\u{1b}\\\\"]);
        assert_eq!(pieces(&mut s, b"\x1bP$qm\x1b\\"), ["held:\\u{1b}P", "text:$qm\\u{1b}\\\\"]);
        assert_eq!(pieces(&mut s, b"\x1b_other\x1b\\"), ["held:\\u{1b}_", "text:other\\u{1b}\\\\"]);
        assert_eq!(pieces(&mut s, b"\x1b\x1b[A"), ["text:\\u{1b}\\u{1b}[A"]);
        // An escape character at the very end of a read is held until the next.
        assert_eq!(pieces(&mut s, b"x\x1b"), ["text:x"]);
        assert_eq!(pieces(&mut s, b"[0m"), ["held:\\u{1b}[", "text:0m"]);
    }

    #[test]
    fn a_picture_can_arrive_in_any_number_of_reads() {
        let whole: &[u8] = b"before\x1b]1337;File=inline=1:QUJDREVG\x07between\x1b_Ga=q,i=1;AAAA\x1b\\\x1bP9q#0~-~\x1b\\after\x1b[1m\x1b[16t\x1b[16;2H\x1b]0;t\x07";
        let joined = |cut: usize| {
            let mut s = Splitter::default();
            let (mut text, mut pictures) = (Vec::new(), Vec::new());
            for part in whole.chunks(cut) {
                for piece in s.feed(part) {
                    match piece {
                        Piece::Text(range) => text.extend_from_slice(&part[range]),
                        Piece::Held(bytes) => text.extend_from_slice(&bytes),
                        Piece::Picture(protocol, body) => pictures.push((protocol, body)),
                        Piece::AsksCellSize => pictures.push((Protocol::Iterm, b"asks".to_vec())),
                    }
                }
            }
            (text, pictures)
        };
        let expected = joined(whole.len());
        assert_eq!(expected.0, b"beforebetweenafter\x1b[1m\x1b[16;2H\x1b]0;t\x07");
        assert_eq!(expected.1.len(), 4);
        for cut in 1..whole.len() {
            assert_eq!(joined(cut), expected, "cut every {cut} bytes");
        }
    }

    #[test]
    fn a_picture_cut_short_or_too_large_is_dropped_and_what_follows_is_kept() {
        let mut s = Splitter::default();
        // Another sequence begins before the picture has ended.
        assert_eq!(pieces(&mut s, b"\x1b_Ga=T;AAAA\x1b[0mok"), ["held:\\u{1b}[", "text:0mok"]);
        // One that starts a new picture is that picture.
        assert_eq!(pieces(&mut s, b"\x1bPqabc\x1b_Ga=q\x1b\\"), ["Kitty:a=q"]);
        // Too large: nothing is kept, and the stream goes on after its end.
        let mut big = b"\x1b]1337;File=inline=1:".to_vec();
        big.resize(big.len() + MAX_PICTURE + 10, b'A');
        big.extend_from_slice(b"\x07next");
        let out = s.feed(&big);
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], Piece::Text(range) if &big[range.clone()] == b"next"));
        assert!(s.buf.is_empty());
    }

    #[test]
    fn the_marks_in_a_cell_say_which_picture_and_where() {
        for (id, row, col) in [(1, 0, 0), (0xfffffe, 4095, 4095), (70000, 17, 300), (255, 256, 1)] {
            let (fg, bg) = mark(id, row, col);
            assert_eq!(unmark(fg, bg), (id, row, col));
        }
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        png_from_pixels(&vec![0x80; (w * h * 3) as usize], (w, h), false).unwrap()
    }

    #[test]
    fn a_file_says_what_it_is_and_how_big() {
        let b64 = STANDARD.encode(png(640, 480));
        assert_eq!(file_size(b64.as_bytes()), (FileKind::Png, (640, 480)));
        let gif = STANDARD.encode(b"GIF89a\x40\x01\xf0\x00rest");
        assert_eq!(file_size(gif.as_bytes()), (FileKind::Gif, (320, 240)));
        // A JPEG keeps its size after whatever notes come first.
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe1, 0x00, 0x06, 1, 2, 3, 4, 0xff, 0xdb, 0x00, 0x03, 9];
        jpeg.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 8, 0x01, 0x2c, 0x01, 0x90, 3]);
        assert_eq!(file_size(STANDARD.encode(&jpeg).as_bytes()), (FileKind::Jpeg, (400, 300)));
        assert_eq!(file_size(STANDARD.encode(b"%PDF-1.7").as_bytes()), (FileKind::Other, (0, 0)));
        assert_eq!(file_size(b"not base64 at all!"), (FileKind::Other, (0, 0)));
        // Without its padding, as some programs send it.
        let unpadded = STANDARD.encode(png(3, 2));
        assert_eq!(file_size(unpadded.trim_end_matches('=').as_bytes()).1, (3, 2));
    }

    #[test]
    fn an_iterm_file_is_read_with_what_it_asks_for() {
        let b64 = STANDARD.encode(png(100, 50));
        let file = parse_iterm(format!("name=eA==;size=9;width=20;height=auto;inline=1:{b64}").as_bytes()).unwrap();
        assert_eq!((file.kind, file.px, file.width, file.height), (FileKind::Png, (100, 50), Dim::Cells(20), Dim::Auto));
        let file = parse_iterm(format!("inline=1;width=50%;height=120px:{b64}").as_bytes()).unwrap();
        assert_eq!((file.width, file.height), (Dim::Percent(50), Dim::Pixels(120)));
        // A download, not a picture; and nothing at all.
        assert!(parse_iterm(format!("name=eA==:{b64}").as_bytes()).is_none());
        assert!(parse_iterm(b"inline=1:").is_none());
        assert!(parse_iterm(b"inline=1").is_none());
        // Something in the data that is not data: not passed on to anyone's terminal.
        assert!(parse_iterm(b"inline=1:QUJD\x9cQUJD").is_none());
        assert!(parse_iterm(b"inline=1:QUJD;QUJD").is_none());
        assert!(is_base64(b"QUJD+/==") && !is_base64(b"QU JD") && !is_base64(b"QUJD\x1b"));
        assert!(is_sixel(b"q\"1;1;2;6#0~~-\n") && !is_sixel(b"q~\x9c") && !is_sixel(b"q~\x07"));
    }

    #[test]
    fn a_kitty_command_is_read_into_its_keys() {
        let cmd = KittyCmd::parse(b"a=T,f=100,i=7,q=2,m=1;QUJD\n");
        assert_eq!((cmd.letter('a', 't'), cmd.number('f'), cmd.number('i'), cmd.number('m')), ('T', 100, 7, 1));
        assert_eq!(cmd.payload, b"QUJD");
        assert_eq!((cmd.letter('t', 'd'), cmd.number('s')), ('d', 0));
        let cmd = KittyCmd::parse(b"m=0");
        assert!(cmd.payload.is_empty());
        assert_eq!(cmd.number('m'), 0);
    }

    #[test]
    fn a_sixel_picture_is_measured() {
        // Declared.
        assert_eq!(sixel_size(b"0;0;0q\"1;1;120;60#0~~"), (120, 60));
        // Not declared: three bands, the widest of them forty wide.
        assert_eq!(sixel_size(b"q#0;2;0;0;0#0!40~$#1!10?-!12~-~~~"), (40, 18));
        // A band with nothing in it after the last line break adds nothing.
        assert_eq!(sixel_size(b"q~~~-"), (3, 6));
        assert_eq!(sixel_size(b"no introducer"), (0, 0));
    }

    #[test]
    fn a_picture_takes_the_cells_its_shape_needs_and_no_more_than_there_are() {
        let auto = (Dim::Auto, Dim::Auto);
        // 400 by 320 pixels in cells of 10 by 20.
        assert_eq!(footprint((400, 320), (10, 20), auto, (80, 24)), (40, 16));
        // Too wide for the room: smaller, in the same shape.
        assert_eq!(footprint((1600, 1280), (10, 20), auto, (80, 24)), (60, 24));
        assert_eq!(footprint((1600, 400), (10, 20), auto, (80, 24)), (80, 10));
        // Asked for by one side, the other follows.
        assert_eq!(footprint((400, 320), (10, 20), (Dim::Cells(20), Dim::Auto), (80, 24)), (20, 8));
        assert_eq!(footprint((400, 320), (10, 20), (Dim::Auto, Dim::Pixels(160)), (80, 24)), (20, 8));
        assert_eq!(footprint((400, 320), (10, 20), (Dim::Percent(50), Dim::Auto), (80, 24)), (40, 16));
        // Both sides given: taken as asked, if that fits.
        assert_eq!(footprint((400, 320), (10, 20), (Dim::Cells(10), Dim::Cells(10)), (80, 24)), (10, 10));
        // Nothing known about the cells or the picture: still something sensible.
        assert_eq!(footprint((400, 320), (0, 0), auto, (80, 24)), (50, 20));
        let unknown = footprint((0, 0), (10, 20), auto, (90, 24));
        assert!(unknown.0 >= 20 && unknown.0 <= 40 && unknown.1 >= 5 && unknown.1 <= 24, "{unknown:?}");
        assert_eq!(footprint((1, 1), (10, 20), auto, (80, 24)), (1, 1));

        assert_eq!(sixel_footprint((120, 60), (10, 20), (80, 24)), Some((12, 3)));
        assert_eq!(sixel_footprint((121, 61), (10, 20), (80, 24)), Some((13, 4)));
        assert_eq!(sixel_footprint((1200, 60), (10, 20), (80, 24)), None);
        assert_eq!(sixel_footprint((120, 60), (0, 0), (80, 24)), None);
    }

    fn image(data: Data, px: (u32, u32)) -> Image {
        Image { id: 9, data: std::sync::Arc::new(data), px, cells: (4, 2), cell_px: (10, 20) }
    }

    #[test]
    fn a_picture_goes_to_a_terminal_in_a_way_it_understands() {
        let (kitty, iterm, sixel) = (Caps::named("kitty"), Caps::named("iterm"), Caps::named("sixel"));
        let file = |kind| image(Data::File { b64: b"QUJD".to_vec(), kind }, (2, 2));
        let pixels = |compressed| image(Data::Pixels { b64: STANDARD.encode([1u8; 12]).into_bytes(), alpha: false, compressed }, (2, 2));
        let six = image(Data::Sixel { body: b"q~~".to_vec() }, (2, 6));

        assert_eq!([file(FileKind::Png).how(kitty), file(FileKind::Png).how(iterm), file(FileKind::Png).how(sixel)], [Some(How::Kitty), Some(How::Iterm), None]);
        assert_eq!([file(FileKind::Jpeg).how(kitty), file(FileKind::Jpeg).how(iterm)], [None, Some(How::Iterm)]);
        assert_eq!([pixels(false).how(kitty), pixels(false).how(iterm), pixels(true).how(iterm)], [Some(How::Kitty), Some(How::Iterm), None]);
        assert_eq!([six.how(kitty), six.how(iterm), six.how(sixel), six.how(Caps::named("iterm,sixel"))], [None, None, Some(How::Sixel), Some(How::Sixel)]);
        assert_eq!(file(FileKind::Png).how(Caps::named("none")), None);
        assert!(Caps::named("Kitty, SIXEL").kitty && Caps::named("kitty, sixel").sixel && Caps::named("kitty").any() && !Caps::named("").any());

        let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
        let mut out = Vec::new();
        file(FileKind::Jpeg).iterm(&mut out);
        assert_eq!(text(&out), "\x1b]1337;File=inline=1;width=4;height=2;preserveAspectRatio=1:QUJD\x07");

        // Raw pixels become a PNG the other kind of terminal can read.
        out.clear();
        pixels(false).iterm(&mut out);
        let sent = text(&out);
        let b64 = sent.split(':').nth(1).unwrap().trim_end_matches('\x07');
        assert_eq!(file_size(b64.as_bytes()), (FileKind::Png, (2, 2)));

        out.clear();
        pixels(true).kitty_send(&mut out);
        assert_eq!(text(&out), format!("\x1b_Ga=t,i=9,f=24,s=2,v=2,o=z,q=2,m=0;{}\x1b\\", STANDARD.encode([1u8; 12])));
        out.clear();
        file(FileKind::Png).kitty_place(3, &mut out);
        kitty_unplace(9, 3, &mut out);
        kitty_forget(9, &mut out);
        assert_eq!(text(&out), "\x1b_Ga=p,i=9,p=3,c=4,r=2,C=1,q=2\x1b\\\x1b_Ga=d,d=i,i=9,p=3,q=2\x1b\\\x1b_Ga=d,d=I,i=9,q=2\x1b\\");
        out.clear();
        six.sixel(&mut out);
        assert_eq!(out, b"\x1bPq~~\x1b\\");
    }

    #[test]
    fn a_large_picture_is_sent_to_kitty_in_pieces() {
        let b64 = vec![b'A'; 4096 * 2 + 8];
        let mut out = Vec::new();
        image(Data::File { b64: b64.clone(), kind: FileKind::Png }, (1, 1)).kitty_send(&mut out);
        let sent = String::from_utf8(out).unwrap();
        let parts: Vec<&str> = sent.split("\x1b\\").filter(|p| !p.is_empty()).collect();
        assert_eq!(parts.len(), 3);
        assert!(parts[0].starts_with("\x1b_Ga=t,i=9,f=100,q=2,m=1;") && parts[1].starts_with("\x1b_Gm=1;") && parts[2].starts_with("\x1b_Gm=0;"));
        let carried: String = parts.iter().map(|p| p.split(';').nth(1).unwrap()).collect();
        assert_eq!(carried.as_bytes(), &b64[..]);
    }

    #[test]
    fn pixels_wrapped_in_a_png_are_a_file_any_reader_accepts() {
        // Two rows of two pixels with transparency; and more than one
        // stored block's worth, which is where the format is easy to get wrong.
        let small = png_from_pixels(&[255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 9, 9, 9, 0], (2, 2), true).unwrap();
        assert_eq!(&small[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&small[12..16], b"IHDR");
        assert_eq!(small[25], 6, "color type: with alpha");
        // The checksum of a chunk covers its name and its body.
        assert_eq!(crc32(&[b"IEND"]), 0xae42_6082);
        assert_eq!(&small[small.len() - 12..], b"\x00\x00\x00\x00IEND\xae\x42\x60\x82");
        let large = png_from_pixels(&vec![7u8; 300 * 300 * 3], (300, 300), false).unwrap();
        let idat = large.windows(4).position(|w| w == b"IDAT").unwrap();
        let zlib = &large[idat + 4..large.len() - 16];
        // Stored blocks: each says its length and the complement of it; only the last is marked last.
        let (mut at, mut blocks, mut total) = (2, 0, 0usize);
        loop {
            let last = zlib[at];
            let len = u16::from_le_bytes([zlib[at + 1], zlib[at + 2]]);
            assert_eq!(!len, u16::from_le_bytes([zlib[at + 3], zlib[at + 4]]));
            at += 5 + len as usize;
            blocks += 1;
            total += len as usize;
            if last == 1 {
                break;
            }
        }
        assert_eq!((blocks, total, at + 4), (5, 300 * 301 * 3 - 300 * 2, zlib.len()));
        assert!(png_from_pixels(&[1, 2, 3], (2, 2), false).is_none());
        assert!(png_from_pixels(&[], (0, 0), false).is_none());
    }
}
