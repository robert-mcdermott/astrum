//! Pictures in a frame: which of them a terminal is shown, and the bytes
//! that put them there and take them away. (See `crate::image` for where
//! they come from and how they are kept.)

use super::Server;
use crate::image::{self, How, Image, PLACEHOLDER};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A picture is drawn once it has been in one place this long. While
/// output scrolls it is not sent again for every line, which for most
/// terminals means the whole picture each time.
const SETTLE: Duration = Duration::from_millis(120);

/// A picture a terminal has on its screen.
pub struct Shown {
    pub id: u32,
    /// The number this terminal knows this placement by (kitty's way only).
    pub placement: u32,
    pub rect: Rect,
    pub how: How,
}

/// What one terminal has been sent, and what it has been seen to need.
#[derive(Default)]
pub struct Pictures {
    pub shown: Vec<Shown>,
    /// The pictures a kitty terminal has been handed and still holds.
    sent: Vec<u32>,
    /// Where each picture in the last frame was, and since when.
    seen: Vec<(u32, (u16, u16), Instant)>,
    placements: u32,
}

/// A picture to be on a terminal's screen after this frame.
pub struct Wanted {
    image: Arc<Image>,
    rect: Rect,
    how: How,
}

impl Pictures {
    /// The screen was cleared: nothing is on it any more.
    pub fn cleared(&mut self, kitty: bool, out: &mut Vec<u8>) {
        self.shown.clear();
        if kitty {
            image::kitty_forget_all(out);
            self.sent.clear();
        }
    }

    /// Before the cells are sent: where a picture is to go from the
    /// screen, the cells under it are sent again whatever they hold, which
    /// is what takes it away (kitty's are taken away by name, in `after`).
    pub fn before(&mut self, wanted: &[Wanted], prev: &mut Buffer) {
        for shown in self.shown.iter().filter(|s| s.how != How::Kitty && !stays(s, wanted)) {
            for y in shown.rect.y..shown.rect.y + shown.rect.height {
                for x in shown.rect.x..shown.rect.x + shown.rect.width {
                    if let Some(cell) = prev.cell_mut((x, y)) {
                        cell.set_char(PLACEHOLDER);
                    }
                }
            }
        }
    }

    /// After the cells: take away kitty's pictures that are to go, and draw
    /// the ones that are new or have moved. `alive` says whether a picture
    /// is still kept anywhere.
    pub fn after(&mut self, wanted: Vec<Wanted>, alive: impl Fn(u32) -> bool, out: &mut Vec<u8>) {
        for shown in self.shown.iter().filter(|s| s.how == How::Kitty && !stays(s, &wanted)) {
            image::kitty_unplace(shown.id, shown.placement, out);
        }
        let mut next = Vec::with_capacity(wanted.len());
        for want in wanted {
            if let Some(at) = self.shown.iter().position(|s| s.id == want.image.id && s.rect == want.rect && s.how == want.how) {
                next.push(self.shown.swap_remove(at));
                continue;
            }
            self.placements = self.placements % 0xff_ffff + 1;
            let shown = Shown { id: want.image.id, placement: self.placements, rect: want.rect, how: want.how };
            out.extend_from_slice(format!("\x1b[{};{}H", want.rect.y + 1, want.rect.x + 1).as_bytes());
            match want.how {
                How::Kitty => {
                    if !self.sent.contains(&shown.id) {
                        want.image.kitty_send(out);
                        self.sent.push(shown.id);
                    }
                    want.image.kitty_place(shown.placement, out);
                }
                How::Iterm => want.image.iterm(out),
                How::Sixel => want.image.sixel(out),
            }
            next.push(shown);
        }
        self.shown = next;
        // A kitty terminal keeps what it was handed until told otherwise.
        self.sent.retain(|id| {
            let keep = alive(*id);
            if !keep {
                image::kitty_forget(*id, out);
            }
            keep
        });
    }
}

fn stays(shown: &Shown, wanted: &[Wanted]) -> bool {
    wanted.iter().any(|w| w.image.id == shown.id && w.rect == shown.rect && w.how == shown.how)
}

/// The cells of one picture found in a frame.
struct Found {
    /// Where its top left corner is, or would be if it were on screen.
    origin: (i32, i32),
    cells: Vec<(u16, u16, u16)>,
}

