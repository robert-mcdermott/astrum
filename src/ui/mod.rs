//! Drawing. Every frame is composed into a ratatui `Buffer`; the server then
//! diffs it against the previous frame and ships only the changed cells.

pub mod text;

use crate::config::Autonomy;
use crate::pane::{CopyMode, Pane};
use crate::paths;
use crate::server::session::{AgentSession, InputMode, Item, ToolItem, ToolState};
use crate::keys::Action;
use crate::server::settings::{SECTIONS, SettingsView};
use crate::server::{Focus, Hit, Menu, OFFERED, Overlay, Picker, Server, Tab, WALKTHROUGH_STEPS, Walk};
use crate::theme::Theme;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{Color as TermColor, CursorShape, NamedColor};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Widget};
use text::{Styles, truncate, truncate_left, width};
use unicode_width::UnicodeWidthChar;

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub struct Cursor {
    pub x: u16,
    pub y: u16,
    /// DECSCUSR parameter (0 = the host terminal's default).
    pub shape: u8,
}

pub struct Layout {
    pub tabbar: Rect,
    pub pane: Rect,
    pub panel: Option<Rect>,
    /// With the panel beside the shell: the line between them, which is
    /// also the handle for resizing.
    pub edge: Option<Rect>,
    pub status: Option<Rect>,
}

/// Where everything goes. `side`, when the agent panel is docked beside the
/// shell, is the share of the width it takes, in thousandths; otherwise the
/// panel sits below the shell and takes `panel_pct` of the height.
pub fn layout(cols: u16, rows: u16, panel: bool, panel_pct: u16, side: Option<u16>, status_bar: bool) -> Layout {
    let status_h = if status_bar && rows >= 6 { 1 } else { 0 };
    let body = rows.saturating_sub(1 + status_h).max(1);
    let status = (status_h > 0).then(|| Rect::new(0, rows - 1, cols, 1));
    let tabbar = Rect::new(0, 0, cols, 1);
    // Beside the shell, if both keep a usable width.
    if let (true, Some(share)) = (panel && cols >= 80, side) {
        let panel_w = (((cols as u32 * share as u32 + 500) / 1000) as u16).clamp(36, cols - 41);
        let pane_w = cols - panel_w - 1;
        return Layout {
            tabbar,
            pane: Rect::new(0, 1, pane_w, body),
            panel: Some(Rect::new(pane_w + 1, 1, panel_w, body)),
            edge: Some(Rect::new(pane_w, 1, 1, body)),
            status,
        };
    }
    let panel_h = if panel && body >= 8 {
        ((rows as u32 * panel_pct as u32 / 100) as u16).clamp(7.min(body - 3), body - 3)
    } else if panel {
        body.saturating_sub(1)
    } else {
        0
    };
    let pane_h = body - panel_h;
    Layout {
        tabbar,
        pane: Rect::new(0, 1, cols, pane_h),
        panel: (panel_h > 0).then(|| Rect::new(0, 1 + pane_h, cols, panel_h)),
        edge: None,
        status,
    }
}

pub fn tab_title(tab: &Tab) -> String {
    if let Some(name) = &tab.name {
        return name.clone();
    }
    // A tab on another machine says which, before what it is doing there.
    match tab.panes.focused().there() {
        Some(there) => {
            let host = there.host.split_whitespace().last().unwrap_or(&there.host);
            let host = host.rsplit('@').next().unwrap_or(host);
            format!("{}: {}", truncate(host, 16), doing_title(tab))
        }
        None => doing_title(tab),
    }
}

/// A tab's title from what its shell is doing or where it is.
fn doing_title(tab: &Tab) -> String {
    if let Some(cmd) = tab.panes.focused().running_cmd() {
        let word = cmd
            .split_whitespace()
            .find(|w| !w.contains('=') && !matches!(*w, "sudo" | "env" | "command" | "exec" | "nohup" | "time"))
            .unwrap_or("");
        let base = word.rsplit('/').next().unwrap_or(word);
        if !base.is_empty() {
            return base.to_string();
        }
    }
    if tab.panes.focused().integrated {
        if let Some(cwd) = &tab.panes.focused().cwd {
            let tilde = paths::tildify(cwd);
            if tilde == "~" || tilde == "/" {
                return tilde;
            }
            return tilde.rsplit('/').next().unwrap_or(&tilde).to_string();
        }
    }
    match &tab.panes.focused().osc_title {
        Some(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => tab.panes.focused().shell_name.clone(),
    }
}

/// Write text at (x, y), clipped to `max_x` (exclusive). Returns the next x.
fn put(buf: &mut Buffer, x: u16, y: u16, s: &str, style: Style, max_x: u16) -> u16 {
    if x >= max_x || y >= buf.area.height {
        return x;
    }
    let (nx, _) = buf.set_stringn(x, y, s, (max_x - x) as usize, style);
    nx
}

fn fill(buf: &mut Buffer, area: Rect, style: Style) {
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.reset();
                cell.set_style(style);
            }
        }
    }
}

/// A filled label. Flat themes have no backgrounds, so they invert instead.
fn chip_style(th: &Theme, bg: Color, fg: Color) -> Style {
    if th.flat { Style::default().fg(bg).add_modifier(Modifier::REVERSED | Modifier::BOLD) } else { Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD) }
}

fn hover_bg(th: &Theme, base: Style, hovered: bool) -> Style {
    if !hovered {
        base
    } else if th.flat {
        base.add_modifier(Modifier::UNDERLINED)
    } else {
        base.bg(th.overlay)
    }
}

pub fn draw(s: &mut Server, buf: &mut Buffer) -> Option<Cursor> {
    let th = s.theme.clone();
    let mut hits: Vec<(Rect, Hit)> = Vec::new();
    let active = s.active.min(s.tabs.len().saturating_sub(1));
    let panel_visible = s.tabs[active].agent.visible;
    let l = s.layout(panel_visible);

    draw_tabbar(s, &th, l.tabbar, buf, &mut hits);

    let mut cursor = draw_panes(s, &th, l.pane, buf, &mut hits);
    if s.tabs[active].focus != Focus::Shell {
        cursor = None;
    }
    if let Some(edge) = l.edge {
        // The line between the shell and a panel beside it; drag it to resize.
        let lit = s.ui.hover == Some(Hit::Divider);
        let style = Style::default().fg(if lit { th.accent } else { th.border }).bg(th.bg);
        for y in edge.y..edge.y + edge.height {
            if let Some(cell) = buf.cell_mut((edge.x, y)) {
                cell.reset();
                cell.set_symbol("│");
                cell.set_style(style);
            }
        }
        hits.push((edge, Hit::Divider));
    }
    if let Some(area) = l.panel {
        let c = draw_panel(s, &th, area, buf, &mut hits, l.edge.is_some());
        if s.tabs[active].focus == Focus::Agent {
            cursor = c;
        }
    }
    if let Some(area) = l.status {
        draw_status(s, &th, area, buf, &mut hits);
    }
    if let Some((msg, _)) = &s.ui.toast {
        let w = (width(msg) as u16 + 4).min(s.cols.saturating_sub(2));
        let area = Rect::new(s.cols.saturating_sub(w + 1), 1.min(s.rows.saturating_sub(1)), w, 1);
        let style = if th.flat { Style::default().add_modifier(Modifier::REVERSED) } else { Style::default().fg(th.text).bg(th.overlay) };
        fill(buf, area, style);
        let x = put(buf, area.x, area.y, "▌ ", style.fg(th.accent), area.x + area.width);
        put(buf, x, area.y, &truncate(msg, w.saturating_sub(3) as usize), style, area.x + area.width);
    }
    if s.ui.overlay.is_some() {
        cursor = draw_overlay(s, &th, buf, &mut hits);
    }
    pair_wide_cells(buf);
    s.ui.hits = hits;
    cursor
}

/// Make a frame say what its double-width characters do on a terminal.
///
/// A terminal draws such a character over two cells, and gives the second
/// the character's colors. A frame holds it in one cell, and whatever was
/// drawn there in the next. Left like that, what Astrum believes is on the
/// terminal's screen and what is there drift apart: the second cell, once
/// the character has gone, can keep a background nobody asked for.
///
/// So the cell after each double-width character is made its second half:
/// blank, in the character's colors, and not to be sent on its own. Where
/// something else was drawn in that cell (the edge of a menu, or the end
/// of the row), the character cannot be shown, and a blank stands in.
fn pair_wide_cells(buf: &mut Buffer) {
    use ratatui::buffer::{Cell, CellDiffOption};
    let width = buf.area.width as usize;
    if width == 0 {
        return;
    }
    for row in buf.content.chunks_mut(width) {
        let mut x = 0;
        while x < row.len() {
            let symbol = row[x].symbol();
            if symbol.is_ascii() || crate::ui::text::width(symbol) < 2 {
                x += 1;
                continue;
            }
            let (fg, bg, modifier) = (row[x].fg, row[x].bg, row[x].modifier);
            let free = row.get(x + 1).is_some_and(|next| next.symbol() == " " && (next.bg == bg || *next == Cell::EMPTY));
            if free {
                let next = &mut row[x + 1];
                next.reset();
                next.fg = fg;
                next.bg = bg;
                next.modifier = modifier;
                next.diff_option = CellDiffOption::Skip;
                x += 2;
            } else {
                row[x].set_char(' ');
                x += 1;
            }
        }
    }
}

// ---- tab bar ----------------------------------------------------------------

fn draw_tabbar(s: &Server, th: &Theme, area: Rect, buf: &mut Buffer, hits: &mut Vec<(Rect, Hit)>) {
    let bar = Style::default().fg(th.subtext).bg(th.bar);
    fill(buf, area, bar);
    let y = area.y;
    let end = area.x + area.width;
    let mut x = put(buf, area.x, y, " ✦ ", Style::default().fg(th.accent).bg(th.bar).add_modifier(Modifier::BOLD), end);

    let session = if s.session == "default" { String::new() } else { format!(" {} ", s.session) };
    let right_w = width(&session) as u16;
    let n = s.tabs.len().max(1) as u16;
    let avail = area.width.saturating_sub(x + 3 + right_w);
    // " 1 title ✕ " costs the title plus 7 columns.
    let title_w = (avail / n).saturating_sub(7).clamp(3, 22) as usize;

    for (i, tab) in s.tabs.iter().enumerate() {
        if x + 8 > end.saturating_sub(right_w + 3) {
            put(buf, x, y, "…", bar, end);
            x += 1;
            break;
        }
        let is_active = i == s.active;
        let hovered = s.ui.hover == Some(Hit::Tab(i)) || s.ui.hover == Some(Hit::CloseTab(i));
        let base = if is_active {
            if th.flat { Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED) } else { Style::default().fg(th.text).bg(th.surface).add_modifier(Modifier::BOLD) }
        } else {
            hover_bg(th, bar, hovered)
        };
        let start = x;
        let num_style = if is_active && !th.flat { base.fg(th.accent) } else { base };
        x = put(buf, x, y, &format!(" {} ", i + 1), num_style, end);
        // Activity marker: agent working, command running, or unseen output.
        let marker = if tab.agent.busy {
            Some((SPINNER[s.ui.spinner % SPINNER.len()], th.accent2))
        } else if tab.activity {
            Some(("●", th.accent))
        } else if tab.panes.iter().any(|p| p.is_busy()) && !is_active {
            Some(("●", th.yellow))
        } else {
            None
        };
        if let Some((glyph, color)) = marker {
            x = put(buf, x, y, glyph, base.fg(color), end);
            x = put(buf, x, y, " ", base, end);
        }
        x = put(buf, x, y, &truncate(&tab_title(tab), title_w), base, end);
        x = put(buf, x, y, " ", base, end);
        let close_start = x;
        let close_hover = s.ui.hover == Some(Hit::CloseTab(i));
        let close_style = if close_hover { base.fg(th.red).add_modifier(Modifier::BOLD) } else { base.fg(th.muted) };
        x = put(buf, x, y, "✕", close_style, end);
        x = put(buf, x, y, " ", base, end);
        hits.push((Rect::new(start, y, x - start, 1), Hit::Tab(i)));
        hits.push((Rect::new(close_start.saturating_sub(1), y, (x - close_start + 1).min(3), 1), Hit::CloseTab(i)));
    }

    let plus_start = x;
    let plus = hover_bg(th, bar.fg(th.accent2).add_modifier(Modifier::BOLD), s.ui.hover == Some(Hit::NewTab));
    x = put(buf, x, y, " + ", plus, end);
    hits.push((Rect::new(plus_start, y, x - plus_start, 1), Hit::NewTab));

    if right_w > 0 && end > x + right_w {
        put(buf, end - right_w, y, &session, bar.fg(th.muted), end);
    }
}

