//! Drawing. Every frame is composed into a ratatui `Buffer`; the server then
//! diffs it against the previous frame and ships only the changed cells.

pub mod text;

use crate::config::Autonomy;
use crate::pane::Pane;
use crate::paths;
use crate::server::session::{AgentSession, InputMode, Item, ToolItem, ToolState};
use crate::server::{Focus, Hit, Overlay, Picker, Server, Tab};
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
    pub status: Option<Rect>,
}

pub fn layout(cols: u16, rows: u16, panel: bool, panel_pct: u16, status_bar: bool) -> Layout {
    let status_h = if status_bar && rows >= 6 { 1 } else { 0 };
    let body = rows.saturating_sub(1 + status_h).max(1);
    let panel_h = if panel && body >= 8 {
        ((rows as u32 * panel_pct as u32 / 100) as u16).clamp(7.min(body - 3), body - 3)
    } else if panel {
        body.saturating_sub(1)
    } else {
        0
    };
    let pane_h = body - panel_h;
    Layout {
        tabbar: Rect::new(0, 0, cols, 1),
        pane: Rect::new(0, 1, cols, pane_h),
        panel: (panel_h > 0).then(|| Rect::new(0, 1 + pane_h, cols, panel_h)),
        status: (status_h > 0).then(|| Rect::new(0, rows - 1, cols, 1)),
    }
}

