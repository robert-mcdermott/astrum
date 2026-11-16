//! Text layout for the agent transcript: a small Markdown renderer and a
//! style-preserving word wrapper.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub type Seg = (String, Style);

#[derive(Clone, Copy)]
pub struct Styles {
    pub text: Style,
    pub bold: Style,
    pub italic: Style,
    pub code: Style,
    pub code_block: Style,
    pub heading: Style,
    pub muted: Style,
    pub bullet: Style,
}

pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Shorten to `max` columns, ending with an ellipsis when cut.
/// A line cut into pieces no wider than `max`, so that none of it is hidden.
pub fn pieces(s: &str, max: usize) -> Vec<String> {
    let max = max.max(1);
    let mut out = vec![String::new()];
    let mut used = 0;
    for c in s.chars() {
        let w = width(c.encode_utf8(&mut [0; 4]));
        if used + w > max && used > 0 {
            out.push(String::new());
            used = 0;
        }
        out.last_mut().unwrap().push(c);
        used += w;
    }
    out
}

pub fn truncate(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw + 1 > max {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

/// Keep the end of a string (for paths and running text), with a leading ellipsis.
pub fn truncate_left(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    let mut kept: Vec<char> = Vec::new();
    let mut w = 0;
    for c in s.chars().rev() {
        let cw = c.width().unwrap_or(0);
        if w + cw + 1 > max {
            break;
        }
        kept.push(c);
        w += cw;
    }
    let tail: String = kept.into_iter().rev().collect();
    format!("…{tail}")
}

/// Word-wrap styled segments. Lines after the first get `indent` spaces.
pub fn wrap(segs: &[Seg], max: usize, indent: usize) -> Vec<Vec<Seg>> {
    let max = max.max(8);
    let indent = indent.min(max / 2);
    let mut lines: Vec<Vec<Seg>> = vec![Vec::new()];
    let mut col = 0usize;

    // Tokens are runs of spaces or runs of non-spaces, each with one style.
    let mut tokens: Vec<(String, Style, bool)> = Vec::new();
    for (text, style) in segs {
        let mut cur = String::new();
        let mut cur_space = false;
        for c in text.chars() {
            let is_space = c == ' ';
            if !cur.is_empty() && is_space != cur_space {
                tokens.push((std::mem::take(&mut cur), *style, cur_space));
            }
            cur_space = is_space;
            cur.push(c);
        }
        if !cur.is_empty() {
            tokens.push((cur, *style, cur_space));
        }
    }

    let new_line = |lines: &mut Vec<Vec<Seg>>, col: &mut usize| {
        lines.push(if indent > 0 { vec![(" ".repeat(indent), Style::default())] } else { Vec::new() });
        *col = indent;
    };

    for (text, style, is_space) in tokens {
        let w = width(&text);
        if is_space {
            // Spaces never start a wrapped line.
            if col + w <= max && col > if lines.len() > 1 { indent } else { 0 } {
                lines.last_mut().unwrap().push((text, style));
                col += w;
            } else if col + w > max {
                new_line(&mut lines, &mut col);
            } else if lines.len() == 1 && col == 0 {
                // Leading indentation on the very first line is meaningful.
                lines.last_mut().unwrap().push((text, style));
                col += w;
            }
            continue;
        }
        if col + w <= max {
            lines.last_mut().unwrap().push((text, style));
            col += w;
            continue;
        }
        if w <= max - indent {
            // Drop a trailing space run before wrapping.
            if let Some(last) = lines.last_mut() {
                if last.last().is_some_and(|(t, _)| t.chars().all(|c| c == ' ')) && last.len() > 1 {
                    last.pop();
                }
            }
            new_line(&mut lines, &mut col);
            lines.last_mut().unwrap().push((text, style));
            col += w;
            continue;
        }
        // A word longer than a whole line: split it by characters.
        let mut piece = String::new();
        for c in text.chars() {
            let cw = c.width().unwrap_or(0);
            if col + cw > max {
                if !piece.is_empty() {
                    lines.last_mut().unwrap().push((std::mem::take(&mut piece), style));
                }
                new_line(&mut lines, &mut col);
            }
            piece.push(c);
            col += cw;
        }
        if !piece.is_empty() {
            lines.last_mut().unwrap().push((piece, style));
        }
    }
    lines
}

fn to_line(segs: Vec<Seg>) -> Line<'static> {
    Line::from(segs.into_iter().map(|(t, s)| Span::styled(t, s)).collect::<Vec<_>>())
}

/// Inline Markdown: `code`, **bold**, *italic*.
pub fn inline(text: &str, st: &Styles, base: Style) -> Vec<Seg> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<Seg> = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    let find = |from: usize, pat: &[char]| -> Option<usize> {
        (from..chars.len().saturating_sub(pat.len() - 1)).find(|&j| chars[j..j + pat.len()] == *pat)
    };
    macro_rules! flush {
        () => {
            if !cur.is_empty() {
                out.push((std::mem::take(&mut cur), base));
            }
        };
    }
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            if let Some(end) = find(i + 1, &['`']) {
                flush!();
                out.push((chars[i + 1..end].iter().collect(), st.code));
                i = end + 1;
                continue;
            }
        }
        if c == '*' && chars.get(i + 1) == Some(&'*') {
            if let Some(end) = find(i + 2, &['*', '*']) {
                if end > i + 2 {
                    flush!();
                    let inner: String = chars[i + 2..end].iter().collect();
                    for (t, s) in inline(&inner, st, st.bold) {
                        out.push((t, if s == st.code { s } else { st.bold }));
                    }
                    i = end + 2;
                    continue;
                }
            }
        }
        if (c == '*' || c == '_') && chars.get(i + 1).is_some_and(|n| !n.is_whitespace() && *n != c) {
            let boundary_before = i == 0 || !chars[i - 1].is_alphanumeric();
            if boundary_before {
                if let Some(end) = find(i + 1, &[c]) {
                    let boundary_after = chars.get(end + 1).is_none_or(|n| !n.is_alphanumeric());
                    if end > i + 1 && !chars[end - 1].is_whitespace() && boundary_after {
                        flush!();
                        out.push((chars[i + 1..end].iter().collect(), st.italic));
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        cur.push(c);
        i += 1;
    }
    flush!();
    out
}

/// Render Markdown-ish text into wrapped, styled lines.
pub fn markdown(text: &str, st: &Styles, max: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut in_code = false;
    let all: Vec<&str> = text.lines().collect();
    let mut at = 0;
    while at < all.len() {
        let raw = all[at];
        at += 1;
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        // A table: a row of cells, then a row of dashes, then the rows.
        if !in_code && trimmed.contains('|') {
            if let Some(aligns) = all.get(at).and_then(|next| table_rule(next)).filter(|a| a.len() == table_row(trimmed).len()) {
                let mut rows = vec![table_row(trimmed)];
                at += 1;
                while let Some(next) = all.get(at).map(|l| l.trim()).filter(|l| l.contains('|') && !l.starts_with("```")) {
                    rows.push(table_row(next));
                    at += 1;
                }
                out.extend(table(&rows, &aligns, st, max));
                continue;
            }
        }
        if in_code {
            let expanded = raw.replace('\t', "    ");
            // Code is hard-wrapped so indentation and alignment survive.
            let mut line = String::from("  ");
            let mut w = 2;
            let mut first = true;
            for c in expanded.chars() {
                let cw = c.width().unwrap_or(0);
                if w + cw > max.max(8) {
                    out.push(Line::from(Span::styled(std::mem::take(&mut line), st.code_block)));
                    line.push_str("    ");
                    w = 4;
                    first = false;
                }
                line.push(c);
                w += cw;
            }
            let _ = first;
            out.push(Line::from(Span::styled(line, st.code_block)));
            continue;
        }
        if trimmed.is_empty() {
            out.push(Line::default());
            continue;
        }
        let lead = raw.len() - trimmed.len();
        if let Some(h) = trimmed.strip_prefix('#') {
            let h = h.trim_start_matches('#').trim();
            for l in wrap(&[(h.to_string(), st.heading)], max, 0) {
                out.push(to_line(l));
            }
            continue;
        }
        if trimmed.chars().all(|c| c == '-' || c == '*' || c == '_') && trimmed.len() >= 3 {
            out.push(Line::from(Span::styled("─".repeat(max.min(40)), st.muted)));
            continue;
        }
        if let Some(q) = trimmed.strip_prefix('>') {
            let mut segs = vec![("▏ ".to_string(), st.muted)];
            segs.extend(inline(q.trim_start(), st, st.muted));
            for l in wrap(&segs, max, 2) {
                out.push(to_line(l));
            }
            continue;
        }
        let bullet = ["- ", "* ", "+ "].iter().find_map(|b| trimmed.strip_prefix(b));
        if let Some(rest) = bullet {
            let pad = " ".repeat(lead.min(12));
            let mut segs = vec![(format!("{pad}• "), st.bullet)];
            segs.extend(inline(rest, st, st.text));
            for l in wrap(&segs, max, lead.min(12) + 2) {
                out.push(to_line(l));
            }
            continue;
        }
        let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 && digits <= 3 && trimmed[digits..].starts_with(". ") {
            let pad = " ".repeat(lead.min(12));
            let marker = &trimmed[..digits + 2];
            let mut segs = vec![(format!("{pad}{marker}"), st.bullet)];
            segs.extend(inline(&trimmed[digits + 2..], st, st.text));
            for l in wrap(&segs, max, lead.min(12) + digits + 2) {
                out.push(to_line(l));
            }
            continue;
        }
        let mut segs = Vec::new();
        if lead > 0 {
            segs.push((" ".repeat(lead.min(12)), st.text));
        }
        segs.extend(inline(trimmed, st, st.text));
        for l in wrap(&segs, max, 0) {
            out.push(to_line(l));
        }
    }
    // Trim blank lines at the edges.
    while out.first().is_some_and(|l| l.width() == 0) {
        out.remove(0);
    }
    while out.last().is_some_and(|l| l.width() == 0) {
        out.pop();
    }
    out
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Align {
    Left,
    Center,
    Right,
}

/// The cells of one row of a table: split at each `|` that is not escaped
/// and not inside `code`, the bars at either end aside.
fn table_row(line: &str) -> Vec<String> {
    let line = line.trim();
    let line = line.strip_prefix('|').unwrap_or(line);
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut in_code = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cur.push('|');
                chars.next();
            }
            '`' => {
                in_code = !in_code;
                cur.push(c);
            }
            '|' if !in_code => cells.push(std::mem::take(&mut cur).trim().to_string()),
            _ => cur.push(c),
        }
    }
    // What follows the last bar is a cell only if there is something in it.
    if !cur.trim().is_empty() {
        cells.push(cur.trim().to_string());
    }
    cells
}