// ---- terminal pane ------------------------------------------------------------

fn term_color(c: TermColor, th: &Theme, recolor: bool, is_fg: bool) -> Color {
    let recolor = recolor && !th.flat;
    let indexed = |i: u8| if recolor && i < 16 { th.ansi[i as usize] } else { Color::Indexed(i) };
    let default = || if !recolor { Color::Reset } else if is_fg { th.term_fg } else { th.term_bg };
    match c {
        TermColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        TermColor::Indexed(i) => indexed(i),
        TermColor::Named(n) => match n {
            NamedColor::Black => indexed(0),
            NamedColor::Red => indexed(1),
            NamedColor::Green => indexed(2),
            NamedColor::Yellow => indexed(3),
            NamedColor::Blue => indexed(4),
            NamedColor::Magenta => indexed(5),
            NamedColor::Cyan => indexed(6),
            NamedColor::White => indexed(7),
            NamedColor::BrightBlack => indexed(8),
            NamedColor::BrightRed => indexed(9),
            NamedColor::BrightGreen => indexed(10),
            NamedColor::BrightYellow => indexed(11),
            NamedColor::BrightBlue => indexed(12),
            NamedColor::BrightMagenta => indexed(13),
            NamedColor::BrightCyan => indexed(14),
            NamedColor::BrightWhite => indexed(15),
            NamedColor::DimBlack => indexed(0),
            NamedColor::DimRed => indexed(1),
            NamedColor::DimGreen => indexed(2),
            NamedColor::DimYellow => indexed(3),
            NamedColor::DimBlue => indexed(4),
            NamedColor::DimMagenta => indexed(5),
            NamedColor::DimCyan => indexed(6),
            NamedColor::DimWhite => indexed(7),
            _ => default(),
        },
    }
}

/// Draw the front tab's panes and the dividers between them. Returns the
/// cursor of the pane the keyboard is in.
fn draw_panes(s: &Server, th: &Theme, area: Rect, buf: &mut Buffer, hits: &mut Vec<(Rect, Hit)>) -> Option<Cursor> {
    let tab = s.tab()?;
    let recolor = s.cfg.theme_terminal;
    // Another terminal may be using this tab, with its panes sized for
    // that one: they are shown here as they are there, as far as they fit.
    let foreign = s.foreign_size();
    if foreign.is_some() {
        return draw_foreign_panes(s, th, tab, area, foreign.unwrap_or_default(), buf, hits);
    }
    let (placed, dividers) = tab.panes.place(area);
    let mut cursor = None;
    let mut focused = area;
    for (id, rect) in &placed {
        let Some(pane) = tab.panes.get(*id) else { continue };
        // A shell on a machine that is not reached: what `ssh` and Astrum
        // have to say about that, in its place.
        if let Some(far) = s.cover_of(pane) {
            let here = *id == tab.panes.focus;
            hits.push((*rect, if here { Hit::Pane } else { Hit::OtherPane(*id) }));
            let at = draw_cover(far, *rect, buf, th, recolor, here.then_some((&s.ui.hover, &mut *hits)));
            if here {
                cursor = at;
                focused = *rect;
            }
            continue;
        }
        if *id == tab.panes.focus {
            hits.push((*rect, Hit::Pane));
            cursor = draw_pane(pane, *rect, buf, th, recolor, Some((&s.ui.hover, &mut *hits)));
            // The link under the pointer stands out.
            if let Some(link) = &s.ui.link {
                light_link(pane, link, *rect, buf, th);
            }
            focused = *rect;
        } else {
            draw_pane(pane, *rect, buf, th, recolor, None);
            hits.push((*rect, Hit::OtherPane(*id)));
        }
    }
    // A divider is lit where it runs along the pane the keyboard is in.
    let background = if recolor && !th.flat { th.term_bg } else { Color::Reset };
    for d in &dividers {
        for y in d.rect.y..d.rect.y + d.rect.height {
            for x in d.rect.x..d.rect.x + d.rect.width {
                let beside = if d.vertical {
                    (x + 1 == focused.x || x == focused.x + focused.width) && y >= focused.y && y < focused.y + focused.height
                } else {
                    (y + 1 == focused.y || y == focused.y + focused.height) && x >= focused.x && x < focused.x + focused.width
                };
                let hovered = s.ui.hover == Some(Hit::Border(d.split));
                let color = if beside || hovered { th.accent } else { th.border };
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.reset();
                    cell.set_symbol(if d.vertical { "│" } else { "─" });
                    cell.set_style(Style::default().fg(color).bg(background));
                }
            }
        }
        hits.push((d.rect, Hit::Border(d.split)));
    }
    cursor
}

/// Draw, in place of a pane, the little terminal of the machine it is
/// waiting for: `ssh`'s questions, Astrum's notes, and along the bottom
/// the buttons there are (for the pane the keyboard is in).
fn draw_cover(far: &crate::server::far::Far, area: Rect, buf: &mut Buffer, th: &Theme, recolor: bool, live: Option<(&Option<Hit>, &mut Vec<(Rect, Hit)>)>) -> Option<Cursor> {
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line};
    if area.width == 0 || area.height == 0 {
        return None;
    }
    let base = if recolor && !th.flat { Style::default().fg(th.term_fg).bg(th.term_bg) } else { Style::default() };
    fill(buf, area, base);
    let grid = far.cover.grid();
    let at = grid.cursor.point;
    // The row the cursor is on is kept in sight, above the buttons.
    let room = area.height.saturating_sub(2).max(1) as i32;
    let first = (at.line.0 + 1 - room).max(0);
    for row in 0..room.min(grid.screen_lines() as i32 - first) {
        for col in 0..(area.width as usize).min(grid.columns()) {
            let cell = &grid[Line(first + row)][Column(col)];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let Some(out) = buf.cell_mut((area.x + col as u16, area.y + row as u16)) else { continue };
            let mut style = Style::default().fg(term_color(cell.fg, th, recolor, true)).bg(term_color(cell.bg, th, recolor, false));
            if cell.flags.contains(Flags::BOLD) {
                style = style.add_modifier(Modifier::BOLD);
            }
            out.set_char(if cell.c.is_control() { ' ' } else { cell.c });
            out.set_style(style);
        }
    }
    let (hover, hits) = live?;
    let (y, end) = (area.y + area.height - 1, area.x + area.width);
    let mut x = area.x + 1;
    for (button, label) in far.buttons() {
        let hit = Hit::Far(*button);
        let style = if *hover == Some(hit.clone()) { chip_style(th, th.accent2, th.on_accent) } else { chip_style(th, th.accent, th.on_accent) };
        let start = x;
        x = put(buf, x, y, &format!(" {label} "), style, end);
        hits.push((Rect::new(start, y, x - start, 1), hit));
        x += 2;
    }
    let shown = at.line.0 - first;
    (shown >= 0 && shown < room && at.column.0 < area.width as usize).then(|| Cursor { x: area.x + at.column.0 as u16, y: area.y + shown as u16, shape: 0 })
}

/// Draw a pane. `live` is given for the pane the keyboard is in: what the
/// pointer is over, and where to record what can be clicked.
/// A tab that is laid out for another terminal's window: its panes at that
/// size, cut off at the edges of this window or padded out, with a note.
/// The first click or key here brings the tab over.
fn draw_foreign_panes(s: &Server, th: &Theme, tab: &Tab, area: Rect, size: (u16, u16), buf: &mut Buffer, hits: &mut Vec<(Rect, Hit)>) -> Option<Cursor> {
    let recolor = s.cfg.theme_terminal;
    let background = if recolor && !th.flat { th.term_bg } else { Color::Reset };
    // The part of this window the other layout does not reach.
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.reset();
                cell.set_symbol("·");
                cell.set_style(Style::default().fg(th.border).bg(background));
            }
        }
    }
    let within = |r: Rect| -> Option<Rect> {
        let (right, bottom) = ((r.x + r.width).min(area.x + area.width), (r.y + r.height).min(area.y + area.height));
        (r.x < right && r.y < bottom).then(|| Rect::new(r.x, r.y, right - r.x, bottom - r.y))
    };
    let (placed, dividers) = tab.panes.place(Rect::new(area.x, area.y, size.0, size.1));
    let mut cursor = None;
    for (id, rect) in &placed {
        let (Some(pane), Some(rect)) = (tab.panes.get(*id), within(*rect)) else { continue };
        fill(buf, rect, Style::default().bg(background));
        let drawn = draw_pane(pane, rect, buf, th, recolor, None);
        if *id == tab.panes.focus {
            cursor = drawn;
        }
    }
    for d in &dividers {
        let Some(rect) = within(d.rect) else { continue };
        for y in rect.y..rect.y + rect.height {
            for x in rect.x..rect.x + rect.width {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.reset();
                    cell.set_symbol(if d.vertical { "│" } else { "─" });
                    cell.set_style(Style::default().fg(th.border).bg(background));
                }
            }
        }
    }
    let note = " in use in another window · click or type to bring it here ";
    let w = (width(note) as u16).min(area.width);
    put(buf, area.x + area.width - w, area.y, note, chip_style(th, th.yellow, th.on_accent), area.x + area.width);
    hits.push((area, Hit::Pane));
    cursor
}

