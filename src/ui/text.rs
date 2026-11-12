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
    for raw in text.lines() {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            continue;
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
    fn truncation() {
        assert_eq!(truncate("hello world", 8), "hello w…");
        assert_eq!(truncate_left("/a/very/long/path", 9), "…ong/path");
        assert_eq!(truncate("short", 8), "short");
    }
}