/// The row of dashes under a table's header, as how each column is
/// aligned (`:---`, `:---:`, `---:`); `None` if the line is not one.
fn table_rule(line: &str) -> Option<Vec<Align>> {
    let line = line.trim();
    if !line.contains('-') || !line.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
        return None;
    }
    let cells = table_row(line);
    let mut aligns = Vec::new();
    for cell in &cells {
        let dashes = cell.trim_matches(':');
        if dashes.is_empty() || !dashes.chars().all(|c| c == '-') {
            return None;
        }
        aligns.push(match (cell.starts_with(':'), cell.ends_with(':')) {
            (true, true) => Align::Center,
            (false, true) => Align::Right,
            _ => Align::Left,
        });
    }
    (!aligns.is_empty()).then_some(aligns)
}

fn seg_width(segs: &[Seg]) -> usize {
    segs.iter().map(|(t, _)| width(t)).sum()
}

/// A table, drawn. `rows[0]` is the header. Each column is as wide as its
/// widest cell; when that is wider than `max`, the widest columns are
/// narrowed and their cells wrapped; and when there is not the room even
/// for that, each row is shown as a list of its cells under their headers.
fn table(rows: &[Vec<String>], aligns: &[Align], st: &Styles, max: usize) -> Vec<Line<'static>> {
    /// The narrowest a column whose cells are wrapped may be.
    const NARROWEST: usize = 8;
    let n = aligns.len();
    let cells: Vec<Vec<Vec<Seg>>> = rows
        .iter()
        .enumerate()
        .map(|(r, row)| (0..n).map(|c| inline(row.get(c).map(String::as_str).unwrap_or(""), st, if r == 0 { st.bold } else { st.text })).collect())
        .collect();
    let mut widths: Vec<usize> = (0..n).map(|c| cells.iter().map(|row| seg_width(&row[c])).max().unwrap_or(0).max(1)).collect();
    // "│ a │ b │": a bar and a space before each cell, a space and a bar after the last.
    let frame = 3 * n + 1;
    let least: usize = widths.iter().map(|w| (*w).min(NARROWEST)).sum();
    if frame + least > max {
        return table_as_list(&cells, st, max);
    }
    while frame + widths.iter().sum::<usize>() > max {
        let widest = (0..n).max_by_key(|&c| widths[c]).unwrap_or(0);
        widths[widest] -= 1;
    }

    let rule = |left: &str, mid: &str, right: &str| {
        let bars: Vec<String> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
        Line::from(Span::styled(format!("{left}{}{right}", bars.join(mid)), st.muted))
    };
    let wrapped: Vec<Vec<Vec<Vec<Seg>>>> = cells.iter().map(|row| (0..n).map(|c| wrap(&row[c], widths[c], 0)).collect()).collect();
    // With a cell on several lines, a rule between the rows keeps them apart.
    let tall = wrapped.iter().any(|row| row.iter().any(|cell| cell.len() > 1));
    let mut out = vec![rule("┌", "┬", "┐")];
    for (r, row) in wrapped.iter().enumerate() {
        if r == 1 || (r > 1 && tall) {
            out.push(rule("├", "┼", "┤"));
        }
        let height = row.iter().map(Vec::len).max().unwrap_or(1);
        for l in 0..height {
            let mut spans = Vec::new();
            for c in 0..n {
                spans.push(Span::styled(if c == 0 { "│ " } else { " │ " }, st.muted));
                let line: &[Seg] = row[c].get(l).map(Vec::as_slice).unwrap_or(&[]);
                let spare = widths[c].saturating_sub(seg_width(line));
                let before = match aligns[c] {
                    Align::Left => 0,
                    Align::Center => spare / 2,
                    Align::Right => spare,
                };
                spans.push(Span::raw(" ".repeat(before)));
                spans.extend(line.iter().map(|(t, s)| Span::styled(t.clone(), *s)));
                spans.push(Span::raw(" ".repeat(spare - before)));
            }
            spans.push(Span::styled(" │", st.muted));
            out.push(Line::from(spans));
        }
    }
    out.push(rule("└", "┴", "┘"));
    out
}