fn draw_pane(pane: &Pane, area: Rect, buf: &mut Buffer, th: &Theme, recolor: bool, live: Option<(&Option<Hit>, &mut Vec<(Rect, Hit)>)>) -> Option<Cursor> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    if recolor && !th.flat {
        fill(buf, area, Style::default().fg(th.term_fg).bg(th.term_bg));
    }
    let content = pane.term.renderable_content();
    let offset = content.display_offset as i32;
    let selection = content.selection;
    let mut sym = String::new();
    // Most frames differ from the last by a row or two of one pane: a key
    // echoed, a line of output. So the cells a pane was last drawn as are
    // kept, and only the rows its program has written to since are worked
    // out again; the rest are copied. That holds while the pane is shown
    // as the program left it. With a selection, a search, or the history
    // on screen, it is worked out afresh every time, as it always was.
    let (wide, high) = (area.width as usize, area.height as usize);
    let made_for = (area.width, area.height, recolor, th.flat);
    let mut drawn = pane.drawn.borrow_mut();
    let plain = selection.is_none() && pane.copy.is_none() && offset == 0;
    // (When every row has been written to, as with output pouring past or
    // a program repainting its whole screen, nothing would be saved by
    // keeping a copy: such a frame is worked out straight into place.)
    let keeping = plain && !drawn.all;
    let kept = keeping && drawn.made_for == Some(made_for) && drawn.cells.len() == wide * high;
    // Which rows to work out, and where the cells go: into the kept copy
    // (then copied to the frame), or straight into the frame.
    let rows: Option<Vec<bool>> = kept.then(|| std::mem::take(&mut drawn.rows));
    drawn.rows.clear();
    drawn.all = !plain;
    drawn.made_for = keeping.then_some(made_for);
    if keeping && !kept {
        drawn.cells.clear();
        let blank = if recolor && !th.flat { Style::default().fg(th.term_fg).bg(th.term_bg) } else { Style::default() };
        let mut cell = ratatui::buffer::Cell::EMPTY;
        cell.set_style(blank);
        drawn.cells.resize(wide * high, cell);
    }
    let frame_wide = buf.area.width as usize;
    let frame_at = (area.y - buf.area.y) as usize * frame_wide + (area.x - buf.area.x) as usize;
    let (target, stride, origin): (&mut [ratatui::buffer::Cell], usize, usize) = if keeping { (&mut drawn.cells[..], wide, 0) } else { (&mut buf.content[..], frame_wide, frame_at) };
    // Looking back through the pane: what a search has found, and the cursor.
    let find = pane.copy.as_ref().and_then(|c| c.find.as_ref());
    let block = pane.copy.as_ref().filter(|c| !c.typing).and(pane.copy_cursor());
    let at = pane.copy_cursor();
    let mut next_match = 0;
    // A double-width character takes two cells of the grid: itself, and a
    // spacer after it. Inserting and deleting can part the two, and then
    // neither half can be drawn as it is: on the terminal a wide character
    // would cover its neighbor, and what Astrum believes is on the screen
    // would no longer be. A half without the other is drawn as a blank
    // that keeps its colors.
    // The cells to work out: every one on screen; or, with rows kept from
    // last time, only those of the rows written to since. (Then nothing
    // of the history is showing, and a row of the screen is that row of
    // the grid.)
    use alacritty_terminal::index::{Column, Line, Point};
    let grid = pane.term.grid();
    let every = content.display_iter.map(|indexed| (indexed.point, indexed.cell));
    let written = rows.iter().flat_map(|rows| rows.iter().enumerate()).filter(|(row, written)| **written && *row < high.min(pane.rows as usize)).flat_map(|(row, _)| {
        let line = Line(row as i32);
        (0..wide.min(pane.cols as usize)).map(move |col| (Point::new(line, Column(col)), &grid[line][Column(col)]))
    });
    let mut cells: std::iter::Peekable<Box<dyn Iterator<Item = (Point, &alacritty_terminal::term::cell::Cell)>>> = if rows.is_some() { (Box::new(written) as Box<dyn Iterator<Item = _>>).peekable() } else { (Box::new(every) as Box<dyn Iterator<Item = _>>).peekable() };
    let mut wide_before = None;
    while let Some((point, cell)) = cells.next() {
        let row = point.line.0 + offset;
        let col = point.column.0;
        let mut flags = cell.flags;
        let after_wide = wide_before == Some((point.line, col.wrapping_sub(1)));
        wide_before = flags.contains(Flags::WIDE_CHAR).then_some((point.line, col));
        if row < 0 || row >= area.height as i32 || col >= area.width as usize {
            continue;
        }
        let whole = flags.contains(Flags::WIDE_CHAR)
            && col + 1 < area.width as usize
            && cells.peek().is_some_and(|(next, after)| next.line == point.line && after.flags.contains(Flags::WIDE_CHAR_SPACER));
        let lone = (flags.contains(Flags::WIDE_CHAR) && !whole) || (flags.contains(Flags::WIDE_CHAR_SPACER) && !after_wide);
        // (The spacer of a whole one is drawn like any cell: a blank in the
        // character's colors, which `pair_wide_cells` then ties to it.)
        if lone {
            flags.remove(Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER);
        }
        let Some(out) = target.get_mut(origin + row as usize * stride + col) else { continue };
        // A cell of a picture is carried through as it is; what is put
        // there is settled once the whole frame is drawn (`settle_pictures`).
        if let (crate::image::PLACEHOLDER, TermColor::Spec(fg), TermColor::Spec(bg)) = (cell.c, cell.fg, cell.bg) {
            out.reset();
            out.set_char(crate::image::PLACEHOLDER);
            out.set_style(Style::default().fg(Color::Rgb(fg.r, fg.g, fg.b)).bg(Color::Rgb(bg.r, bg.g, bg.b)));
            continue;
        }
        let mut style = Style::default().fg(term_color(cell.fg, th, recolor, true)).bg(term_color(cell.bg, th, recolor, false));
        let mut m = Modifier::empty();
        if flags.contains(Flags::BOLD) {
            m |= Modifier::BOLD;
        }
        if flags.contains(Flags::ITALIC) {
            m |= Modifier::ITALIC;
        }
        if flags.intersects(Flags::ALL_UNDERLINES) {
            m |= Modifier::UNDERLINED;
        }
        if flags.contains(Flags::DIM) {
            m |= Modifier::DIM;
        }
        if flags.contains(Flags::INVERSE) {
            m |= Modifier::REVERSED;
        }
        if flags.contains(Flags::HIDDEN) {
            m |= Modifier::HIDDEN;
        }
        if flags.contains(Flags::STRIKEOUT) {
            m |= Modifier::CROSSED_OUT;
        }
        // What a program marked as a link looks like one.
        if cell.hyperlink().is_some() {
            m |= Modifier::UNDERLINED;
        }
        if selection.is_some_and(|sel| sel.contains(point)) {
            if th.flat {
                m ^= Modifier::REVERSED;
            } else {
                style = style.bg(th.selection);
                if style.fg == Some(Color::Reset) {
                    style = style.fg(th.text);
                }
                m.remove(Modifier::REVERSED);
            }
        }
        if let Some(find) = find {
            while find.visible.get(next_match).is_some_and(|found| *found.end() < point) {
                next_match += 1;
            }
            if let Some(found) = find.visible.get(next_match).filter(|found| found.contains(&point)) {
                // The match the cursor is on stands out from the others.
                let current = Some(*found.start()) == at;
                if th.flat {
                    m ^= Modifier::REVERSED;
                    if current {
                        m |= Modifier::BOLD | Modifier::UNDERLINED;
                    }
                } else {
                    style = style.bg(if current { th.accent } else { th.yellow }).fg(th.on_accent);
                    m.remove(Modifier::REVERSED);
                }
            }
        }
        if block == Some(point) {
            if th.flat {
                m ^= Modifier::REVERSED;
            } else {
                style = style.bg(th.text).fg(th.bg);
                m.remove(Modifier::REVERSED);
            }
        }
        style = style.add_modifier(m);
        let c = if cell.c == '\0' || cell.c == '\t' { ' ' } else { cell.c };
        // (A kept cell holds what was there before: it starts again from nothing.)
        out.reset();
        if lone {
            out.set_char(' ');
            out.set_style(style);
            continue;
        }
        match cell.zerowidth() {
            Some(extra) if !extra.is_empty() => {
                sym.clear();
                sym.push(c);
                sym.extend(extra.iter());
                out.set_symbol(&sym);
            }
            _ => {
                out.set_char(c);
            }
        }
        out.set_style(style);
    }
    if keeping {
        for row in 0..high {
            let to = frame_at + row * frame_wide;
            if let (Some(to), Some(from)) = (buf.content.get_mut(to..to + wide), drawn.cells.get(row * wide..(row + 1) * wide)) {
                to.clone_from_slice(from);
            }
        }
    }
    drop(drawn);

    if let Some(copy) = &pane.copy {
        return draw_copy_bar(copy, area, buf, th, live);
    }
    if offset > 0 {
        let label = format!(" ↑ {offset} lines back · scroll down or type to return ");
        let w = (width(&label) as u16).min(area.width);
        put(buf, area.x + area.width - w, area.y, &label, chip_style(th, th.yellow, th.on_accent), area.x + area.width);
        return None;
    }
    let cur = content.cursor;
    if cur.shape == CursorShape::Hidden {
        return None;
    }
    let (cx, cy) = (cur.point.column.0 as u16, cur.point.line.0);
    if cy < 0 || cy as u16 >= area.height || cx >= area.width {
        return None;
    }
    let style = pane.term.cursor_style();
    let shape = match (style.shape, style.blinking) {
        // An untouched cursor keeps whatever the host terminal uses.
        (CursorShape::Block, false) => 0,
        (CursorShape::Block, true) => 1,
        (CursorShape::Underline, true) => 3,
        (CursorShape::Underline, false) => 4,
        (CursorShape::Beam, true) => 5,
        (CursorShape::Beam, false) => 6,
        _ => 0,
    };
    Some(Cursor { x: area.x + cx, y: area.y + cy as u16, shape })
}

/// Show the cells of a link as something to click: underlined, in the
/// accent color.
fn light_link(pane: &Pane, link: &crate::pane::Link, area: Rect, buf: &mut Buffer, th: &Theme) {
    let content = pane.term.renderable_content();
    let offset = content.display_offset as i32;
    for indexed in content.display_iter {
        let on = match &link.cells {
            Some(cells) => cells.contains(&indexed.point),
            None => indexed.cell.hyperlink().is_some_and(|h| h.uri() == link.uri),
        };
        let (row, col) = (indexed.point.line.0 + offset, indexed.point.column.0);
        if !on || row < 0 || row >= area.height as i32 || col >= area.width as usize {
            continue;
        }
        if let Some(cell) = buf.cell_mut((area.x + col as u16, area.y + row as u16)) {
            let style = cell.style().add_modifier(Modifier::UNDERLINED);
            cell.set_style(if th.flat { style.add_modifier(Modifier::BOLD) } else { style.fg(th.accent2) });
        }
    }
}

/// The key of a label such as `^B ←`, without the prefix it shares with
/// the labels beside it.
fn short(label: &str) -> String {
    label.rsplit(' ').next().unwrap_or(label).to_string()
}

