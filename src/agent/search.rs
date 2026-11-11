//! The agent's search tools: find files by name pattern, and search the
//! text of files. Both walk the directory tree themselves, so they behave
//! the same on every system and need no external programs.

use regex_automata::meta::Regex;
use regex_automata::util::syntax;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// Directories that hold other people's code or generated files. They are
/// passed over unless the search starts inside one.
const SKIP_DIRS: &[&str] =
    &[".git", ".hg", ".svn", "node_modules", "target", ".venv", "venv", "__pycache__", ".cache", ".tox", ".mypy_cache", ".next"];

const MAX_ENTRIES: usize = 200_000;
const TIME_LIMIT: Duration = Duration::from_secs(20);
/// Larger files are not searched: they are rarely source, and one of them
/// can cost more than the rest of the tree together.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_LINE_CHARS: usize = 300;

/// Translate a file-name pattern into a regular expression over a relative
/// path: `*` and `?` stay within one path component, `**` crosses them,
/// `[abc]` and `{a,b}` work as in the shell. A pattern without a `/` is
/// matched against the file name at any depth.
fn glob_to_regex(glob: &str) -> String {
    let anywhere = !glob.contains('/');
    let chars: Vec<char> = glob.trim_start_matches("./").chars().collect();
    let mut re = String::from(if anywhere { "^(?:.*/)?" } else { "^" });
    let mut braces = 0;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' if chars.get(i + 1) == Some(&'*') => {
                i += 1;
                if chars.get(i + 1) == Some(&'/') {
                    i += 1;
                    re.push_str("(?:.*/)?");
                } else {
                    re.push_str(".*");
                }
            }
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            '[' => match chars[i..].iter().position(|c| *c == ']') {
                Some(end) if end > 1 => {
                    let body: String = chars[i + 1..i + end].iter().collect();
                    let body = body.strip_prefix('!').map(|rest| format!("^{rest}")).unwrap_or(body);
                    re.push_str(&format!("[{}]", body.replace('\\', "\\\\")));
                    i += end;
                }
                _ => re.push_str("\\["),
            },
            '{' => {
                braces += 1;
                re.push_str("(?:");
            }
            '}' if braces > 0 => {
                braces -= 1;
                re.push(')');
            }
            ',' if braces > 0 => re.push('|'),
            c if "\\.+()|^$".contains(c) || c == '{' || c == '}' => {
                re.push('\\');
                re.push(c);
            }
            c => re.push(c),
        }
        i += 1;
    }
    for _ in 0..braces {
        re.push(')');
    }
    re.push('$');
    re
}

fn compile(pattern: &str, ignore_case: bool) -> Result<Regex, String> {
    Regex::builder()
        .syntax(syntax::Config::new().case_insensitive(ignore_case).multi_line(true))
        .build(pattern)
        .map_err(|e| format!("`{pattern}` is not a valid pattern: {e}"))
}

/// Every file under `root`, depth first, with its path relative to `root`.
/// Stops early, saying so in `note`, when the tree is enormous.
struct Walk {
    stack: Vec<PathBuf>,
    root: PathBuf,
    seen: usize,
    began: Instant,
    pub cut_short: bool,
}

impl Walk {
    fn new(root: &Path) -> Self {
        Self { stack: vec![root.to_path_buf()], root: root.to_path_buf(), seen: 0, began: Instant::now(), cut_short: false }
    }

    /// Visit files until `visit` returns false.
    fn run(&mut self, mut visit: impl FnMut(&Path, &str) -> bool) {
        while let Some(dir) = self.stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                self.seen += 1;
                if self.seen > MAX_ENTRIES || self.began.elapsed() > TIME_LIMIT {
                    self.cut_short = true;
                    return;
                }
                let Ok(kind) = entry.file_type() else { continue };
                let path = entry.path();
                if kind.is_dir() {
                    if !SKIP_DIRS.contains(&entry.file_name().to_string_lossy().as_ref()) {
                        self.stack.push(path);
                    }
                } else if kind.is_file() {
                    let relative = path.strip_prefix(&self.root).unwrap_or(&path).to_string_lossy().into_owned();
                    if !visit(&path, &relative) {
                        return;
                    }
                }
                // Symbolic links are not followed: they can loop, and what
                // they point to is found under its own name.
            }
        }
    }
}

fn check_dir(root: &Path) -> Result<(), String> {
    match std::fs::metadata(root) {
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => Err(format!("{} is a file, not a directory.", root.display())),
        Err(e) => Err(format!("{}: {e}", root.display())),
    }
}

