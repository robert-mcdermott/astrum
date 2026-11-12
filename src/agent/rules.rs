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
    use tokio::io::AsyncReadExt;
    // No more of it than is used, however large it is.
    let mut bytes = Vec::new();
    tokio::fs::File::open(path).await.ok()?.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes).await.ok()?;
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
/// top of its project, the nearest directory holding `.git` or the home
/// directory, whichever comes first. Outside the home directory, with no
/// repository above, the working directory's own alone: a directory such
/// as `/tmp` is anyone's, and what someone else put above it is not this
/// project's. (A project's skills are looked for the same way: see
/// `skills::project_levels`.) The closest few, outermost first.
async fn project_files(cwd: &Path, home: &Path) -> Vec<PathBuf> {
    let mut levels = Vec::new();
    let mut at_top = false;
    let mut repository = None;
    for d in cwd.ancestors() {
        levels.push(d);
        if tokio::fs::try_exists(d.join(".git")).await.unwrap_or(false) {
            at_top = true;
            repository = Some(d);
            break;
        }
        if d == home {
            at_top = true;
            break;
        }
    }
    if !at_top {
        levels.truncate(1);
    }
    let real = |p: &Path| std::fs::canonicalize(p).ok();
    let mut found = Vec::new();
    for d in levels {
        let candidate = d.join(FILE);
        // One that is a link must stay in the project: in a repository,
        // inside it; elsewhere, in its own directory. A link out to a file
        // of the user's (`~/.netrc`) would put that file before the model.
        let inside = real(repository.unwrap_or(d));
        let stays = real(&candidate).zip(inside).is_some_and(|(file, top)| file.starts_with(top));
        if stays && tokio::fs::metadata(&candidate).await.is_ok_and(|m| m.is_file()) {
            found.push(candidate);
            if found.len() >= MAX_PROJECT_FILES {
                break;
            }
        }
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
    headed(gather(files).await)
}

/// The files that are there, each as a block of the system prompt.
async fn gather(files: Vec<(PathBuf, &str)>) -> Rules {
    let mut rules = Rules::default();
    for (path, scope) in files {
        let Some(body) = read(&path).await else { continue };
        let shown = paths::tildify(&path.to_string_lossy());
        rules.text.push_str(&format!("\n\n<rules file=\"{shown}\" scope=\"{scope}\">\n{body}\n</rules>"));
        rules.sources.push(shown);
    }
    rules
}

fn headed(mut rules: Rules) -> Rules {
    if !rules.text.is_empty() {
        rules.text.insert_str(
            0,
            "\n\nRules\nThe user keeps standing instructions in the files below. Follow them. Where two of them disagree, the one listed later (closer to the working directory) wins, and what the user asks for in the conversation wins over all of them.",
        );
    }
    rules
}

/// A project's own rules and no others, for a project on another machine:
/// that machine reads them (see `crate::remote`), and they are joined to
/// the user's own, kept here, by `load_beside`.
pub async fn load_project(cwd: &str) -> Rules {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
    let cwd = Path::new(cwd);
    if !cwd.is_absolute() {
        return Rules::default();
    }
    gather(project_files(cwd, &home).await.into_iter().map(|path| (path, "this project")).collect()).await
}

/// The user's own rules from this machine, with a project's as another
/// machine read them.
pub async fn load_beside(project: Rules) -> Rules {
    let mut rules = gather(vec![(paths::config_dir().join(FILE), "yours, for every project")]).await;
    rules.text.push_str(&project.text);
    rules.sources.extend(project.sources);
    headed(rules)
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
    async fn outside_the_home_directory_only_a_repository_is_a_project() {
        let root = scratch("outside");
        // (A scratch directory inside a repository cannot show this.)
        if root.ancestors().skip(1).any(|d| d.join(".git").exists()) {
            return;
        }
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        // Someone else's, in a directory anyone may write to.
        let shared = root.join("tmp");
        let work = shared.join("work");
        std::fs::create_dir_all(work.join("deeper")).unwrap();
        std::fs::write(shared.join(FILE), "planted rule").unwrap();
        std::fs::write(work.join(FILE), "work rule").unwrap();
        let r = load_from(&root.join("missing.md"), Some(&work), &home).await;
        assert!(r.text.contains("work rule") && !r.text.contains("planted rule"), "{}", r.text);
        let r = load_from(&root.join("missing.md"), Some(&work.join("deeper")), &home).await;
        assert!(r.text.is_empty(), "a directory below has none of its own: {}", r.text);
        // A repository there is a project, up to its top.
        let repo = shared.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join(FILE), "repo rule").unwrap();
        let r = load_from(&root.join("missing.md"), Some(&repo.join("src")), &home).await;
        assert!(r.text.contains("repo rule") && !r.text.contains("planted rule"), "{}", r.text);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_project_rule_that_leads_out_of_the_project_is_left_out() {
        let root = scratch("links");
        let home = root.join("home");
        let repo = home.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("docs")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(home.join(".netrc"), "machine example login me password hunter2").unwrap();
        std::os::unix::fs::symlink(home.join(".netrc"), repo.join(FILE)).unwrap();
        // A link within the repository is followed.
        std::fs::write(repo.join("docs").join("rules.md"), "kept rule").unwrap();
        std::os::unix::fs::symlink(repo.join("docs").join("rules.md"), repo.join("src").join(FILE)).unwrap();
        let r = load_from(&root.join("missing.md"), Some(&repo.join("src")), &home).await;
        assert!(r.text.contains("kept rule"), "{}", r.text);
        assert!(!r.text.contains("hunter2"), "{}", r.text);
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
