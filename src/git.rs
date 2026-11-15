//! The git repository a shell's directory is in, for the status bar: its
//! branch, read from the repository's own files (no program needed), and
//! what has changed, from `git status`, when git is installed.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// What the status bar says of a repository.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Info {
    /// The branch checked out, or the start of the commit when none is.
    pub branch: String,
    /// No branch is checked out (`branch` is a commit).
    pub detached: bool,
    /// A merge, rebase or the like stopped half way.
    pub doing: Option<&'static str>,
    /// What `git status` found, when it could be asked.
    pub status: Option<Counts>,
}

/// Files changed, by how far the change has got.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Counts {
    /// Staged: in the index, to go into the next commit.
    pub staged: u32,
    /// Changed in the working tree and not staged.
    pub changed: u32,
    /// Not tracked by git.
    pub untracked: u32,
    /// Left in conflict by a merge.
    pub conflicts: u32,
    /// Commits on this branch and not its upstream, and the other way round.
    pub ahead: u32,
    pub behind: u32,
}

/// A repository: where git keeps it, and the folder it is checked out in.
#[derive(Debug, Clone, PartialEq)]
pub struct Repo {
    pub git_dir: PathBuf,
    pub work_tree: PathBuf,
}

/// The repository `dir` is in, looking up through the folders above it.
pub fn find(dir: &Path) -> Option<Repo> {
    for at in dir.ancestors() {
        let dot_git = at.join(".git");
        let Ok(meta) = std::fs::metadata(&dot_git) else { continue };
        if meta.is_dir() {
            // (A folder named `.git` that git would not take is not one.)
            if dot_git.join("HEAD").is_file() {
                return Some(Repo { git_dir: dot_git, work_tree: at.to_path_buf() });
            }
        } else if meta.is_file() {
            // A worktree or a submodule: `.git` is a file saying where.
            let text = std::fs::read_to_string(&dot_git).ok()?;
            let to = text.lines().next()?.strip_prefix("gitdir:")?.trim();
            let git_dir = if Path::new(to).is_absolute() { PathBuf::from(to) } else { at.join(to) };
            return git_dir.join("HEAD").is_file().then(|| Repo { git_dir, work_tree: at.to_path_buf() });
        }
    }
    None
}

/// The branch checked out, read from `HEAD`: `(name, detached)`.
pub fn head(git_dir: &Path) -> Option<(String, bool)> {
    let text = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let text = text.trim();
    if let Some(name) = text.strip_prefix("ref:") {
        let name = name.trim();
        return Some((name.strip_prefix("refs/heads/").unwrap_or(name).to_string(), false));
    }
    // A commit itself: the start of it is enough to know it by.
    (text.len() >= 7 && text.bytes().all(|b| b.is_ascii_hexdigit())).then(|| (text[..7].to_string(), true))
}

/// A merge, rebase, cherry-pick, revert or bisect that has stopped half way.
fn doing(git_dir: &Path) -> Option<&'static str> {
    // (In a worktree, a bisect is kept in the main repository's folder.)
    let common = std::fs::read_to_string(git_dir.join("commondir"))
        .ok()
        .map(|c| git_dir.join(c.trim()))
        .unwrap_or_else(|| git_dir.to_path_buf());
    let has = |name: &str| git_dir.join(name).exists();
    if has("rebase-merge") || has("rebase-apply/rebasing") {
        Some("rebasing")
    } else if has("rebase-apply/applying") {
        Some("applying")
    } else if has("MERGE_HEAD") {
        Some("merging")
    } else if has("CHERRY_PICK_HEAD") {
        Some("cherry-picking")
    } else if has("REVERT_HEAD") {
        Some("reverting")
    } else if common.join("BISECT_LOG").exists() {
        Some("bisecting")
    } else {
        None
    }
}