/// The bar along the top of a pane that is being looked back through: the
/// search with its buttons, or a reminder of the cursor's keys.
fn draw_copy_bar(copy: &CopyMode, area: Rect, buf: &mut Buffer, th: &Theme, live: Option<(&Option<Hit>, &mut Vec<(Rect, Hit)>)>) -> Option<Cursor> {
    let base = Style::default().fg(th.text).bg(th.surface);
    let button = |label: &str, hit: Hit, hover: &Option<Hit>| {
        let style = hover_bg(th, base.fg(th.accent2).add_modifier(Modifier::BOLD), hover.as_ref() == Some(&hit));
        (label.to_string(), style, Some(hit))
    };
    let none = None;
    let hover = live.as_ref().map(|(h, _)| *h).unwrap_or(&none);
    let mut parts: Vec<(String, Style, Option<Hit>)> = Vec::new();
    let hint: String;
    // Where the search text ends, counted in cells from the start of the bar.
    let mut caret = None;
    match &copy.find {
        Some(find) => {
            let room = (area.width as usize).saturating_sub(30).clamp(8, 40);
            let shown = truncate_left(&find.query, room);
            let field = format!(" find: {shown:<12} ");
            caret = Some(width(" find: ") + width(&shown));
            parts.push((field, base, Some(Hit::FindBox)));
            let count = match (find.query.is_empty(), find.total, find.index) {
                (true, _, _) => String::new(),
                (_, 0, _) => " no match ".into(),
                (_, n, _) if n > crate::pane::MAX_COUNTED => format!(" {}+ ", crate::pane::MAX_COUNTED),
                (_, n, 0) => format!(" {n} found "),
                (_, n, i) => format!(" {i} of {n} "),
            };
            let color = if find.total == 0 && !find.query.is_empty() { th.red } else { th.subtext };
            parts.push((count, base.fg(color), None));
            parts.push(button(" ↑ ", Hit::FindOlder, hover));
            parts.push(button(" ↓ ", Hit::FindNewer, hover));
            hint = match (copy.typing, copy.search_only) {
                (true, true) => " enter older · ↑↓ step · esc close ".into(),
                (true, false) => " enter done · esc back ".into(),
                (false, _) => " n older · N newer · v select · y copy · q quit ".into(),
            };
        }
        None => {
            parts.push((" COPY ".into(), chip_style(th, th.yellow, th.on_accent), None));
            hint = " arrows move · v select · y copy · / find · q quit ".into();
        }
    }
    parts.push(button(" ✕ ", Hit::CopyClose, hover));

    let parts_w: usize = parts.iter().map(|(t, _, _)| width(t)).sum();
    let end = area.x + area.width;
    let with_hint = parts_w + width(&hint) <= area.width as usize;
    let total = (parts_w + if with_hint { width(&hint) } else { 0 }).min(area.width as usize) as u16;
    let mut x = end - total;
    if with_hint {
        x = put(buf, x, area.y, &hint, base.fg(th.muted), end);
    }
    let mut cursor = None;
    let mut live = live;
    for (label, style, hit) in parts {
        let start = x;
        x = put(buf, x, area.y, &label, style, end);
        if hit == Some(Hit::FindBox) && copy.typing {
            if let Some(cells) = caret {
                let cx = start + cells as u16;
                if cx < end {
                    cursor = Some(Cursor { x: cx, y: area.y, shape: 0 });
                }
            }
        }
        if let (Some(hit), Some((_, hits))) = (hit, live.as_mut()) {
            hits.push((Rect::new(start, area.y, x - start, 1), hit));
        }
    }
    cursor
}

// ---- status bar ---------------------------------------------------------------

fn draw_status(s: &Server, th: &Theme, area: Rect, buf: &mut Buffer, hits: &mut Vec<(Rect, Hit)>) {
    let bar = Style::default().fg(th.subtext).bg(th.bar);
    fill(buf, area, bar);
    let y = area.y;
    let end = area.x + area.width;
    let Some(tab) = s.tab() else { return };

    // Left: which thing the keyboard is talking to.
    let (label, color) = match tab.focus {
        Focus::Shell => (" SHELL ", th.accent2),
        Focus::Agent => (" ✦ AGENT ", th.accent),
    };
    let mut x = put(buf, area.x, y, label, chip_style(th, color, th.on_accent), end);
    hits.push((Rect::new(area.x, y, x - area.x, 1), Hit::Mode));

    // Over a link: where it goes, and how to follow it.
    if let Some(link) = s.ui.link.as_ref().filter(|_| s.ui.prefix.is_none()) {
        let how = "  Ctrl+click or right-click to open ";
        let room = (end.saturating_sub(x) as usize).saturating_sub(width(how) + 4);
        if room >= 12 {
            x = put(buf, x, y, " ↗ ", bar.fg(th.accent2), end);
            x = put(buf, x, y, &truncate(&link.uri, room), bar.fg(th.text).add_modifier(Modifier::UNDERLINED), end);
            put(buf, x, y, how, bar.fg(th.muted), end);
            return;
        }
    }

    if s.ui.prefix.is_some() {
        x = put(buf, x, y, " PREFIX ", chip_style(th, th.yellow, th.on_accent), end);
        // Each entry can be clicked as well as typed. What does not fit is
        // one click away, in the menu.
        let more = " ≡ more ";
        // What has a key to press after the prefix, with that key.
        let entries: Vec<(Action, String, &str)> = OFFERED
            .iter()
            .filter_map(|&(action, name, _)| s.keymap.prefix_key(action).map(|key| (action, key.label(), name)))
            .collect();
        let entry_w = |key: &str, name: &str| (width(name) + width(key)) as u16 + 3;
        let all: u16 = entries.iter().map(|(_, k, n)| entry_w(k, n)).sum();
        let everything_fits = x + all + 1 <= end;
        let limit = if everything_fits { end } else { end.saturating_sub(width(more) as u16) };
        for (action, key, name) in &entries {
            if x + entry_w(key, name) > limit {
                break;
            }
            let hovered = s.ui.hover == Some(Hit::Do(*action));
            let start = x;
            x = put(buf, x, y, &format!(" {key}"), hover_bg(th, bar.fg(th.yellow).add_modifier(Modifier::BOLD), hovered), end);
            x = put(buf, x, y, &format!(" {name} "), hover_bg(th, bar.fg(th.text), hovered), end);
            hits.push((Rect::new(start, y, x - start, 1), Hit::Do(*action)));
        }
        // (Said only while those keys are the ones they started as.)
        let also = "  1-9 tab · arrows pane";
        let as_they_were = s.keymap.specs(Action::Tab(0)) == ["1"] && s.keymap.specs(Action::FocusLeft) == ["left"];
        if everything_fits && as_they_were {
            // The two that are keys only, when there is room to say so.
            if x + width(also) as u16 <= end {
                put(buf, x, y, also, bar.fg(th.muted), end);
            }
        } else {
            let start = end.saturating_sub(width(more) as u16).max(x);
            let stop = put(buf, start, y, more, hover_bg(th, bar.fg(th.accent2), s.ui.hover == Some(Hit::Menu)), end);
            hits.push((Rect::new(start, y, stop - start, 1), Hit::Menu));
        }
        return;
    }

    // Right side, built first so the left can use what remains.
    let auto = if tab.agent.session_auto { Autonomy::Auto } else { s.cfg.agent.autonomy };
    let (auto_label, auto_color) = match auto {
        Autonomy::Ask => (" ask first ".to_string(), th.green),
        Autonomy::Safe => (" auto read-only ".to_string(), th.blue),
        Autonomy::Auto => (" ▶▶ autopilot ".to_string(), th.yellow),
    };
    let model = if s.cfg.agent.model.is_empty() { "choose model".to_string() } else { truncate(&s.cfg.agent.model, 28) };
    let model_label = format!(" ◆ {model} ");
    let help_key = s.keymap.label(Action::Help, &s.prefix.label());
    let help_label = if help_key.is_empty() { " help ".to_string() } else { format!(" {help_key} help ") };
    let menu_label = " ≡ menu ".to_string();
    let hint = match tab.focus {
        Focus::Shell => format!(" {} agent ", s.agent_key.label()),
        Focus::Agent => format!(" {} shell ", s.agent_key.label()),
    };
    let mut right: Vec<(String, Style, Option<Hit>)> = Vec::new();
    right.push((hint, bar.fg(th.muted), Some(Hit::Mode)));
    right.push((model_label, hover_bg(th, bar.fg(th.text), s.ui.hover == Some(Hit::Model)), Some(Hit::Model)));
    right.push((auto_label, hover_bg(th, bar.fg(auto_color), s.ui.hover == Some(Hit::Autonomy)), Some(Hit::Autonomy)));
    right.push((help_label, hover_bg(th, bar.fg(th.muted), s.ui.hover == Some(Hit::Help)), Some(Hit::Help)));
    right.push((menu_label, hover_bg(th, bar.fg(th.accent2), s.ui.hover == Some(Hit::Menu)), Some(Hit::Menu)));
    // Drop the least important pieces on narrow terminals.
    let total = |r: &Vec<(String, Style, Option<Hit>)>| r.iter().map(|(t, _, _)| width(t) as u16).sum::<u16>();
    while right.len() > 1 && x + total(&right) + 2 > end {
        right.remove(0);
    }
    let right_w = total(&right);
    let mut rx = end.saturating_sub(right_w);
    let left_end = rx.saturating_sub(1);

    // Offers that fit the moment: a command just failed, or one is running.
    let mut offers: Vec<(String, Style, Option<Hit>)> = Vec::new();
    let offer = |label: &str, hit: Hit| {
        let style = hover_bg(th, bar.fg(th.accent2).add_modifier(Modifier::UNDERLINED), s.ui.hover == Some(hit.clone()));
        (label.to_string(), style, Some(hit))
    };
    // (And a reminder that the other panes are only hidden.)
    if tab.panes.zoomed {
        offers.push((format!("  ⤢ zoomed · 1 of {} panes", tab.panes.len()), bar.fg(th.yellow), None));
    }
    if let Some(code) = s.unexplained_failure(s.active) {
        offers.push((format!("  ✗ exit {code} "), bar.fg(th.red), None));
        offers.push(offer("explain", Hit::Explain));
        offers.push((" ".into(), bar, None));
        offers.push(offer("fix", Hit::Fix));
    } else if tab.panes.focused().offers_inner() {
        // In `ssh`, or something else that ends in another shell: the
        // agent can be taken in there.
        offers.push(("  ".into(), bar, None));
        offers.push(offer("take the agent in", Hit::Do(Action::AgentHere)));
    } else if tab.panes.focused().is_busy() && !tab.agent.busy && !tab.agent.detached {
        offers.push(("  ".into(), bar, None));
        offers.push(offer("watch", Hit::Watch));
    }
    let offers_w: u16 = offers.iter().map(|(t, _, _)| width(t) as u16).sum();
    let offers_fit = x + offers_w + 8 <= left_end;

    // Left: where the shell is. For a shell on another machine, which
    // machine comes first, and its directory is given as it is there.
    let inner = tab.panes.focused().inner();
    // (The same for a tab that is on another machine altogether.)
    let far = tab.panes.focused().there().and_then(|t| s.far(&t.host));
    let far_home = far.and_then(|f| f.there.as_ref()).map(|m| m.home.clone()).filter(|h| h.len() > 1);
    if let Some(inner) = inner {
        x = put(buf, x, y, &format!(" ⇢ {}", truncate(&inner.label(), 28)), bar.fg(th.accent2), left_end);
    } else if let Some(there) = tab.panes.focused().there() {
        let who = match far.and_then(|f| f.there.as_ref()) {
            Some(m) if !m.host.is_empty() => format!("{}@{}", m.user, m.host),
            // (Before it has said: the machine as it was named, without
            // whatever options were given with it.)
            _ => there.host.split_whitespace().last().unwrap_or(&there.host).to_string(),
        };
        x = put(buf, x, y, &format!(" ⇢ {}", truncate(&who, 28)), bar.fg(th.accent2), left_end);
    }
    if let Some(cwd) = tab.panes.focused().cwd_shown() {
        let room = left_end.saturating_sub(x + 2 + if offers_fit { offers_w } else { 0 }) as usize;
        if room >= 6 {
            let shown = match (&inner, &far_home, tab.panes.focused().there()) {
                (Some(_), ..) => cwd.to_string(),
                // (Its home folder is not this machine's.)
                (None, Some(home), Some(_)) => match cwd.strip_prefix(home.as_str()) {
                    Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
                    _ => cwd.to_string(),
                },
                (None, None, Some(_)) => cwd.to_string(),
                _ => paths::tildify(cwd),
            };
            x = put(buf, x, y, &format!(" {}", truncate_left(&shown, room)), bar, left_end);
        }
    }
    if offers_fit {
        for (label, style, hit) in offers {
            let start = x;
            x = put(buf, x, y, &label, style, left_end);
            if let Some(h) = hit {
                hits.push((Rect::new(start, y, x - start, 1), h));
            }
        }
    }
    if tab.agent.detached {
        put(buf, x, y, "  ⧗ the agent is following a running command", bar.fg(th.yellow), left_end);
    } else if s.cover_of(tab.panes.focused()).is_some() {
        // (Nothing to say of a shell that is not reached yet.)
    } else if !tab.panes.focused().integrated && tab.panes.focused().shell_name != "zsh" && tab.panes.focused().shell_name != "bash" && tab.panes.focused().shell_name != "fish" {
        put(buf, x, y, "  (no shell integration)", bar.fg(th.muted), left_end);
    }

    if x + right_w <= end {
        for (label, style, hit) in right {
            let start = rx;
            rx = put(buf, rx, y, &label, style, end);
            if let Some(h) = hit {
                hits.push((Rect::new(start, y, rx - start, 1), h));
            }
        }
    }
}

