//! Standing instructions the user keeps in `AGENTS.md` files: one for
//! themselves in the config directory, and any in the project they are
//! working in. They are read at the start of every turn, so edits take
//! effect at once.

use crate::paths;
use std::path::{Path, PathBuf};

const FILE: &str = "AGENTS.md";
/// Per file; a rules file larger than this is cut off, with a note.
const MAX_BYTES: usize = 32 * 1024;
const MAX_PROJECT_FILES: usize = 6;

#[derive(Default, Debug)]
pub struct Rules {
    /// Text to append to the system prompt; empty when there are no rules.
    pub text: String,
    /// The files it came from, for telling the user.
    pub sources: Vec<String>,
}

pub async fn load(cwd: Option<&str>) -> Rules {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
    load_from(&paths::config_dir().join(FILE), cwd.map(Path::new), &home).await
}

async fn read(path: &Path) -> Option<String> {
    let bytes = tokio::fs::read(path).await.ok()?;
    let cut = bytes.len() > MAX_BYTES;
    let mut text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_BYTES)]).into_owned();
    if text.trim().is_empty() {
        return None;
    }
    if cut {
        text.push_str("\n[the rest of this file was left out: it is longer than 32 KB]");
    }
    Some(text.trim().to_string())
}

/// Project rules: every `AGENTS.md` from the working directory up to the
/// project root (the directory holding `.git`), or up to the home directory
/// when there is no repository. Outermost first.
async fn project_files(cwd: &Path, home: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        let candidate = d.join(FILE);
        if tokio::fs::metadata(&candidate).await.is_ok_and(|m| m.is_file()) {
            found.push(candidate);
        }
        let at_root = tokio::fs::try_exists(d.join(".git")).await.unwrap_or(false);
        if at_root || d == home || found.len() >= MAX_PROJECT_FILES {
            break;
        }
        dir = d.parent();
    }
    found.reverse();
    found
}

async fn load_from(user_file: &Path, cwd: Option<&Path>, home: &Path) -> Rules {
    let mut files: Vec<(PathBuf, &str)> = vec![(user_file.to_path_buf(), "yours, for every project")];
    if let Some(cwd) = cwd.filter(|c| c.is_absolute()) {
        for path in project_files(cwd, home).await {
            if path != user_file {
                files.push((path, "this project"));
            }
        }
    }
    let mut rules = Rules::default();
    for (path, scope) in files {
        let Some(body) = read(&path).await else { continue };
        let shown = paths::tildify(&path.to_string_lossy());
        rules.text.push_str(&format!("\n\n<rules file=\"{shown}\" scope=\"{scope}\">\n{body}\n</rules>"));
        rules.sources.push(shown);
    }
    if !rules.text.is_empty() {
        rules.text.insert_str(
            0,
            "\n\nRules\nThe user keeps standing instructions in the files below. Follow them. Where two of them disagree, the one listed later (closer to the working directory) wins, and what the user asks for in the conversation wins over all of them.",
        );
    }
    rules
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("astrum-rules-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn user_and_project_rules_are_layered() {
        let root = scratch("layered");
        let home = root.join("home");
        let sub = home.join("proj/src/deep");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(home.join("proj/.git")).unwrap();
        std::fs::write(root.join("user.md"), "user rule").unwrap();
        std::fs::write(home.join(FILE), "home rule").unwrap();
        std::fs::write(home.join("proj").join(FILE), "root rule").unwrap();
        std::fs::write(home.join("proj/src").join(FILE), "src rule").unwrap();

        // Inside a repository: the user's file, then the project's from the root down.
        let r = load_from(&root.join("user.md"), Some(&sub), &home).await;
        assert_eq!(r.sources.len(), 3, "{:?}", r.sources);
        let order: Vec<usize> = ["user rule", "root rule", "src rule"].iter().map(|n| r.text.find(n).unwrap()).collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]));
        assert!(!r.text.contains("home rule"), "the search stops at the repository root");

        // Outside one: up to the home directory, and no further.
        let other = home.join("notes/today");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(root.join(FILE), "above home").unwrap();
        let r = load_from(&root.join("user.md"), Some(&other), &home).await;
        assert!(r.text.contains("home rule") && !r.text.contains("above home"));

        // No files, no text.
        std::fs::create_dir_all(home.join("bare/.git")).unwrap();
        let r = load_from(&root.join("missing.md"), Some(&home.join("bare")), &home).await;
        assert!(r.text.is_empty() && r.sources.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn oversized_and_empty_files() {
        let root = scratch("sizes");
        std::fs::write(root.join("big.md"), "x".repeat(MAX_BYTES + 10)).unwrap();
        std::fs::write(root.join("blank.md"), "  \n").unwrap();
        let r = load_from(&root.join("big.md"), None, &root).await;
        assert!(r.text.contains("left out") && r.text.len() < MAX_BYTES + 1000);
        let r = load_from(&root.join("blank.md"), None, &root).await;
        assert!(r.text.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
