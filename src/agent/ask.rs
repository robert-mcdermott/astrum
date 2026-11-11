//! Asking the agent from the shell prompt: deciding whether a line typed at
//! the prompt was meant for the agent rather than the shell.

use std::path::Path;

/// Words a question or request in plain language tends to start with.
const STARTERS: &[&str] = &[
    "how", "what", "what's", "whats", "why", "where", "when", "which", "who", "whose", "can", "could", "would", "should", "is",
    "are", "am", "does", "do", "did", "will", "please", "explain", "tell", "show", "describe", "summarize", "help", "i", "i'm",
    "im", "my", "there",
];

/// Shell words that are commands even though no file by that name is on the PATH.
const BUILTINS: &[&str] = &[
    "which", "where", "who", "whence", "do", "help", "time", "test", "type", "set", "if", "for", "while", "until", "case", "select",
    "function", "command", "builtin", "exec", "eval", "source", "alias", "export", "local", "read", "wait", "jobs", "history",
];

/// The question in `line`, a line typed at the shell prompt, if it is for
/// the agent:
/// - it starts with `prefix` (when one is set) and a space, as in
///   `# why is this slow`, or
/// - `natural` is on and it reads as plain language rather than a command.
pub fn question(line: &str, prefix: &str, natural: bool) -> Option<String> {
    let prefix = prefix.trim();
    if !prefix.is_empty() {
        if let Some(rest) = line.trim_start().strip_prefix(prefix) {
            let asked = rest.trim();
            // `#!x` and `##` are not questions; `# x` is.
            return (rest.starts_with(char::is_whitespace) && !asked.is_empty()).then(|| asked.to_string());
        }
    }
    (natural && looks_natural(line, &on_path)).then(|| line.trim().to_string())
}

/// Plain language, as far as can be told without asking the shell: several
/// words, none of the shell's own punctuation, an opening word that
/// questions and requests start with, and either a question mark at the end
/// or an opening word that is not a command on this machine. A line that
/// starts with a space is always left to the shell.
fn looks_natural(line: &str, is_program: &dyn Fn(&str) -> bool) -> bool {
    if line.starts_with(char::is_whitespace) {
        return false;
    }
    let line = line.trim_end();
    let words: Vec<&str> = line.split_whitespace().collect();
    if words.len() < 3 || line.contains(|c| "|&;<>$`\\=(){}*".contains(c)) {
        return false;
    }
    // `word -flag …` is how commands look.
    if words[1].starts_with('-') {
        return false;
    }
    let first = words[0].to_lowercase();
    if !STARTERS.contains(&first.as_str()) {
        return false;
    }
    line.ends_with('?') || !(BUILTINS.contains(&first.as_str()) || is_program(words[0]))
}

/// Is there a program by this name on the PATH?
fn on_path(name: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|dir| {
        let candidate: &Path = &dir.join(name);
        std::fs::metadata(candidate).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prefix_marks_a_question() {
        assert_eq!(question("# why is this slow", "#", false).as_deref(), Some("why is this slow"));
        assert_eq!(question("  #   list big files  ", "#", false).as_deref(), Some("list big files"));
        // Not questions: a shebang, a heading, a bare prefix, an ordinary command.
        for line in ["#!/bin/sh", "## notes", "#", "#   ", "#nospace", "ls -la # not at the start"] {
            assert_eq!(question(line, "#", false), None, "{line}");
        }
        // Another prefix can be chosen, or none.
        assert_eq!(question("? what time is it", "?", false).as_deref(), Some("what time is it"));
        assert_eq!(question("# why is this slow", "", false), None);
    }

    #[test]
    fn plain_language_is_told_from_commands() {
        let programs = |name: &str| ["who", "which", "show", "find", "make", "is"].contains(&name);
        let natural = |line: &str| looks_natural(line, &programs);
        for line in [
            "how do I find large files",
            "why did that command fail",
            "what's using port 8080",
            "can you show me the last three commits",
            "please undo my last commit",
            "who is logged in right now?",
            "which process is using the most memory?",
        ] {
            assert!(natural(line), "{line}");
        }
        for line in [
            "ls -la",
            "git status",
            "who am i",
            "which python3",
            "show interfaces brief",
            "is this a command",
            "how --help me",
            "what is $HOME",
            "can you run ls | wc",
            "why not",
            " how do I find large files",
            "make all the things",
        ] {
            assert!(!natural(line), "{line}");
        }
        // Off unless asked for.
        assert_eq!(question("how do I find large files", "#", false), None);
    }
}