/// Files under `root` whose path matches the glob `pattern`, most recently
/// changed first.
pub fn find_files(root: &Path, pattern: &str, max: usize) -> Result<String, String> {
    check_dir(root)?;
    let matcher = compile(&glob_to_regex(pattern), false).map_err(|_| format!("`{pattern}` is not a pattern this tool understands."))?;
    let mut found: Vec<(SystemTime, String)> = Vec::new();
    let mut walk = Walk::new(root);
    walk.run(|path, relative| {
        if matcher.is_match(relative) {
            let changed = std::fs::metadata(path).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
            found.push((changed, relative.to_string()));
        }
        true
    });
    if found.is_empty() {
        return Ok(format!("No files match `{pattern}` under {}.{}", root.display(), cut_note(&walk)));
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let total = found.len();
    let mut out = format!("{total} file{} under {}, most recently changed first:\n", if total == 1 { "" } else { "s" }, root.display());
    for (_, path) in found.iter().take(max) {
        out.push_str(path);
        out.push('\n');
    }
    if total > max {
        out.push_str(&format!("[{} more not shown; narrow the pattern or the path]\n", total - max));
    }
    out.push_str(cut_note(&walk).trim_start());
    Ok(out.trim_end().to_string())
}

fn cut_note(walk: &Walk) -> &'static str {
    if walk.cut_short { " [The search stopped early: the directory tree is very large. Search a subdirectory.]" } else { "" }
}

pub struct Grep<'a> {
    pub pattern: &'a str,
    /// Only search files whose path matches this glob.
    pub glob: Option<&'a str>,
    pub ignore_case: bool,
    /// Lines to show before and after each matching line.
    pub context: usize,
    pub max: usize,
}

fn clip(line: &str) -> String {
    if line.chars().count() > MAX_LINE_CHARS { line.chars().take(MAX_LINE_CHARS).collect::<String>() + " […]" } else { line.to_string() }
}