/// Everything there is to say of the repository `dir` is in; `None` when it
/// is in none. `status` says whether to ask git what has changed.
pub fn look(dir: &Path, status: bool) -> Option<Info> {
    let repo = find(dir)?;
    let (mut branch, detached) = head(&repo.git_dir)?;
    // During a rebase HEAD has let go of the branch: name the branch.
    let doing = doing(&repo.git_dir);
    if detached && doing == Some("rebasing") {
        for file in ["rebase-merge/head-name", "rebase-apply/head-name"] {
            if let Ok(name) = std::fs::read_to_string(repo.git_dir.join(file)) {
                branch = name.trim().strip_prefix("refs/heads/").unwrap_or(name.trim()).to_string();
                break;
            }
        }
    }
    let status = if status { ask_status(&repo.work_tree) } else { None };
    Some(Info { branch, detached, doing, status })
}

/// How long `git status` is given: a very large repository, or one on a
/// slow disk, still shows its branch.
const STATUS_LIMIT: Duration = Duration::from_secs(3);

/// What `git status` says has changed, if git is installed and answers in time.
fn ask_status(work_tree: &Path) -> Option<Counts> {
    let git = git_program()?;
    let mut child = Command::new(git)
        // Never take the index's lock: a `git commit` of the user's at the
        // same moment would fail on it.
        .arg("--no-optional-locks")
        // Nothing the repository's own settings name is run for this: a
        // folder from somewhere else may carry a `.git/config` of its own.
        .args(["-c", "core.fsmonitor=false"])
        .arg("-C")
        .arg(work_tree)
        .args(["status", "--porcelain=v2", "--branch"])
        // (Whatever repository the server's own environment names, this
        // folder's is the one to ask about.)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Read as it comes, so that a long answer cannot fill the pipe and stall it.
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut text = Vec::new();
        let _ = std::io::Read::read_to_end(&mut out, &mut text);
        text
    });
    let until = Instant::now() + STATUS_LIMIT;
    loop {
        match child.try_wait() {
            Ok(Some(done)) if done.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if Instant::now() >= until => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    let text = reader.join().ok()?;
    Some(parse_status(&String::from_utf8_lossy(&text)))
}

/// Count what `git status --porcelain=v2 --branch` lists.
pub fn parse_status(text: &str) -> Counts {
    let mut c = Counts::default();
    for line in text.lines() {
        let mut fields = line.split(' ');
        match fields.next() {
            Some("#") => {
                if fields.next() == Some("branch.ab") {
                    for n in fields {
                        if let Some(a) = n.strip_prefix('+') {
                            c.ahead = a.parse().unwrap_or(0);
                        } else if let Some(b) = n.strip_prefix('-') {
                            c.behind = b.parse().unwrap_or(0);
                        }
                    }
                }
            }
            // A change, or a rename or copy: XY is what the index has, then the tree.
            Some("1" | "2") => {
                let xy = fields.next().unwrap_or("..").as_bytes();
                if xy.first().is_some_and(|x| *x != b'.') {
                    c.staged += 1;
                }
                if xy.get(1).is_some_and(|y| *y != b'.') {
                    c.changed += 1;
                }
            }
            Some("u") => c.conflicts += 1,
            Some("?") => c.untracked += 1,
            _ => {}
        }
    }
    c
}

/// The git to run, if there is one that will not ask to be installed.
fn git_program() -> Option<&'static Path> {
    static GIT: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    GIT.get_or_init(|| {
        let path = std::env::var_os("PATH")?;
        let git = std::env::split_paths(&path).map(|d| d.join("git")).find(|p| is_program(p))?;
        // On a Mac without the developer tools, /usr/bin/git only offers to
        // install them, in a window of its own. `xcode-select -p` says
        // whether they are there, and offers nothing.
        if cfg!(target_os = "macos") && git == Path::new("/usr/bin/git") {
            let tools = Command::new("/usr/bin/xcode-select").arg("-p").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
            if !tools.is_ok_and(|s| s.success()) {
                return None;
            }
        }
        Some(git)
    })
    .as_deref()
}

