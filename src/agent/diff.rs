//! Line differences between two versions of a file, grouped into hunks the
//! user can accept or decline one at a time.

/// One contiguous group of changed lines with a little unchanged text
/// around it.
#[derive(Debug, Clone, PartialEq)]
pub struct Hunk {
    /// 1-based line in the old text where the hunk starts.
    pub old_start: usize,
    pub old_len: usize,
    pub new_start: usize,
    pub new_len: usize,
    /// `' '` unchanged, `'-'` removed, `'+'` added; each with its line,
    /// line ending included.
    pub lines: Vec<(char, String)>,
}

fn split(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

/// The edit script from `old` to `new`, line by line. The unchanged start
/// and end are set aside first; what is left in the middle is compared
/// exactly when it is small enough and otherwise treated as one replacement.
fn script<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<(char, &'a str)> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..].iter().rev().zip(new[prefix..].iter().rev()).take_while(|(a, b)| a == b).count();
    let (a, b) = (&old[prefix..old.len() - suffix], &new[prefix..new.len() - suffix]);

    let mut ops: Vec<(char, &str)> = old[..prefix].iter().map(|l| (' ', *l)).collect();
    if a.len().saturating_mul(b.len()) <= 4_000_000 {
        // Longest common subsequence, by the table of suffix lengths.
        let width = b.len() + 1;
        let mut lcs = vec![0u32; (a.len() + 1) * width];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                lcs[i * width + j] = if a[i] == b[j] { lcs[(i + 1) * width + j + 1] + 1 } else { lcs[(i + 1) * width + j].max(lcs[i * width + j + 1]) };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            if a[i] == b[j] {
                ops.push((' ', a[i]));
                i += 1;
                j += 1;
            } else if lcs[(i + 1) * width + j] >= lcs[i * width + j + 1] {
                ops.push(('-', a[i]));
                i += 1;
            } else {
                ops.push(('+', b[j]));
                j += 1;
            }
        }
        ops.extend(a[i..].iter().map(|l| ('-', *l)));
        ops.extend(b[j..].iter().map(|l| ('+', *l)));
    } else {
        ops.extend(a.iter().map(|l| ('-', *l)));
        ops.extend(b.iter().map(|l| ('+', *l)));
    }
    ops.extend(old[old.len() - suffix..].iter().map(|l| (' ', *l)));
    ops
}

/// The changes that turn `old` into `new`, each with up to `context`
/// unchanged lines either side. Changes close enough to share context are
/// one hunk.
pub fn hunks(old: &str, new: &str, context: usize) -> Vec<Hunk> {
    let ops = script(&split(old), &split(new));
    let changed: Vec<usize> = ops.iter().enumerate().filter(|(_, (kind, _))| *kind != ' ').map(|(i, _)| i).collect();
    let mut out = Vec::new();
    let mut k = 0;
    while k < changed.len() {
        let first = changed[k];
        let mut last = first;
        while k + 1 < changed.len() && changed[k + 1] - last <= 2 * context + 1 {
            k += 1;
            last = changed[k];
        }
        k += 1;
        let (from, to) = (first.saturating_sub(context), (last + context + 1).min(ops.len()));
        let before = &ops[..from];
        let lines: Vec<(char, String)> = ops[from..to].iter().map(|(kind, line)| (*kind, line.to_string())).collect();
        out.push(Hunk {
            old_start: before.iter().filter(|(kind, _)| *kind != '+').count() + 1,
            old_len: lines.iter().filter(|(kind, _)| *kind != '+').count(),
            new_start: before.iter().filter(|(kind, _)| *kind != '-').count() + 1,
            new_len: lines.iter().filter(|(kind, _)| *kind != '-').count(),
            lines,
        });
    }
    out
}