pub fn tab_title(tab: &Tab) -> String {
    if let Some(name) = &tab.name {
        return name.clone();
    }
    if let Some(cmd) = tab.pane.running_cmd() {
        let word = cmd
            .split_whitespace()
            .find(|w| !w.contains('=') && !matches!(*w, "sudo" | "env" | "command" | "exec" | "nohup" | "time"))
            .unwrap_or("");
        let base = word.rsplit('/').next().unwrap_or(word);
        if !base.is_empty() {
            return base.to_string();
        }
    }
    if tab.pane.integrated {
        if let Some(cwd) = &tab.pane.cwd {
            let tilde = paths::tildify(cwd);
            if tilde == "~" || tilde == "/" {
                return tilde;
            }
            return tilde.rsplit('/').next().unwrap_or(&tilde).to_string();
        }
    }
    match &tab.pane.osc_title {
        Some(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => tab.pane.shell_name.clone(),
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
    let l = layout(s.cols, s.rows, panel_visible, s.ui.panel_pct, s.cfg.status_bar);

    draw_tabbar(s, &th, l.tabbar, buf, &mut hits);

    hits.push((l.pane, Hit::Pane));
    let mut cursor = draw_pane(&s.tabs[active].pane, l.pane, buf, &th, s.cfg.theme_terminal);
    if s.tabs[active].focus != Focus::Shell {
        cursor = None;
    }
    if let Some(area) = l.panel {
        let c = draw_panel(s, &th, area, buf, &mut hits);
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
    s.ui.hits = hits;
    cursor
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
        } else if tab.pane.is_busy() && !is_active {
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

fn draw_pane(pane: &Pane, area: Rect, buf: &mut Buffer, th: &Theme, recolor: bool) -> Option<Cursor> {
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
    for indexed in content.display_iter {
        let row = indexed.point.line.0 + offset;
        let col = indexed.point.column.0;
        if row < 0 || row >= area.height as i32 || col >= area.width as usize {
            continue;
        }
        let cell = indexed.cell;
        let flags = cell.flags;
        if flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        let Some(out) = buf.cell_mut((area.x + col as u16, area.y + row as u16)) else { continue };
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
        if selection.is_some_and(|sel| sel.contains(indexed.point)) {
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
        style = style.add_modifier(m);
        let c = if cell.c == '\0' || cell.c == '\t' { ' ' } else { cell.c };
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

    if s.ui.prefix.is_some() {
        x = put(buf, x, y, " PREFIX ", chip_style(th, th.yellow, th.on_accent), end);
        put(buf, x, y, "  c new tab · n/p next/prev · 1-9 go to · x close · , rename · a agent · e explain · f fix · o watch · m model · s theme · d detach · ? help", bar, end);
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
    let help_label = format!(" {} ? help ", s.prefix.label());
    let hint = match tab.focus {
        Focus::Shell => format!(" {} agent ", s.agent_key.label()),
        Focus::Agent => format!(" {} shell ", s.agent_key.label()),
    };
    let mut right: Vec<(String, Style, Option<Hit>)> = Vec::new();
    right.push((hint, bar.fg(th.muted), Some(Hit::Mode)));
    right.push((model_label, hover_bg(th, bar.fg(th.text), s.ui.hover == Some(Hit::Model)), Some(Hit::Model)));
    right.push((auto_label, hover_bg(th, bar.fg(auto_color), s.ui.hover == Some(Hit::Autonomy)), Some(Hit::Autonomy)));
    right.push((help_label, hover_bg(th, bar.fg(th.muted), s.ui.hover == Some(Hit::Help)), Some(Hit::Help)));
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
    if let Some(code) = s.unexplained_failure(s.active) {
        offers.push((format!("  ✗ exit {code} "), bar.fg(th.red), None));
        offers.push(offer("explain", Hit::Explain));
        offers.push((" ".into(), bar, None));
        offers.push(offer("fix", Hit::Fix));
    } else if tab.pane.is_busy() && !tab.agent.busy && !tab.agent.detached {
        offers.push(("  ".into(), bar, None));
        offers.push(offer("watch", Hit::Watch));
    }
    let offers_w: u16 = offers.iter().map(|(t, _, _)| width(t) as u16).sum();
    let offers_fit = x + offers_w + 8 <= left_end;

    // Left: where the shell is.
    if let Some(cwd) = &tab.pane.cwd {
        let room = left_end.saturating_sub(x + 2 + if offers_fit { offers_w } else { 0 }) as usize;
        if room >= 6 {
            x = put(buf, x, y, &format!(" {}", truncate_left(&paths::tildify(cwd), room)), bar, left_end);
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
    } else if !tab.pane.integrated && tab.pane.shell_name != "zsh" && tab.pane.shell_name != "bash" && tab.pane.shell_name != "fish" {
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

fn draw_panel(s: &Server, th: &Theme, area: Rect, buf: &mut Buffer, hits: &mut Vec<(Rect, Hit)>) -> Option<Cursor> {
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
    hits.push((Rect::new(area.x, y0, area.width, 1), Hit::Divider));
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
            InputMode::Prompt if a.busy => ("❯ ", th.muted, "Working… · type to add to this turn · Esc to stop"),
            InputMode::Prompt => ("❯ ", th.accent, "Ask anything · / for commands · ! runs a shell command · Esc closes"),
        };
        let field = Style::default().fg(th.text).bg(th.surface);
        let text_w = max.saturating_sub(3);
        let (rows, cur_row, cur_col) = input_rows(&a.input.text, a.input.cursor, text_w);
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
            let rows: Vec<(String, &str)> = vec![
                ("Tabs".into(), ""),
                (format!("{p} c"), "new tab (or click +)"),
                (format!("{p} n / {p} p"), "next / previous tab (or click a tab)"),
                (format!("{p} 1-9"), "go to tab"),
                (format!("{p} x"), "close tab (or click ✕, or middle-click)"),
                (format!("{p} ,"), "rename tab"),
                (format!("{p} d"), "detach: leave everything running, come back with `astrum`"),
                ("".into(), ""),
                ("Agent".into(), ""),
                (k.clone(), "switch between the shell and the agent"),
                (format!("{p} a"), "show or hide the agent panel"),
                ("# question".into(), "at the shell prompt: ask the agent without switching"),
                (format!("{p} e / {p} f"), "explain, or fix, the last command (or click the offer)"),
                (format!("{p} o"), "have the agent watch the command that is running"),
                ("Shift+Tab".into(), "ask first → auto read-only → autopilot"),
                (format!("{p} m  /model"), "choose provider and model"),
                (format!("{p} s  /theme"), "choose a color theme"),
                ("Esc".into(), "stop the agent, clear the prompt, or close the panel"),
                ("".into(), ""),
                ("Scrolling and copying".into(), ""),
                ("Mouse wheel".into(), "scroll back through output"),
                ("Shift+PageUp/Down".into(), "page through output"),
                ("Drag".into(), "select text (copies on release); double-click a word"),
                ("Shift+drag".into(), "select inside programs that use the mouse"),
                (format!("{p} {p}"), "send the prefix key itself to the shell"),
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