/// A table with no room to be one: each row as its cells, one under the
/// other, each after the header of its column.
fn table_as_list(cells: &[Vec<Vec<Seg>>], st: &Styles, max: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for row in cells.iter().skip(1) {
        if !out.is_empty() {
            out.push(Line::default());
        }
        for (c, cell) in row.iter().enumerate() {
            if cell.is_empty() {
                continue;
            }
            let mut segs: Vec<Seg> = cells[0][c].iter().map(|(t, _)| (t.clone(), st.muted)).collect();
            if !segs.is_empty() {
                segs.push((": ".to_string(), st.muted));
            }
            segs.extend(cell.iter().cloned());
            out.extend(wrap(&segs, max, 2).into_iter().map(to_line));
        }
    }
    out
}

/// Wrap plain styled text and return ready lines.
pub fn plain(segs: &[Seg], max: usize, indent: usize) -> Vec<Line<'static>> {
    wrap(segs, max, indent).into_iter().map(to_line).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_line_is_cut_into_pieces_that_fit_and_lose_nothing() {
        let line = format!("{}tail", "x".repeat(150));
        let parts = super::pieces(&line, 40);
        assert!(parts.iter().all(|p| super::width(p) <= 40), "{parts:?}");
        assert_eq!(parts.concat(), line);
        // Wide characters are not split, and an empty line stays one line.
        assert_eq!(super::pieces("日本語", 4), vec!["日本", "語"]);
        assert_eq!(super::pieces("", 10), vec![String::new()]);
    }

    use super::*;
    use ratatui::style::{Color, Modifier};

    fn styles() -> Styles {
        let s = Style::default();
        Styles {
            text: s,
            bold: s.add_modifier(Modifier::BOLD),
            italic: s.add_modifier(Modifier::ITALIC),
            code: s.fg(Color::Cyan),
            code_block: s.fg(Color::Green),
            heading: s.fg(Color::Magenta),
            muted: s.fg(Color::DarkGray),
            bullet: s.fg(Color::Blue),
        }
    }

    fn text_of(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect()
    }

    #[test]
    fn wraps_on_words() {
        let lines = wrap(&[("the quick brown fox jumps".into(), Style::default())], 10, 0);
        let got: Vec<String> = lines.iter().map(|l| l.iter().map(|(t, _)| t.as_str()).collect()).collect();
        assert_eq!(got, vec!["the quick", "brown fox", "jumps"]);
    }

    #[test]
    fn inline_styles() {
        let st = styles();
        let segs = inline("run `ls -la` in **bold** and *it* 2*3*4", &st, st.text);
        assert!(segs.contains(&("ls -la".to_string(), st.code)));
        assert!(segs.contains(&("bold".to_string(), st.bold)));
        assert!(segs.contains(&("it".to_string(), st.italic)));
        assert!(segs.iter().any(|(t, _)| t.contains("2*3*4")));
    }

    #[test]
    fn markdown_blocks() {
        let st = styles();
        let lines = markdown("# Title\n\n- one\n- two\n\n```sh\necho hi\n```\ndone", &st, 40);
        assert_eq!(text_of(&lines), vec!["Title", "", "• one", "• two", "", "  echo hi", "done"]);
    }

    #[test]
    fn a_table_is_drawn_with_each_column_as_wide_as_its_widest_cell() {
        let st = styles();
        let md = "Here:\n\n| Name | Qty | Note |\n|:---|---:|:---:|\n| **Apples** | 3 | ok |\n| Figs | 120 | `a|b` |\nafter";
        let lines = markdown(md, &st, 60);
        assert_eq!(
            text_of(&lines),
            vec![
                "Here:",
                "",
                "┌────────┬─────┬──────┐",
                "│ Name   │ Qty │ Note │",
                "├────────┼─────┼──────┤",
                "│ Apples │   3 │  ok  │",
                "│ Figs   │ 120 │ a|b  │",
                "└────────┴─────┴──────┘",
                "after",
            ]
        );
        // The header is bold, and so is what a cell marks as bold; code stays code.
        let style_of = |line: usize, text: &str| lines[line].spans.iter().find(|s| s.content == text).map(|s| s.style);
        assert_eq!(style_of(3, "Name"), Some(st.bold));
        assert_eq!(style_of(5, "Apples"), Some(st.bold));
        assert_eq!(style_of(6, "a|b"), Some(st.code));
        assert_eq!(style_of(6, "Figs"), Some(st.text));
    }

    #[test]
    fn a_table_too_wide_wraps_its_widest_cells_and_one_with_no_room_becomes_a_list() {
        let st = styles();
        let md = "| Country | Why |\n|---|---|\n| Malaysia | Affordable food, housing and private healthcare |\n| Mexico | Good value |";
        let lines = markdown(md, &st, 40);
        let got = text_of(&lines);
        assert!(got.iter().all(|l| width(l) == 40), "{got:#?}");
        assert_eq!(got[3], "│ Malaysia │ Affordable food, housing  │");
        assert_eq!(got[4], "│          │ and private healthcare    │");
        // A cell on two lines: the rows are ruled apart.
        assert_eq!(got[5], "├──────────┼───────────────────────────┤");
        assert_eq!(got[6], "│ Mexico   │ Good value                │");

        // Wide characters count two columns.
        let got = text_of(&markdown("| a | b |\n|---|---|\n| 日本 | x |", &st, 40));
        assert_eq!(got[3], "│ 日本 │ x │");
        assert_eq!(width(&got[0]), width(&got[3]));

        // No room for three columns of any use: each row as a list.
        let md = "| Country | Why consider it | Main trade-off |\n|---|---|---|\n| Malaysia | Affordable food | Hot, humid climate |\n| Mexico | Good value | Safety varies |";
        let got = text_of(&markdown(md, &st, 30));
        assert_eq!(got, vec!["Country: Malaysia", "Why consider it: Affordable", "  food", "Main trade-off: Hot, humid", "  climate", "", "Country: Mexico", "Why consider it: Good value", "Main trade-off: Safety varies"]);
    }

    #[test]
    fn what_only_looks_like_a_table_is_left_as_it_is() {
        let st = styles();
        // No row of dashes: text. In a code block: code. A rule of another size: text.
        assert_eq!(text_of(&markdown("a | b\nc | d", &st, 40)), vec!["a | b", "c | d"]);
        assert_eq!(text_of(&markdown("```\n| a | b |\n|---|---|\n```", &st, 40)), vec!["  | a | b |", "  |---|---|"]);
        assert_eq!(text_of(&markdown("| a | b |\n|---|", &st, 40)), vec!["| a | b |", "|---|"]);
        // Still arriving: the header alone is text, then a table as far as
        // it has come, a last row cut short and all.
        assert_eq!(text_of(&markdown("| a | b |", &st, 40)), vec!["| a | b |"]);
        assert_eq!(text_of(&markdown("| a | b |\n|---|---|\n| one | tw", &st, 40)), vec!["┌─────┬────┐", "│ a   │ b  │", "├─────┼────┤", "│ one │ tw │", "└─────┴────┘"]);
        assert_eq!(text_of(&markdown("| a | b |\n|---|---|\n| one |", &st, 40))[3], "│ one │   │");
        // No bars at the ends, and an escaped bar inside a cell.
        assert_eq!(text_of(&markdown("a | b\n---|---\nx \\| y | z", &st, 40))[3], "│ x | y │ z │");
        assert_eq!(table_rule("| :-- | :-: | --: |"), Some(vec![Align::Left, Align::Center, Align::Right]));
        assert_eq!(table_rule("---"), Some(vec![Align::Left]));
        assert_eq!(table_rule("| a | b |"), None);
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate("hello world", 8), "hello w…");
        assert_eq!(truncate_left("/a/very/long/path", 9), "…ong/path");
        assert_eq!(truncate("short", 8), "short");
    }
}