/// `old` with the accepted hunks applied and the others left as they were.
pub fn apply(old: &str, hunks: &[Hunk], accept: &[bool]) -> String {
    let lines = split(old);
    let mut out = String::new();
    let mut at = 0;
    for (hunk, take) in hunks.iter().zip(accept) {
        let start = hunk.old_start - 1;
        out.extend(lines[at..start].iter().copied());
        let wanted = if *take { '+' } else { '-' };
        out.extend(hunk.lines.iter().filter(|(kind, _)| *kind == ' ' || *kind == wanted).map(|(_, line)| line.as_str()));
        at = start + hunk.old_len;
    }
    out.extend(lines[at..].iter().copied());
    out
}

/// A hunk as text: a `@@` header, then each line behind its marker.
pub fn render(hunk: &Hunk) -> Vec<String> {
    let mut out = vec![format!("@@ -{},{} +{},{} @@", hunk.old_start, hunk.old_len, hunk.new_start, hunk.new_len)];
    out.extend(hunk.lines.iter().map(|(kind, line)| format!("{kind} {}", line.trim_end_matches(['\n', '\r']).replace('\t', "    "))));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(n: usize) -> String {
        (1..=n).map(|i| format!("line {i}\n")).collect()
    }

    #[test]
    fn separate_changes_are_separate_hunks() {
        let old = numbered(30);
        let new = old.replace("line 3\n", "line three\n").replace("line 25\n", "line 25\nan extra line\n");
        let found = hunks(&old, &new, 3);
        assert_eq!(found.len(), 2);
        assert_eq!((found[0].old_start, found[0].old_len, found[0].new_start, found[0].new_len), (1, 6, 1, 6));
        assert_eq!((found[1].old_start, found[1].old_len, found[1].new_start, found[1].new_len), (23, 6, 23, 7));
        let shown = render(&found[0]);
        assert_eq!(shown[0], "@@ -1,6 +1,6 @@");
        assert!(shown.contains(&"- line 3".to_string()) && shown.contains(&"+ line three".to_string()) && shown.contains(&"  line 2".to_string()));

        // All, none, and one of the two.
        assert_eq!(apply(&old, &found, &[true, true]), new);
        assert_eq!(apply(&old, &found, &[false, false]), old);
        let first_only = apply(&old, &found, &[true, false]);
        assert!(first_only.contains("line three\n") && !first_only.contains("an extra line"));
        let second_only = apply(&old, &found, &[false, true]);
        assert!(second_only.contains("line 3\n") && second_only.contains("an extra line\n"));
    }

    #[test]
    fn nearby_changes_share_a_hunk() {
        let old = numbered(12);
        let new = old.replace("line 4\n", "line four\n").replace("line 8\n", "");
        let found = hunks(&old, &new, 3);
        assert_eq!(found.len(), 1);
        assert_eq!(apply(&old, &found, &[true]), new);
    }

    #[test]
    fn new_files_endings_and_no_change() {
        // A new file is one hunk of additions.
        let created = hunks("", "a\nb\n", 3);
        assert_eq!(created.len(), 1);
        assert_eq!(render(&created[0]), ["@@ -1,0 +1,2 @@", "+ a", "+ b"]);
        assert_eq!(apply("", &created, &[true]), "a\nb\n");
        // Emptying a file, and a missing final newline, round-trip exactly.
        let emptied = hunks("a\nb\n", "", 3);
        assert_eq!(apply("a\nb\n", &emptied, &[true]), "");
        let ending = hunks("a\nb", "a\nb\n", 3);
        assert_eq!(ending.len(), 1);
        assert_eq!(apply("a\nb", &ending, &[true]), "a\nb\n");
        assert!(hunks("same\n", "same\n", 3).is_empty());
    }

    #[test]
    fn a_large_rewrite_still_applies_exactly() {
        // Too big to compare line by line in the middle: one replacement.
        let old: String = (0..3000).map(|i| format!("old {i}\n")).collect();
        let new: String = (0..3000).map(|i| format!("new {i}\n")).collect();
        let found = hunks(&format!("top\n{old}end\n"), &format!("top\n{new}end\n"), 3);
        assert_eq!(found.len(), 1);
        assert_eq!(apply(&format!("top\n{old}end\n"), &found, &[true]), format!("top\n{new}end\n"));
    }
}