// ---- agent panel --------------------------------------------------------------

fn md_styles(th: &Theme) -> Styles {
    let base = Style::default().fg(th.text).bg(th.bg);
    Styles {
        text: base,
        bold: base.add_modifier(Modifier::BOLD),
        italic: base.add_modifier(Modifier::ITALIC),
        code: base.fg(th.accent2),
        code_block: base.fg(th.green),
        heading: base.fg(th.accent).add_modifier(Modifier::BOLD),
        muted: base.fg(th.muted),
        bullet: base.fg(th.accent),
    }
}

fn tool_line(t: &ToolItem, th: &Theme, spinner: &str, max: usize) -> Vec<Line<'static>> {
    let base = Style::default().fg(th.subtext).bg(th.bg);
    let (glyph, gstyle) = match t.state {
        ToolState::Proposed => ("○", base.fg(th.muted)),
        ToolState::Running => (spinner, base.fg(th.accent2)),
        ToolState::Done => ("✓", base.fg(th.green)),
        ToolState::Failed => ("✗", base.fg(th.red)),
        ToolState::Declined => ("⊘", base.fg(th.muted)),
    };
    let verb = match (t.name.as_str(), t.state) {
        ("run_command", ToolState::Proposed) => "Run",
        ("run_command", ToolState::Running) => "Running",
        ("run_command", ToolState::Declined) => "Didn't run",
        ("run_command", _) => "Ran",
        ("read_file", ToolState::Running | ToolState::Proposed) => "Reading",
        ("read_file", _) => "Read",
        ("find_files", ToolState::Running | ToolState::Proposed) => "Finding files",
        ("find_files", _) => "Found files",
        ("search_files", ToolState::Running | ToolState::Proposed) => "Searching for",
        ("search_files", _) => "Searched for",
        ("write_file", ToolState::Done) => "Wrote",
        ("write_file", ToolState::Declined) => "Didn't write",
        ("write_file", _) => "Write",
        ("edit_file", ToolState::Done) => "Edited",
        ("edit_file", ToolState::Declined) => "Didn't edit",
        ("edit_file", _) => "Edit",
        ("read_terminal", _) => "Read the terminal",
        ("command_status", ToolState::Running) => "Waiting on the command",
        ("command_status", _) => "Checked the command",
        ("send_input", ToolState::Declined) => "Didn't type",
        ("send_input", ToolState::Done) => "Typed",
        ("send_input", _) => "Type",
        (other, _) => other,
    };
    let mut segs: Vec<text::Seg> = vec![(format!("{glyph} "), gstyle), (verb.to_string(), base)];
    if !t.target.is_empty() {
        let shown = t.target.lines().next().unwrap_or("");
        let more = if t.target.lines().count() > 1 { " …" } else { "" };
        segs.push((" ".into(), base));
        segs.push((format!("{}{more}", truncate(shown, max.saturating_mul(3))), base.fg(th.accent2)));
    }
    if let Some(code) = t.exit.filter(|c| *c != 0) {
        segs.push((format!("  exit {code}"), base.fg(th.red)));
    }
    let mut lines = text::plain(&segs, max, 2);
    if !t.summary.is_empty() {
        let segs = vec![("  └ ".to_string(), base.fg(th.muted)), (t.summary.clone(), base.fg(th.muted))];
        lines.extend(text::plain(&segs, max, 4));
    }
    lines
}

fn item_lines(item: &Item, th: &Theme, st: &Styles, spinner: &str, max: usize) -> Vec<Line<'static>> {
    let base = Style::default().fg(th.text).bg(th.bg);
    match item {
        Item::User(t) => {
            let mut segs: Vec<text::Seg> = vec![("❯ ".to_string(), base.fg(th.accent).add_modifier(Modifier::BOLD))];
            let mut lines = Vec::new();
            for (i, l) in t.lines().enumerate() {
                if i > 0 {
                    segs = vec![("  ".to_string(), base)];
                }
                segs.push((l.to_string(), base.add_modifier(Modifier::BOLD)));
                lines.extend(text::plain(&segs, max, 2));
            }
            lines
        }
        Item::Assistant(t) => text::markdown(t, st, max),
        Item::Tool(t) => tool_line(t, th, spinner, max),
        Item::Notice(t) => {
            let muted = Styles { text: st.muted, bold: st.muted.add_modifier(Modifier::BOLD), ..*st };
            text::markdown(t, &muted, max)
        }
        Item::Error(t) => {
            let segs = vec![("✗ ".to_string(), base.fg(th.red).add_modifier(Modifier::BOLD)), (t.clone(), base.fg(th.red))];
            text::plain(&segs, max, 2)
        }
    }
}

/// Lay the prompt text out into rows, reporting where the cursor falls.
fn input_rows(text: &str, cursor: usize, max: usize) -> (Vec<String>, usize, usize) {
    let max = max.max(4);
    let mut rows = vec![String::new()];
    let mut w = 0usize;
    let (mut cur_row, mut cur_col) = (0usize, 0usize);
    for (i, c) in text.char_indices() {
        if i == cursor {
            cur_row = rows.len() - 1;
            cur_col = w;
        }
        if c == '\n' {
            rows.push(String::new());
            w = 0;
            continue;
        }
        let cw = c.width().unwrap_or(0);
        if w + cw > max {
            rows.push(String::new());
            w = 0;
            if i == cursor {
                cur_row = rows.len() - 1;
                cur_col = 0;
            }
        }
        rows.last_mut().unwrap().push(c);
        w += cw;
    }
    if cursor >= text.len() {
        cur_row = rows.len() - 1;
        cur_col = w;
        if cur_col >= max {
            rows.push(String::new());
            cur_row += 1;
            cur_col = 0;
        }
    }
    (rows, cur_row, cur_col)
}