impl Server {
    /// Find the pictures in a frame that has been drawn for the terminal
    /// `client`, and settle each: one whose every cell is there, that this
    /// terminal has a way to show, and that has stopped moving is wanted on
    /// its screen, and its cells are left empty for it. The cells of any
    /// other say that a picture belongs there.
    pub(super) fn settle_pictures(&mut self, client: u64, frame: &mut Buffer) -> Vec<Wanted> {
        let area = frame.area;
        let mut mark = [0u8; 4];
        let mark: &str = PLACEHOLDER.encode_utf8(&mut mark);
        // Where each pane of the tab in front is drawn. A picture is
        // looked for only inside a pane, and belongs to that pane: the
        // marks are a program's to write, and one that wrote them by hand
        // must not be able to have a picture drawn outside its own pane,
        // or borrow another pane's.
        let panes: Vec<(u64, Rect)> = match self.tabs.get(self.active) {
            Some(tab) => {
                let within = self.panes_area();
                let laid_out = match self.foreign_size() {
                    Some((cols, rows)) => Rect::new(within.x, within.y, cols, rows),
                    None => within,
                };
                tab.panes.place(laid_out).0.into_iter().map(|(id, rect)| (id, rect.intersection(within))).collect()
            }
            None => Vec::new(),
        };
        let pane_at = |x: u16, y: u16| panes.iter().find(|(_, r)| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height).copied();
        let mut found: HashMap<(u32, i32, i32), Found> = HashMap::new();
        for y in 0..area.height {
            for x in 0..area.width {
                let cell = &frame[(x, y)];
                if cell.symbol() != mark || pane_at(x, y).is_none() {
                    continue;
                }
                let (Color::Rgb(a, b, c), Color::Rgb(d, e, f)) = (cell.fg, cell.bg) else { continue };
                let (id, row, col) = image::unmark([a, b, c], [d, e, f]);
                let origin = (x as i32 - col as i32, y as i32 - row as i32);
                found.entry((id, origin.0, origin.1)).or_insert_with(|| Found { origin, cells: Vec::new() }).cells.push((x, y, row));
            }
        }
        let Some(at) = self.clients.iter().position(|c| c.id == client) else { return Vec::new() };
        if found.is_empty() {
            self.clients[at].pictures.seen.clear();
            return Vec::new();
        }

        let th = &self.theme;
        let blank = if self.cfg.theme_terminal && !th.flat { Style::default().fg(th.term_fg).bg(th.term_bg) } else { Style::default() };
        let quiet = blank.fg(th.border);
        let (caps, cell_px) = if self.cfg.images { (self.clients[at].images, self.clients[at].cell_px) } else { Default::default() };
        let now = Instant::now();
        let mut seen = Vec::new();
        let mut wanted = Vec::new();
        let mut keys: Vec<(u32, i32, i32)> = found.keys().copied().collect();
        keys.sort_unstable();
        for key in keys {
            let found = &found[&key];
            // The pane these cells are in, which has to be the one that
            // holds the picture and has to hold all of it.
            let home = found.cells.first().and_then(|(x, y, _)| pane_at(*x, *y));
            let image = home.and_then(|(id, _)| self.tabs.get(self.active).and_then(|tab| tab.panes.get(id)).and_then(|pane| pane.image(key.0))).cloned();
            let whole = image.as_ref().zip(home).is_some_and(|(image, (_, pane))| {
                let (cols, rows) = (image.cells.0 as i32, image.cells.1 as i32);
                let inside = found.origin.0 >= pane.x as i32 && found.origin.1 >= pane.y as i32 && found.origin.0 + cols <= (pane.x + pane.width) as i32 && found.origin.1 + rows <= (pane.y + pane.height) as i32;
                // Every cell where its own row and column say, and each once.
                let mut cells: Vec<(u16, u16)> = found.cells.iter().map(|(x, y, _)| (*x, *y)).collect();
                cells.sort_unstable();
                cells.dedup();
                let in_range = found.cells.iter().all(|(x, y, _)| (*x as i32 - found.origin.0) < cols && (*y as i32 - found.origin.1) < rows);
                inside && in_range && cells.len() == (cols * rows) as usize
            });
            let how = image.as_ref().filter(|_| whole).and_then(|image| {
                // Sixel is not scaled: it fits only cells of the size it was measured for.
                image.how(caps).filter(|how| *how != How::Sixel || (cell_px == image.cell_px && cell_px.0 > 0))
            });
            let mut pending = false;
            if let (Some(image), Some(how)) = (&image, how) {
                let origin = (found.origin.0 as u16, found.origin.1 as u16);
                let since = self.clients[at].pictures.seen.iter().find(|(id, was, _)| *id == key.0 && *was == origin).map_or(now, |(_, _, since)| *since);
                seen.push((key.0, origin, since));
                if now.duration_since(since) >= SETTLE {
                    wanted.push(Wanted { image: image.clone(), rect: Rect::new(origin.0, origin.1, image.cells.0, image.cells.1), how });
                } else {
                    pending = true;
                    let due = since + SETTLE;
                    self.pictures_due = Some(self.pictures_due.map_or(due, |already| already.min(due)));
                }
            }
            // Its cells: empty under a picture that is drawn or about to
            // be; otherwise a faint pattern, with a line saying what it is.
            let drawn = how.is_some();
            for &(x, y, _) in &found.cells {
                let cell = &mut frame[(x, y)];
                cell.reset();
                cell.set_symbol(if drawn { " " } else { "░" });
                cell.set_style(if drawn { blank } else { quiet });
            }
            if drawn || pending {
                continue;
            }
            let label = match &image {
                None => "image no longer kept".to_string(),
                Some(image) if whole && !self.cfg.images => format!("{} · images are off", image.label()),
                Some(image) if whole => format!("{} · this terminal cannot show it", image.label()),
                Some(image) => image.label(),
            };
            // On its first row, where that is on screen and wide enough.
            let mut first: Vec<u16> = found.cells.iter().filter(|(_, y, row)| *row == 0 && *y as i32 == found.origin.1).map(|(x, _, _)| *x).collect();
            first.sort_unstable();
            let text: Vec<char> = format!(" {label} ").chars().collect();
            let contiguous = first.windows(2).all(|pair| pair[1] == pair[0] + 1);
            if contiguous && first.len() >= text.len() + 2 {
                for (x, ch) in first.iter().skip(1).zip(text) {
                    let cell = &mut frame[(*x, found.origin.1 as u16)];
                    cell.set_char(ch);
                    cell.set_style(quiet.add_modifier(Modifier::DIM));
                }
            }
        }
        self.clients[at].pictures.seen = seen;
        wanted
    }
}