fn is_program(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("astrum-git-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_branch_is_read_from_head_in_the_folder_or_above_it() {
        let dir = temp("branch");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join(".git/HEAD"), "ref: refs/heads/wave-7\n").unwrap();
        std::fs::create_dir_all(dir.join("src/deep")).unwrap();
        let info = look(&dir.join("src/deep"), false).unwrap();
        assert_eq!((info.branch.as_str(), info.detached, info.doing), ("wave-7", false, None));
        // A branch with a slash in its name keeps it.
        std::fs::write(dir.join(".git/HEAD"), "ref: refs/heads/feature/git-bar\n").unwrap();
        assert_eq!(look(&dir, false).unwrap().branch, "feature/git-bar");
        // No branch: the commit, shortened.
        std::fs::write(dir.join(".git/HEAD"), "0123456789abcdef0123456789abcdef01234567\n").unwrap();
        let info = look(&dir, false).unwrap();
        assert_eq!((info.branch.as_str(), info.detached), ("0123456", true));
        // Rebasing: HEAD is a commit, and the branch is named where the rebase keeps it.
        std::fs::create_dir_all(dir.join(".git/rebase-merge")).unwrap();
        std::fs::write(dir.join(".git/rebase-merge/head-name"), "refs/heads/main\n").unwrap();
        let info = look(&dir, false).unwrap();
        assert_eq!((info.branch.as_str(), info.doing), ("main", Some("rebasing")));
        std::fs::remove_dir_all(dir.join(".git/rebase-merge")).unwrap();
        std::fs::write(dir.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(dir.join(".git/MERGE_HEAD"), "0123456789abcdef0123456789abcdef01234567\n").unwrap();
        assert_eq!(look(&dir, false).unwrap().doing, Some("merging"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_worktree_is_found_through_the_file_that_says_where_its_repository_is() {
        let dir = temp("worktree");
        let kept = dir.join("main/.git/worktrees/site");
        std::fs::create_dir_all(&kept).unwrap();
        std::fs::write(kept.join("HEAD"), "ref: refs/heads/gh-pages\n").unwrap();
        std::fs::create_dir_all(dir.join("site")).unwrap();
        std::fs::write(dir.join("site/.git"), format!("gitdir: {}\n", kept.display())).unwrap();
        assert_eq!(look(&dir.join("site"), false).unwrap().branch, "gh-pages");
        // A relative one, as a submodule has.
        std::fs::write(dir.join("site/.git"), "gitdir: ../main/.git/worktrees/site\n").unwrap();
        assert_eq!(look(&dir.join("site"), false).unwrap().branch, "gh-pages");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_in_no_repository_has_nothing_to_show() {
        let dir = temp("none");
        assert_eq!(look(&dir, true), None);
        // Nor does a `.git` folder that is not a repository.
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        assert_eq!(look(&dir, true), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_has_changed_is_counted_from_git_status() {
        let text = "\
# branch.oid 1fbc4a5
# branch.head wave-7
# branch.upstream origin/wave-7
# branch.ab +2 -1
1 M. N... 100644 100644 100644 aaa bbb src/git.rs
1 .M N... 100644 100644 100644 aaa bbb src/ui/mod.rs
1 MM N... 100644 100644 100644 aaa bbb README.md
2 R. N... 100644 100644 100644 aaa bbb R100 new.rs\told.rs
u UU N... 100644 100644 100644 100644 aaa bbb ccc both.rs
? notes.txt
? \"a\\nb.txt\"
";
        let c = parse_status(text);
        assert_eq!(c, Counts { staged: 3, changed: 2, untracked: 2, conflicts: 1, ahead: 2, behind: 1 });
        assert_eq!(parse_status("# branch.oid (initial)\n# branch.head main\n"), Counts::default());
    }

    #[test]
    fn a_real_repository_is_asked_what_has_changed() {
        // (Only where git is installed.)
        if git_program().is_none() {
            return;
        }
        let dir = temp("real");
        let git = |args: &[&str]| {
            Command::new("git").arg("-C").arg(&dir).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap()
        };
        git(&["init", "-q", "-b", "trunk"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "one"]);
        std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        std::fs::write(dir.join("b.txt"), "new\n").unwrap();
        std::fs::write(dir.join("c.txt"), "staged\n").unwrap();
        git(&["add", "c.txt"]);
        let info = look(&dir, true).unwrap();
        assert_eq!(info.branch, "trunk");
        let c = info.status.unwrap();
        assert_eq!((c.staged, c.changed, c.untracked, c.conflicts), (1, 1, 1, 0));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