fn draw_panel(s: &Server, th: &Theme, area: Rect, buf: &mut Buffer, hits: &mut Vec<(Rect, Hit)>, beside: bool) -> Option<Cursor> {
    let tab = s.tab()?;
    let a: &AgentSession = &tab.agent;
    let base = Style::default().fg(th.text).bg(th.bg);
    fill(buf, area, base);
    hits.push((area, Hit::Panel));
    let end = area.x + area.width;
    let spinner = SPINNER[s.ui.spinner % SPINNER.len()];

    // Header doubles as the drag handle for resizing.
    let head = Style::default().fg(th.border).bg(th.bg);
    let y0 = area.y;
    for x in area.x..end {
        if let Some(c) = buf.cell_mut((x, y0)) {
            c.set_symbol("─");
            c.set_style(head);
        }
    }
    // Below the shell, the header is what is dragged to resize the panel.
    if !beside {
        hits.push((Rect::new(area.x, y0, area.width, 1), Hit::Divider));
    }
    let focused = tab.focus == Focus::Agent;
    let title_style = if focused { base.fg(th.accent).add_modifier(Modifier::BOLD) } else { base.fg(th.subtext) };
    let mut x = put(buf, area.x + 2, y0, " ✦ Astrum agent ", title_style, end);
    if a.totals.requests > 0 {
        // How full the context is, then the conversation's tokens: in,
        // cached, out and in total. On a narrow panel, the first that fits.
        let n = crate::agent::models::tokens;
        let t = &a.totals;
        // Right after a compaction the size is not known until the next reply.
        let context = if t.context == 0 {
            String::new()
        } else if a.window > 0 {
            format!("{} of {} context · ", n(t.context), n(a.window))
        } else {
            format!("{} context · ", n(t.context))
        };
        let total = format!("{} total", n(t.total()));
        let choices = [
            format!(" {context}{} in · {} cached · {} out · {total} ", n(t.input), n(t.cache_read), n(t.output)),
            format!(" {} in · {} cached · {} out · {total} ", n(t.input), n(t.cache_read), n(t.output)),
            format!(" {} in · {} out · {total} ", n(t.input), n(t.output)),
            format!(" {total} "),
        ];
        let room = end.saturating_sub(6).saturating_sub(x + 1) as usize;
        if let Some(label) = choices.iter().find(|c| width(c) <= room) {
            x = put(buf, x + 1, y0, label, base.fg(th.muted), end.saturating_sub(6));
        }
    }
    let _ = x;
    if area.width > 12 {
        let cx = end - 5;
        let close = if s.ui.hover == Some(Hit::PanelClose) { base.fg(th.red).add_modifier(Modifier::BOLD) } else { base.fg(th.muted) };
        put(buf, cx, y0, " ✕ ", close, end);
        hits.push((Rect::new(cx, y0, 3, 1), Hit::PanelClose));
    }

    let inner_x = area.x + 1;
    let inner_w = area.width.saturating_sub(2);
    let body_top = area.y + 1;
    let body_bottom = area.y + area.height; // exclusive
    if inner_w < 8 || body_bottom <= body_top {
        return None;
    }
    let max = inner_w as usize;
    let mut cursor = None;
    let mut bottom = body_bottom;

    // ---- bottom region: approval card or prompt ---------------------------
    if let (Some(ap), InputMode::Prompt) = (&a.approval, a.mode) {
        let card = Style::default().fg(th.text).bg(th.surface);
        let edge = if ap.warning.is_some() { th.red } else { th.yellow };
        let mut lines: Vec<(Line<'static>, Option<usize>)> = Vec::new();
        lines.push((Line::from(Span::styled(ap.question.clone(), card.add_modifier(Modifier::BOLD))), None));
        if let Some(w) = &ap.warning {
            lines.push((Line::from(Span::styled(w.clone(), card.fg(th.red).add_modifier(Modifier::BOLD))), None));
        }
        // Choices go on one row when they fit, otherwise one per row.
        let one_row_w: usize = ap.choices.iter().map(|(_, l)| width(l) + 9).sum();
        let horizontal = one_row_w <= max.saturating_sub(2);
        let choice_rows = if horizontal { 1 } else { ap.choices.len() };
        let hint = if horizontal { "←→ choose · Enter confirm · Esc decline" } else { "↑↓ choose · Enter confirm · Esc decline" };

        // A diff gets as much of the panel as there is; a command only needs a few lines.
        let is_diff = ap.edit.is_some();
        let fixed = lines.len() + choice_rows + 1;
        let mut room = (area.height as usize).saturating_sub(fixed + 1).clamp(1, if is_diff { 40 } else { 10 });
        let overflow = ap.preview.len() > room;
        if overflow {
            // One row goes to saying how much more there is.
            room = room.saturating_sub(1).max(1);
        }
        let from = ap.scroll.min(ap.preview.len().saturating_sub(room));
        let is_cmd = ap.call.name == "run_command";
        for (i, p) in ap.preview.iter().enumerate().skip(from).take(room) {
            let style = if p.starts_with("+ ") || p == "+" {
                card.fg(th.green)
            } else if p.starts_with("- ") || p == "-" {
                card.fg(th.red)
            } else if p.starts_with("# ") || p.starts_with("  …") || p.starts_with("@@") {
                card.fg(th.muted)
            } else if is_diff {
                card.fg(th.subtext)
            } else {
                card.fg(th.accent2)
            };
            let prefix = if is_cmd && i == 0 { "$ " } else if is_cmd && !p.starts_with("# ") && !p.is_empty() { "  " } else { "" };
            lines.push((Line::from(Span::styled(truncate(&format!("{prefix}{p}"), max.saturating_sub(3)), style)), None));
        }
        if overflow {
            let below = ap.preview.len() - (from + room).min(ap.preview.len());
            let note = match (is_diff, from > 0, below > 0) {
                (true, _, true) => format!("  … {below} more lines · PgDn / wheel to scroll"),
                (true, _, false) => "  … end of the change · PgUp / wheel to scroll back".to_string(),
                (false, _, _) => format!("  … {below} more lines"),
            };
            lines.push((Line::from(Span::styled(note, card.fg(th.muted))), None));
        }

        let h = ((lines.len() + choice_rows + 1) as u16).min(bottom - body_top);
        let top = bottom - h;
        fill(buf, Rect::new(area.x, top, area.width, h), card);
        for y in top..bottom {
            put(buf, area.x, y, "▌", card.fg(edge), end);
        }
        let mut y = top;
        for (line, _) in lines.into_iter() {
            if y >= bottom.saturating_sub(choice_rows as u16 + 1) {
                break;
            }
            buf.set_line(inner_x + 1, y, &line, inner_w.saturating_sub(1));
            y += 1;
        }
        let mut cx = inner_x + 1;
        for (i, (choice, label)) in ap.choices.iter().enumerate() {
            if y >= bottom {
                break;
            }
            let selected = i == ap.selected;
            let row = if !selected {
                card
            } else if th.flat {
                card.add_modifier(Modifier::REVERSED | Modifier::BOLD)
            } else {
                card.bg(th.overlay).add_modifier(Modifier::BOLD)
            };
            let start = cx;
            cx = put(buf, cx, y, if selected { " ❯ " } else { "   " }, row.fg(th.accent), end);
            cx = put(buf, cx, y, label, row, end);
            cx = put(buf, cx, y, &format!(" ({}) ", choice.key()), row.fg(th.muted), end);
            if horizontal {
                hits.push((Rect::new(start, y, cx - start, 1), Hit::Choice(i)));
                cx += 1;
            } else {
                hits.push((Rect::new(area.x, y, area.width, 1), Hit::Choice(i)));
                cx = inner_x + 1;
                y += 1;
            }
        }
        if horizontal {
            y += 1;
        }
        if y < bottom {
            put(buf, inner_x + 2, y, hint, card.fg(th.muted), end);
        }
        bottom = top;
    } else {
        let (glyph, gcolor, placeholder) = match a.mode {
            InputMode::EditCommand => ("$ ", th.yellow, "Edit the command · Enter runs it · Esc goes back"),
            InputMode::Guidance => ("↳ ", th.yellow, "What should the agent do instead? · Enter sends · Esc just declines"),
            InputMode::Secret => ("⚿ ", th.yellow, "Paste or type the API key · Enter stores it · Esc cancels"),
            InputMode::Prompt if a.busy => ("❯ ", th.muted, "Working… · type to add to this turn · Esc to stop"),
            InputMode::Prompt => ("❯ ", th.accent, "Ask anything · / for commands · ! runs a shell command · Esc closes"),
        };
        let field = Style::default().fg(th.text).bg(th.surface);
        let text_w = max.saturating_sub(3);
        // A key being entered is drawn as dots.
        let masked;
        let (shown_text, shown_cursor) = if a.mode == InputMode::Secret {
            masked = "•".repeat(a.input.text.chars().count());
            (masked.as_str(), a.input.text[..a.input.cursor].chars().count() * "•".len())
        } else {
            (a.input.text.as_str(), a.input.cursor)
        };
        let (rows, cur_row, cur_col) = input_rows(shown_text, shown_cursor, text_w);
        let shown = rows.len().min(6).min((bottom - body_top) as usize).max(1);
        let first = cur_row.saturating_sub(shown - 1).min(rows.len().saturating_sub(shown));
        let top = bottom - shown as u16;
        fill(buf, Rect::new(area.x, top, area.width, shown as u16), field);
        for (i, row) in rows.iter().skip(first).take(shown).enumerate() {
            let y = top + i as u16;
            if i == 0 && first == 0 {
                put(buf, inner_x, y, glyph, field.fg(gcolor).add_modifier(Modifier::BOLD), end);
            }
            put(buf, inner_x + 2, y, row, field, end - 1);
        }
        if a.input.is_empty() {
            put(buf, inner_x + 2, top, &truncate(placeholder, text_w), field.fg(th.muted), end - 1);
        }
        if cur_row >= first && cur_row < first + shown {
            cursor = Some(Cursor { x: inner_x + 2 + cur_col as u16, y: top + (cur_row - first) as u16, shape: 5 });
        }
        bottom = top;

        // Slash-command menu sits directly above the prompt.
        let menu = a.slash_matches();
        if !menu.is_empty() && bottom > body_top + 1 {
            let n = menu.len().min((bottom - body_top - 1) as usize);
            let top = bottom - n as u16;
            let sel = a.slash_sel.min(menu.len() - 1);
            // A short panel shows a window of the menu that follows the selection.
            let first = sel.saturating_sub(n - 1);
            for (i, (name, desc)) in menu.iter().enumerate().skip(first).take(n) {
                let y = top + (i - first) as u16;
                let row = if i == sel {
                    if th.flat { base.add_modifier(Modifier::REVERSED) } else { base.bg(th.overlay) }
                } else {
                    base
                };
                fill(buf, Rect::new(area.x, y, area.width, 1), row);
                let x = put(buf, inner_x + 2, y, &format!("{name:<15}"), row.fg(th.accent2).add_modifier(Modifier::BOLD), end);
                put(buf, x, y, &truncate(desc, (end.saturating_sub(x + 1)) as usize), row.fg(th.subtext), end);
                hits.push((Rect::new(area.x, y, area.width, 1), Hit::Slash(i)));
            }
            bottom = top;
        }
    }

    // ---- messages waiting for the agent's next step -------------------------
    if !a.queued.is_empty() && bottom > body_top + 1 {
        let shown = a.queued.len().min(3).min((bottom - body_top - 1) as usize);
        let top = bottom - shown as u16;
        // The most recent ones, oldest first.
        for (i, text) in a.queued.iter().skip(a.queued.len() - shown).enumerate() {
            let y = top + i as u16;
            let hidden = a.queued.len() - shown;
            let note = if i == 0 && hidden > 0 { format!(" (+{hidden} more)") } else { String::new() };
            let hint = if i + 1 == shown { "  · sent at the next step · Backspace to edit" } else { "" };
            let line = text.lines().next().unwrap_or("");
            let x = put(buf, inner_x, y, "◷ ", base.fg(th.yellow), end);
            let room = (end.saturating_sub(x + 1) as usize).saturating_sub(width(hint) + width(&note));
            let x = put(buf, x, y, &truncate(line, room.max(8)), base.fg(th.subtext), end - 1);
            put(buf, x, y, &format!("{note}{hint}"), base.fg(th.muted), end - 1);
        }
        bottom = top;
    }

    // ---- activity line ----------------------------------------------------
    if a.busy && bottom > body_top {
        let y = bottom - 1;
        let secs = a.started.map(|t| t.elapsed().as_secs()).unwrap_or(0);
        let mut x = put(buf, inner_x, y, &format!("{spinner} "), base.fg(th.accent2), end);
        let status = if a.status.is_empty() { "Working" } else { &a.status };
        if a.approval.is_some() {
            x = put(buf, x, y, "Waiting for your answer", base.fg(th.yellow), end);
        } else {
            x = put(buf, x, y, &format!("{status}… {secs}s"), base.fg(th.subtext), end);
            x = put(buf, x, y, " · Esc to stop", base.fg(th.muted), end);
        }
        if matches!(a.wait, Some(crate::server::session::Wait::Command { .. })) && tab.focus == Focus::Agent {
            x = put(buf, x, y, &format!(" · {} to type in the terminal", s.agent_key.label()), base.fg(th.muted), end);
        }
        if !a.thinking_tail.is_empty() && end > x + 12 {
            let tail: String = a.thinking_tail.split_whitespace().collect::<Vec<_>>().join(" ");
            put(buf, x + 2, y, &truncate_left(&tail, (end - x - 4) as usize), base.fg(th.muted).add_modifier(Modifier::ITALIC), end - 1);
        }
        bottom = y;
    }

    // ---- transcript ---------------------------------------------------------
    if bottom <= body_top {
        return cursor;
    }
    let height = (bottom - body_top) as usize;
    if a.items.is_empty() {
        let lines = [
            ("Astrum agent", base.fg(th.accent).add_modifier(Modifier::BOLD)),
            ("Ask about this machine, or have it do something in the terminal above.", base.fg(th.subtext)),
            ("", base),
            ("/model  pick a provider and model      /theme  change colors", base.fg(th.muted)),
            ("Shift+Tab  how much it may do on its own      !cmd  run a shell command", base.fg(th.muted)),
            ("/resume  continue your last conversation      /conversations  browse them", base.fg(th.muted)),
        ];
        let start = body_top + (height.saturating_sub(lines.len()) / 2) as u16;
        for (i, (t, style)) in lines.iter().enumerate() {
            let y = start + i as u16;
            if y < bottom {
                put(buf, inner_x + 1, y, &truncate(t, max.saturating_sub(1)), *style, end - 1);
            }
        }
        return cursor;
    }
    let st = md_styles(th);
    // Build from the newest item backwards, only as far as is visible.
    let want = height + a.scroll;
    let mut blocks: Vec<(Vec<Line<'static>>, bool)> = Vec::new();
    let mut have = 0usize;
    for item in a.items.iter().rev() {
        let lines = item_lines(item, th, &st, spinner, max.saturating_sub(1));
        have += lines.len() + 1;
        blocks.push((lines, matches!(item, Item::Tool(_))));
        if have >= want + 1 {
            break;
        }
    }
    let mut all: Vec<Line<'static>> = Vec::with_capacity(have);
    let mut prev_tool = false;
    for (i, (block, is_tool)) in blocks.into_iter().rev().enumerate() {
        // Consecutive tool rows read as one compact list.
        if i > 0 && !(prev_tool && is_tool) {
            all.push(Line::default());
        }
        prev_tool = is_tool;
        all.extend(block);
    }
    let scroll = a.scroll.min(all.len().saturating_sub(height));
    let end_idx = all.len() - scroll;
    let start_idx = end_idx.saturating_sub(height);
    let visible = &all[start_idx..end_idx];
    // Sit the conversation at the bottom, next to the prompt.
    let y_start = bottom - visible.len() as u16;
    for (i, line) in visible.iter().enumerate() {
        buf.set_line(inner_x + 1, y_start + i as u16, line, inner_w.saturating_sub(1));
    }
    if scroll > 0 {
        let label = format!(" ↑ scrolled · {scroll} lines below ");
        let w = (width(&label) as u16).min(inner_w);
        put(buf, end - w - 1, body_top, &label, chip_style(th, th.yellow, th.on_accent), end);
    }
    cursor
}

// ---- overlays -----------------------------------------------------------------

fn centered(cols: u16, rows: u16, w: u16, h: u16) -> Rect {
    let w = w.min(cols.saturating_sub(2)).max(10.min(cols));
    let h = h.min(rows.saturating_sub(2)).max(3.min(rows));
    Rect::new((cols - w) / 2, (rows - h) / 2, w, h)
}

fn frame(th: &Theme, area: Rect, title: &str, buf: &mut Buffer) {
    Clear.render(area, buf);
    let style = Style::default().fg(th.text).bg(th.bg);
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(th.accent).bg(th.bg))
        .title(Line::from(Span::styled(format!(" {title} "), style.fg(th.accent).add_modifier(Modifier::BOLD))))
        .style(style)
        .render(area, buf);
}

fn draw_overlay(s: &Server, th: &Theme, buf: &mut Buffer, hits: &mut Vec<(Rect, Hit)>) -> Option<Cursor> {
    let base = Style::default().fg(th.text).bg(th.bg);
    match s.ui.overlay.as_ref()? {
        Overlay::Remote { text } => {
            let area = centered(s.cols, s.rows, 64, 5);
            frame(th, area, "New tab on another machine", buf);
            hits.push((area, Hit::Overlay));
            let end = area.x + area.width - 1;
            put(buf, area.x + 2, area.y + 1, "The machine, as you would name it to ssh:", base.fg(th.muted), end);
            let x = put(buf, area.x + 2, area.y + 2, &truncate_left(text, area.width.saturating_sub(5) as usize), base, end);
            put(buf, area.x + 2, area.y + 3, "Enter opens it · Esc cancels · its shells stay there if the connection drops", base.fg(th.muted), end);
            Some(Cursor { x, y: area.y + 2, shape: 5 })
        }
        Overlay::Rename { text } => {
            let area = centered(s.cols, s.rows, 46, 3);
            frame(th, area, "Rename tab", buf);
            hits.push((area, Hit::Overlay));
            let x = put(buf, area.x + 2, area.y + 1, &truncate_left(text, area.width.saturating_sub(5) as usize), base, area.x + area.width - 1);
            Some(Cursor { x, y: area.y + 1, shape: 5 })
        }
        Overlay::Help => {
            let p = s.prefix.label();
            let k = s.agent_key.label();
            // The key for an action as the keymap has it, or a dash.
            let key = |action: Action| -> String {
                let label = s.keymap.label(action, &p);
                if label.is_empty() { "—".to_string() } else { label }
            };
            let pair = |a: Action, b: Action| format!("{} / {}", key(a), key(b));
            let rows: Vec<(String, &str)> = vec![
                ("Tabs".into(), ""),
                (key(Action::NewTab), "new tab (or click +)"),
                (key(Action::RemoteTab), "new tab on another machine, over ssh"),
                (pair(Action::NextTab, Action::PrevTab), "next / previous tab (or click a tab)"),
                (format!("{} … {}", key(Action::Tab(0)), key(Action::Tab(8)).rsplit(' ').next().unwrap_or("")), "go to tab"),
                (pair(Action::SplitRight, Action::SplitDown), "split the pane: side by side / one above the other"),
                (format!("{} {} {} {}", key(Action::FocusLeft), short(&key(Action::FocusRight)), short(&key(Action::FocusUp)), short(&key(Action::FocusDown))), "move between panes (or click one); drag a divider to resize"),
                (key(Action::Zoom), "zoom: let one pane fill the tab, and back"),
                (key(Action::Close), "close the pane, or the tab when it is the last (or click ✕)"),
                (key(Action::Rename), "rename tab"),
                (pair(Action::MoveTabLeft, Action::MoveTabRight), "move the tab left / right (or drag it)"),
                ("Right-click".into(), "a menu for the tab or pane under the pointer"),
                ("≡ menu".into(), "on the status bar: all of this, to click"),
                (key(Action::Settings), "settings"),
                (key(Action::Detach), "detach: leave everything running, come back with `astrum`"),
                ("".into(), ""),
                ("Agent".into(), ""),
                (k.clone(), "switch between the shell and the agent"),
                (key(Action::AgentPanel), "show or hide the agent panel"),
                ("# question".into(), "at the shell prompt: ask the agent without switching"),
                (pair(Action::Explain, Action::Fix), "explain, or fix, the last command (or click the offer)"),
                (key(Action::Watch), "have the agent watch the command that is running"),
                ("Shift+Tab".into(), "ask first → auto read-only → autopilot"),
                (format!("{}  /model", key(Action::Model)), "choose provider and model"),
                (format!("{}  /theme", key(Action::Theme)), "choose a color theme"),
                ("Esc".into(), "stop the agent, clear the prompt, or close the panel"),
                ("".into(), ""),
                ("Scrolling and copying".into(), ""),
                ("Mouse wheel".into(), "scroll back through output"),
                ("Shift+PageUp/Down".into(), "page through output"),
                ("Drag".into(), "select text (copies on release); double-click a word"),
                ("Shift+drag".into(), "select inside programs that use the mouse"),
                ("Ctrl+click".into(), "open the link under the pointer (or right-click it)"),
                (key(Action::Find), "find in scrollback (or ≡ menu, or right-click the pane)"),
                (key(Action::CopyMode), "copy mode: move with the keys, v select, y copy, q quit"),
                (format!("{p} {p}"), "send the prefix key itself to the shell"),
                ("".into(), ""),
                ("Keys can be changed".into(), ""),
                ("[keys]".into(), "in the config file; `astrum keys` lists every action"),
            ];
            let area = centered(s.cols, s.rows, 76, rows.len() as u16 + 3);
            frame(th, area, "Astrum keys", buf);
            hits.push((area, Hit::Overlay));
            let end = area.x + area.width - 1;
            for (i, (key, what)) in rows.iter().enumerate() {
                let y = area.y + 1 + i as u16;
                if y >= area.y + area.height - 1 {
                    break;
                }
                if what.is_empty() {
                    put(buf, area.x + 2, y, key, base.fg(th.accent).add_modifier(Modifier::BOLD), end);
                } else {
                    put(buf, area.x + 3, y, &format!("{key:<20}"), base.fg(th.accent2), end);
                    put(buf, area.x + 24, y, what, base.fg(th.subtext), end);
                }
            }
            put(buf, area.x + 2, area.y + area.height - 1, " any key to close ", base.fg(th.muted), end);
            None
        }
        Overlay::Picker(p) => draw_picker(s, th, p, buf, hits),
        Overlay::Settings(view) => {
            draw_settings(s, th, view, buf, hits);
            None
        }
        Overlay::Welcome { step } => {
            draw_walkthrough(s, th, *step, buf, hits);
            None
        }
        Overlay::Menu(menu) => {
            draw_menu(s, th, menu, buf, hits);
            None
        }
    }
}

/// The settings screen: headings, and under each its rows, a label on the
/// left and the value on the right. Every row can be clicked.
fn draw_settings(s: &Server, th: &Theme, view: &SettingsView, buf: &mut Buffer, hits: &mut Vec<(Rect, Hit)>) {
    let base = Style::default().fg(th.text).bg(th.bg);
    // The lines of the list: a heading, or a row by its number.
    let mut lines: Vec<(Option<&str>, Option<usize>)> = Vec::new();
    let mut number = 0;
    for (heading, rows) in SECTIONS {
        lines.push((Some(heading), None));
        for _ in rows.iter() {
            lines.push((None, Some(number)));
            number += 1;
        }
    }
    let all = crate::server::settings::rows();
    let area = centered(s.cols, s.rows, 78, lines.len() as u16 + 4);
    frame(th, area, "Settings", buf);
    hits.push((area, Hit::Overlay));
    let end = area.x + area.width - 1;
    // Close, at the top right.
    let close = Rect::new(end.saturating_sub(4), area.y, 3, 1);
    put(buf, close.x, close.y, " ✕ ", hover_bg(th, base.fg(th.muted), s.ui.hover == Some(Hit::SettingsClose)), end);
    hits.push((close, Hit::SettingsClose));

    // As many lines as fit, keeping the chosen row in sight.
    let room = area.height.saturating_sub(4) as usize;
    let chosen = lines.iter().position(|(_, row)| *row == Some(view.selected)).unwrap_or(0);
    let first = if chosen >= room { chosen + 1 - room } else { 0 };
    for (i, (heading, row)) in lines.iter().enumerate().skip(first).take(room) {
        let y = area.y + 1 + (i - first) as u16;
        if let Some(heading) = heading {
            put(buf, area.x + 2, y, heading, base.fg(th.accent).add_modifier(Modifier::BOLD), end);
            continue;
        }
        let Some(row) = *row else { continue };
        let setting = all[row];
        let line = Rect::new(area.x + 1, y, area.width.saturating_sub(2), 1);
        let lit = row == view.selected || s.ui.hover == Some(Hit::Setting(row));
        let style = if !lit {
            base
        } else if th.flat {
            base.add_modifier(Modifier::REVERSED)
        } else {
            base.bg(th.overlay)
        };
        fill(buf, line, style);
        put(buf, area.x + 4, y, setting.label(), style, end);
        let value = if setting.opens() { format!("{} ▸", s.setting_value(setting)) } else { s.setting_value(setting) };
        let value = truncate(value.trim(), (area.width as usize).saturating_sub(width(setting.label()) + 10));
        let vx = end.saturating_sub(width(&value) as u16 + 2);
        put(buf, vx, y, &value, style.fg(if th.flat { Color::Reset } else { th.accent2 }), end);
        hits.push((line, Hit::Setting(row)));
    }
    let foot = area.y + area.height - 2;
    let note = format!("Click a row to change it · Esc closes · everything else: {}", paths::tildify(&paths::config_file().to_string_lossy()));
    put(buf, area.x + 2, foot, &truncate(&note, area.width.saturating_sub(4) as usize), base.fg(th.muted), end);
}

/// What one card of the walkthrough says: its title and its lines.
fn walkthrough_card(s: &Server, step: usize) -> (&'static str, Vec<String>) {
    let prefix = s.prefix.label();
    let key = |action: Action| s.keymap.label(action, &prefix);
    match step {
        0 => (
            "Welcome to Astrum",
            vec![
                "This is your own shell, with your prompt and your colors.".into(),
                "Astrum adds tabs, keeps everything running when you close".into(),
                "the window, and has an assistant that can work alongside you.".into(),
                String::new(),
                "Nothing here needs keyboard shortcuts: everything can be".into(),
                "clicked. The shortcuts are shown wherever they exist.".into(),
            ],
        ),
        1 => (
            "Tabs and panes",
            vec![
                "Click + in the bar at the top for a new tab, ✕ to close one,".into(),
                "and drag a tab to move it.".into(),
                String::new(),
                "Right-click a tab or the shell for a menu: split the pane,".into(),
                "zoom it, find text, rename. Drag the line between panes to".into(),
                "resize them.".into(),
                String::new(),
                format!("≡ menu at the bottom right lists everything ({} opens it too).", key(Action::Menu)),
            ],
        ),
        2 => (
            "The agent",
            vec![
                format!("Click the SHELL chip at the bottom left, or press {},", s.agent_key.label()),
                "to talk to the agent. It sees your terminal and runs commands".into(),
                "in it, where you can watch. It asks first, unless you tell".into(),
                "it otherwise.".into(),
                String::new(),
                "At the shell prompt, a line starting with # and a space is".into(),
                "a question for the agent: # why did that fail".into(),
            ],
        ),
        _ => (
            "Coming back",
            vec![
                "Close the window whenever you like: your shells keep running.".into(),
                "Run astrum again and you are back where you were. After a".into(),
                "restart of the machine, your tabs and their output return.".into(),
                String::new(),
                format!("Settings are under ≡ menu → Settings ({}).", key(Action::Settings)),
                "They can show this walkthrough again.".into(),
            ],
        ),
    }
}

fn draw_walkthrough(s: &Server, th: &Theme, step: usize, buf: &mut Buffer, hits: &mut Vec<(Rect, Hit)>) {
    let base = Style::default().fg(th.text).bg(th.bg);
    let step = step.min(WALKTHROUGH_STEPS - 1);
    let (title, lines) = walkthrough_card(s, step);
    let area = centered(s.cols, s.rows, 70, 14);
    frame(th, area, title, buf);
    hits.push((area, Hit::Overlay));
    let end = area.x + area.width - 1;
    for (i, line) in lines.iter().enumerate() {
        let y = area.y + 2 + i as u16;
        if y + 3 >= area.y + area.height {
            break;
        }
        put(buf, area.x + 3, y, line, base, end);
    }
    // Along the bottom: where we are, and the buttons.
    let y = area.y + area.height - 2;
    let dots: String = (0..WALKTHROUGH_STEPS).map(|i| if i == step { "● " } else { "○ " }).collect();
    put(buf, area.x + 3, y, dots.trim_end(), base.fg(th.muted), end);
    let last = step + 1 == WALKTHROUGH_STEPS;
    let mut buttons: Vec<(&str, Walk, bool)> = Vec::new();
    if !last {
        buttons.push((" Skip ", Walk::Skip, false));
    }
    if step > 0 {
        buttons.push((" Back ", Walk::Back, false));
    }
    if step == 2 && s.cfg.agent.model.is_empty() {
        buttons.push((" Choose a model ", Walk::Model, false));
    }
    buttons.push((if last { " Done " } else { " Next " }, Walk::Next, true));
    let total: u16 = buttons.iter().map(|(label, _, _)| width(label) as u16 + 1).sum();
    let mut x = end.saturating_sub(total + 1);
    for (label, button, main) in buttons {
        let hovered = s.ui.hover == Some(Hit::Walk(button));
        let style = if main { chip_style(th, th.accent, th.on_accent) } else { hover_bg(th, base.fg(th.accent2), hovered) };
        let start = x;
        x = put(buf, x, y, label, style, end);
        hits.push((Rect::new(start, y, x - start, 1), Hit::Walk(button)));
        x += 1;
    }
}

/// A right-click menu: a small box at the pointer, kept inside the window.
fn draw_menu(s: &Server, th: &Theme, menu: &Menu, buf: &mut Buffer, hits: &mut Vec<(Rect, Hit)>) {
    let base = Style::default().fg(th.text).bg(th.bg);
    // A label may carry, after a tab, the key that does the same.
    let parts = |label: &str| -> (String, String) {
        let (name, key) = label.split_once('\t').unwrap_or((label, ""));
        (name.to_string(), key.to_string())
    };
    let label_w = |label: &str| {
        let (name, key) = parts(label);
        width(&name) + if key.is_empty() { 0 } else { width(&key) + 3 }
    };
    let inner = menu.items.iter().map(|(label, _)| label_w(label)).max().unwrap_or(0) as u16 + 4;
    let (w, h) = ((inner + 2).min(s.cols), (menu.items.len() as u16 + 2).min(s.rows));
    let x = menu.at.0.min(s.cols.saturating_sub(w));
    let y = (menu.at.1 + 1).min(s.rows.saturating_sub(h));
    let area = Rect::new(x, y, w, h);
    Clear.render(area, buf);
    Block::bordered().border_type(BorderType::Rounded).border_style(Style::default().fg(th.border).bg(th.bg)).style(base).render(area, buf);
    hits.push((area, Hit::Overlay));
    for (i, (label, _)) in menu.items.iter().enumerate() {
        let row = Rect::new(area.x + 1, area.y + 1 + i as u16, area.width.saturating_sub(2), 1);
        if row.y + 1 >= area.y + area.height {
            break;
        }
        let chosen = i == menu.selected || s.ui.hover == Some(Hit::MenuItem(i));
        let style = if !chosen {
            base
        } else if th.flat {
            base.add_modifier(Modifier::REVERSED)
        } else {
            base.bg(th.overlay).add_modifier(Modifier::BOLD)
        };
        fill(buf, row, style);
        let (name, key) = parts(label);
        put(buf, row.x + 1, row.y, &name, style, row.x + row.width);
        if !key.is_empty() {
            let kx = (row.x + row.width).saturating_sub(width(&key) as u16 + 1);
            put(buf, kx, row.y, &key, style.fg(th.muted), row.x + row.width);
        }
        hits.push((row, Hit::MenuItem(i)));
    }
}

fn draw_picker(s: &Server, th: &Theme, p: &Picker, buf: &mut Buffer, hits: &mut Vec<(Rect, Hit)>) -> Option<Cursor> {
    let base = Style::default().fg(th.text).bg(th.bg);
    let visible = p.visible();
    let list_h = (visible.len().max(1) as u16).min(s.rows.saturating_sub(8)).min(16);
    let area = centered(s.cols, s.rows, 68, list_h + 5);
    frame(th, area, &p.title, buf);
    hits.push((area, Hit::Overlay));
    let end = area.x + area.width - 1;
    let inner_w = area.width.saturating_sub(4) as usize;

    // Filter line.
    let y = area.y + 1;
    let x = put(buf, area.x + 2, y, "❯ ", base.fg(th.accent).add_modifier(Modifier::BOLD), end);
    let fx = put(buf, x, y, &p.filter, base, end);
    if p.filter.is_empty() {
        put(buf, x, y, "type to filter", base.fg(th.muted), end);
    }
    for cx in area.x + 1..end {
        if let Some(c) = buf.cell_mut((cx, y + 1)) {
            c.set_symbol("─");
            c.set_style(base.fg(th.border));
        }
    }

    let list_top = y + 2;
    let rows = area.height.saturating_sub(5) as usize;
    if p.loading {
        let sp = SPINNER[s.ui.spinner % SPINNER.len()];
        put(buf, area.x + 3, list_top, &format!("{sp} Loading…"), base.fg(th.subtext), end);
    } else if visible.is_empty() {
        put(buf, area.x + 3, list_top, "Nothing matches.", base.fg(th.muted), end);
    } else {
        let sel = p.selected.min(visible.len() - 1);
        let first = sel.saturating_sub(rows.saturating_sub(1)).min(visible.len().saturating_sub(rows));
        for (row, &idx) in visible.iter().enumerate().skip(first).take(rows) {
            let item = &p.items[idx];
            let y = list_top + (row - first) as u16;
            let selected = row == sel;
            let style = if selected {
                if th.flat { base.add_modifier(Modifier::REVERSED) } else { base.bg(th.overlay).add_modifier(Modifier::BOLD) }
            } else {
                base
            };
            fill(buf, Rect::new(area.x + 1, y, area.width - 2, 1), style);
            put(buf, area.x + 2, y, if selected { "▸ " } else { "  " }, style.fg(th.accent), end);
            let detail_w = width(&item.detail).min(inner_w / 2);
            let label_w = inner_w.saturating_sub(detail_w + 4);
            put(buf, area.x + 4, y, &truncate(&item.label, label_w), style, end);
            if detail_w > 0 && item.detail != item.label {
                let dx = end.saturating_sub(detail_w as u16 + 1);
                put(buf, dx, y, &truncate(&item.detail, detail_w), style.fg(th.muted).remove_modifier(Modifier::BOLD), end);
            }
            hits.push((Rect::new(area.x + 1, y, area.width - 2, 1), Hit::PickerItem(row)));
        }
    }
    put(buf, area.x + 2, area.y + area.height - 2, &truncate(&p.note, inner_w), base.fg(th.muted), end);
    Some(Cursor { x: fx, y, shape: 5 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::{Cell, CellDiffOption};

    #[test]
    fn a_double_width_character_is_given_its_second_cell() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 8, 3));
        let blue = Style::default().bg(Color::Indexed(4)).add_modifier(Modifier::UNDERLINED);
        // Written the way the panel and the bars write text: the cell
        // after the character is left as nothing at all.
        buf.set_string(0, 0, "a日b", blue);
        // The way a pane writes it: a blank in the same colors after it.
        buf[(0, 1)].set_symbol("語").set_style(blue);
        buf[(1, 1)].set_symbol(" ").set_style(blue);
        // With no room after it, and with something else drawn there.
        buf[(7, 1)].set_symbol("本").set_style(blue);
        buf[(3, 2)].set_symbol("本").set_style(blue);
        buf[(4, 2)].set_symbol("│");
        buf[(6, 2)].set_symbol("本").set_style(blue);
        buf[(7, 2)].set_symbol(" ").set_style(Style::default().bg(Color::Indexed(1)));
        pair_wide_cells(&mut buf);

        for (x, y) in [(2, 0), (1, 1)] {
            let second = &buf[(x, y)];
            assert_eq!((second.symbol(), second.bg, second.modifier, second.diff_option), (" ", Color::Indexed(4), Modifier::UNDERLINED, CellDiffOption::Skip), "at {x},{y}");
        }
        assert_eq!((buf[(1, 0)].symbol(), buf[(0, 1)].symbol(), buf[(3, 0)].symbol()), ("日", "語", "b"));
        // Each of those that cannot be shown is a blank in its own colors,
        // and what was beside it is as it was.
        for (x, y) in [(7, 1), (3, 2), (6, 2)] {
            assert_eq!((buf[(x, y)].symbol(), buf[(x, y)].bg, buf[(x, y)].diff_option), (" ", Color::Indexed(4), CellDiffOption::None), "at {x},{y}");
        }
        assert_eq!((buf[(4, 2)].symbol(), buf[(7, 2)].bg), ("│", Color::Indexed(1)));

        // The point of it: when another double-width character is drawn
        // one cell to the left, the cell the first one's second half was
        // in is sent again, though it is a blank in both frames.
        let mut before = Buffer::empty(Rect::new(0, 0, 6, 1));
        before[(2, 0)].set_symbol("한").set_style(blue);
        before[(3, 0)].set_symbol(" ").set_style(blue);
        let mut after = Buffer::empty(Rect::new(0, 0, 6, 1));
        after[(1, 0)].set_symbol("本");
        pair_wide_cells(&mut before);
        pair_wide_cells(&mut after);
        let sent: Vec<(u16, &str)> = before.diff(&after).into_iter().map(|(x, _, cell)| (x, cell.symbol())).collect();
        assert_eq!(sent, [(1, "本"), (3, " ")]);
        assert_eq!(after[(3, 0)], Cell::EMPTY);
    }
}