/// Lines matching a regular expression, in one file or in every text file
/// under a directory, as `path:line: text`.
pub fn search_files(target: &Path, q: &Grep) -> Result<String, String> {
    let re = compile(q.pattern, q.ignore_case)?;
    let only = match q.glob {
        Some(glob) => Some(compile(&glob_to_regex(glob), false).map_err(|_| format!("`{glob}` is not a pattern this tool understands."))?),
        None => None,
    };
    let meta = std::fs::metadata(target).map_err(|e| format!("{}: {e}", target.display()))?;
    let mut out = String::new();
    let (mut matches, mut files, mut skipped_large) = (0usize, 0usize, 0usize);
    let mut search_one = |path: &Path, shown: &str| -> bool {
        if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_FILE_BYTES) {
            skipped_large += 1;
            return true;
        }
        let Ok(bytes) = std::fs::read(path) else { return true };
        if bytes.iter().take(8192).any(|&b| b == 0) || !re.is_match(&bytes) {
            return true;
        }
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        let mut last_shown: Option<usize> = None;
        let mut any = false;
        for (i, line) in lines.iter().enumerate() {
            if !re.is_match(line.as_bytes()) {
                continue;
            }
            if matches >= q.max {
                return false;
            }
            matches += 1;
            any = true;
            let from = i.saturating_sub(q.context).max(last_shown.map(|l| l + 1).unwrap_or(0));
            let to = (i + q.context).min(lines.len() - 1);
            if q.context > 0 && last_shown.is_some_and(|l| from > l + 1) {
                out.push_str("--\n");
            }
            for (n, text) in lines.iter().enumerate().take(to + 1).skip(from) {
                let mark = if n == i { ':' } else { '-' };
                out.push_str(&format!("{shown}{mark}{}{mark} {}\n", n + 1, clip(text)));
            }
            last_shown = Some(to);
        }
        if any {
            files += 1;
        }
        true
    };

    let mut note = "";
    if meta.is_file() {
        search_one(target, &target.to_string_lossy());
    } else {
        let mut walk = Walk::new(target);
        walk.run(|path, relative| {
            if only.as_ref().is_some_and(|glob| !glob.is_match(relative)) {
                return true;
            }
            search_one(path, relative)
        });
        note = cut_note(&walk);
    }

    if matches == 0 {
        let scope = q.glob.map(|g| format!(" in files matching `{g}`")).unwrap_or_default();
        return Ok(format!("No match for `{}`{scope} under {}.{note}", q.pattern, target.display()));
    }
    let mut head = format!(
        "{matches} matching line{} in {files} file{}{}:\n",
        if matches == 1 { "" } else { "s" },
        if files == 1 { "" } else { "s" },
        if meta.is_dir() { format!(" under {}", target.display()) } else { String::new() },
    );
    if matches >= q.max {
        head = format!("The first {matches} matching lines (there are more; narrow the pattern, the path or the glob):\n");
    }
    if skipped_large > 0 {
        out.push_str(&format!("[{skipped_large} file{} over 2 MB not searched]\n", if skipped_large == 1 { "" } else { "s" }));
    }
    out.push_str(note.trim_start());
    Ok(format!("{head}{}", out.trim_end()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("astrum-search-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (path, body) in [
            ("Cargo.toml", "[package]\nname = \"demo\"\n"),
            ("src/main.rs", "fn main() {\n    println!(\"hello\");\n    helper();\n}\n"),
            ("src/util/helper.rs", "pub fn helper() {}\n// TODO: tidy\n"),
            ("docs/notes.md", "Notes\nTODO write more\nthe end\n"),
            ("node_modules/dep/index.js", "// TODO in a dependency\n"),
            (".git/config", "TODO not source\n"),
            ("data.bin", "TODO\0binary"),
        ] {
            let file = root.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, body).unwrap();
        }
        root
    }

    #[test]
    fn globs_follow_shell_rules() {
        let matches = |glob: &str, path: &str| compile(&glob_to_regex(glob), false).unwrap().is_match(path);
        assert!(matches("*.rs", "main.rs") && matches("*.rs", "src/util/helper.rs"));
        assert!(matches("src/*.rs", "src/main.rs") && !matches("src/*.rs", "src/util/helper.rs"));
        assert!(matches("src/**/*.rs", "src/main.rs") && matches("src/**/*.rs", "src/util/helper.rs"));
        assert!(matches("**/*.{md,toml}", "docs/notes.md") && matches("*.{md,toml}", "Cargo.toml") && !matches("*.{md,toml}", "a.rs"));
        assert!(matches("file?.txt", "file1.txt") && !matches("file?.txt", "file10.txt"));
        assert!(matches("[a-c]*.rs", "b.rs") && !matches("[!a-c]*.rs", "b.rs"));
        assert!(matches("a+b(1).txt", "a+b(1).txt") && !matches("*.rs", "main.rsx"));
    }

    #[test]
    fn finding_files_skips_dependency_and_vcs_directories() {
        let root = tree("find");
        let out = find_files(&root, "*.rs", 50).unwrap();
        assert!(out.starts_with("2 files") && out.contains("src/main.rs") && out.contains("src/util/helper.rs"), "{out}");
        let all = find_files(&root, "**/*", 50).unwrap();
        assert!(!all.contains("node_modules") && !all.contains(".git/") && all.contains("data.bin"), "{all}");
        // Starting inside a skipped directory searches it.
        assert!(find_files(&root.join("node_modules"), "*.js", 50).unwrap().contains("dep/index.js"));
        assert!(find_files(&root, "*.py", 50).unwrap().starts_with("No files match"));
        assert!(find_files(&root, "**/*", 2).unwrap().contains("more not shown"));
        assert!(find_files(&root.join("missing"), "*", 5).is_err() && find_files(&root.join("Cargo.toml"), "*", 5).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn searching_text_reports_path_line_and_context() {
        let root = tree("grep");
        let q = |pattern: &'static str| Grep { pattern, glob: None, ignore_case: false, context: 0, max: 50 };
        let out = search_files(&root, &q("TODO")).unwrap();
        assert!(out.starts_with("2 matching lines in 2 files"), "{out}");
        assert!(out.contains("docs/notes.md:2: TODO write more") && out.contains("src/util/helper.rs:2: // TODO: tidy"), "{out}");
        assert!(!out.contains("node_modules") && !out.contains(".git") && !out.contains("data.bin"), "{out}");

        // A glob narrows the files; a regular expression is a regular expression.
        let rs = search_files(&root, &Grep { glob: Some("*.rs"), ..q(r"fn \w+\(\)") }).unwrap();
        assert!(rs.contains("src/main.rs:1: fn main() {") && rs.contains("helper.rs:1: pub fn helper() {}") && !rs.contains("notes.md"), "{rs}");
        assert!(search_files(&root, &Grep { ignore_case: true, ..q("todo WRITE") }).unwrap().contains("notes.md:2"));
        assert!(search_files(&root, &q("todo WRITE")).unwrap().starts_with("No match"));

        // Context lines are marked differently from the matching line.
        let ctx = search_files(&root.join("docs/notes.md"), &Grep { context: 1, ..q("TODO") }).unwrap();
        assert!(ctx.contains("notes.md-1- Notes") && ctx.contains("notes.md:2: TODO write more") && ctx.contains("notes.md-3- the end"), "{ctx}");

        assert!(search_files(&root, &Grep { max: 1, ..q("TODO") }).unwrap().starts_with("The first 1 matching lines"));
        assert!(search_files(&root, &q("(unclosed")).unwrap_err().contains("not a valid pattern"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
