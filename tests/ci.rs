//! Tests of the two scripts that keep code from being tested twice:
//! `.github/scripts/already-tested.sh`, which CI and the release build ask
//! before running the suite, and `scripts/test.sh`, the same question for
//! a run made by hand. Each is run for real, in a repository made for the
//! test, with stand-ins for `gh`, `cargo` and `rustc` in front of the real
//! ones. (What the stand-in for `gh` is given to filter with is not
//! applied by it: that expression was tried against GitHub itself.)

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);
const ASKS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/.github/scripts/already-tested.sh");
const KEY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/.github/scripts/code-key.sh");
const BY_HAND: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/test.sh");

/// A repository of its own, and a folder of stand-in programs.
struct Scene {
    root: PathBuf,
}

impl Scene {
    fn new() -> Scene {
        use std::os::unix::fs::PermissionsExt;
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("astrum-ci-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("repo")).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        // Each stand-in notes how it was called, says what its `.says`
        // file holds, and ends as its `.ends` file tells it to.
        for name in ["gh", "cargo", "rustc"] {
            let path = root.join("bin").join(name);
            let body = format!("#!/bin/sh\necho \"$@\" >> '{0}/{name}.calls'\ncat '{0}/{name}.says' 2>/dev/null\nexit \"$(cat '{0}/{name}.ends' 2>/dev/null || echo 0)\"\n", root.display());
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let scene = Scene { root };
        scene.says("rustc", "rustc 1.99.0 (stand-in)\n");
        scene.git(&["init", "-q", "-b", "main"]);
        scene.write(".gitignore", "target/\n");
        // (The script run by hand finds the one that names the code where
        // it is in a checkout.)
        std::fs::create_dir_all(scene.repo().join(".github/scripts")).unwrap();
        std::fs::copy(KEY, scene.repo().join(".github/scripts/code-key.sh")).unwrap();
        scene.write("code.txt", "one\n");
        scene.commit("the first state");
        scene
    }

    fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.repo().join(name), text).unwrap();
    }

    fn says(&self, program: &str, text: &str) {
        std::fs::write(self.root.join(format!("{program}.says")), text).unwrap();
    }

    fn ends(&self, program: &str, code: i32) {
        std::fs::write(self.root.join(format!("{program}.ends")), code.to_string()).unwrap();
    }

    /// How a stand-in has been called since this was last asked.
    fn calls(&self, program: &str) -> Vec<String> {
        let path = self.root.join(format!("{program}.calls"));
        let all = std::fs::read_to_string(&path).unwrap_or_default();
        let _ = std::fs::remove_file(path);
        all.lines().map(str::to_string).collect()
    }

    fn command(&self, program: &str) -> Command {
        let mut cmd = Command::new(program);
        let path = format!("{}:{}", self.root.join("bin").display(), std::env::var("PATH").unwrap_or_default());
        cmd.current_dir(self.repo()).env("PATH", path).env("HOME", &self.root);
        // Nothing of the machine's own git setup, or of a run of CI these tests are part of.
        cmd.env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_SYSTEM", "/dev/null");
        for name in ["GITHUB_OUTPUT", "GITHUB_STEP_SUMMARY", "GITHUB_EVENT_NAME", "GITHUB_REPOSITORY", "GH_TOKEN", "PR_HEAD_REPO", "PR_HEAD_REF", "FORCE", "GIT_DIR", "GIT_INDEX_FILE", "GIT_WORK_TREE"] {
            cmd.env_remove(name);
        }
        cmd
    }

    fn git(&self, args: &[&str]) -> String {
        let out = self.command("git").args(["-c", "user.name=Test", "-c", "user.email=test@example.invalid", "-c", "commit.gpgsign=false"]).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
    }

    /// The name the code that is checked out goes by.
    fn key(&self) -> String {
        let out = self.command("sh").arg(KEY).output().unwrap();
        assert!(out.status.success(), "{}", text(&out));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Ask, as a workflow would for this kind of event: whether the
    /// suite is to be run, and everything that was said.
    fn ask(&self, event: &str, more: &[(&str, &str)]) -> (bool, String) {
        let written = self.root.join("output");
        let _ = std::fs::remove_file(&written);
        let mut cmd = self.command("sh");
        cmd.arg(ASKS).env("GITHUB_OUTPUT", &written).env("GITHUB_REPOSITORY", "someone/astrum").env("GITHUB_EVENT_NAME", event).env("GH_TOKEN", "none");
        for (name, value) in more {
            cmd.env(name, value);
        }
        let out = cmd.output().unwrap();
        let said = text(&out);
        assert!(out.status.success(), "{said}");
        let written = std::fs::read_to_string(written).unwrap_or_default();
        assert!(written.contains(&format!("key={}\n", self.key())), "{written}");
        let run = match (written.contains("run=true\n"), written.contains("run=false\n")) {
            (true, false) => true,
            (false, true) => false,
            _ => panic!("neither or both: {written}"),
        };
        (run, said)
    }

    /// Run the suite by hand, as far as it is run.
    fn by_hand(&self, args: &[&str]) -> Output {
        self.command("sh").arg(BY_HAND).args(args).output().unwrap()
    }
}

impl Drop for Scene {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

fn script(path: &str) -> &Path {
    let path = Path::new(path);
    assert!(path.exists(), "{}", path.display());
    path
}

// ---- in CI ----------------------------------------------------------------------

#[test]
fn code_that_has_not_passed_is_tested_and_code_that_has_is_not() {
    script(ASKS);
    let s = Scene::new();
    let (run, said) = s.ask("push", &[]);
    assert!(run && said.contains("has not passed before"), "{said}");
    // What was looked for is a note under the tree's own name.
    let asked = s.calls("gh");
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(asked[0].starts_with(&format!("api repos/someone/astrum/actions/artifacts?name=tests-passed-{}&", s.key())), "{asked:?}");

    // A run that passed it has left one.
    s.says("gh", "43152317434\n");
    let (run, said) = s.ask("push", &[]);
    assert!(!run && said.contains("passed in https://github.com/someone/astrum/actions/runs/43152317434"), "{said}");

    // Change a file, and it is another state of the code.
    let before = s.key();
    s.write("code.txt", "two\n");
    s.commit("the second state");
    assert_ne!(s.key(), before);
    s.calls("gh");
    s.ask("push", &[]);
    assert!(s.calls("gh")[0].contains(&format!("tests-passed-{}", s.key())));
}

#[test]
fn a_note_in_the_roadmap_or_the_changelog_is_not_a_change_to_the_code() {
    script(KEY);
    let s = Scene::new();
    let code = s.key();
    assert_eq!(code.len(), 40, "{code}");
    for name in ["ROADMAP.md", "CHANGELOG.md"] {
        s.write(name, "Slice 9: done\n");
        s.commit("a note");
        assert_eq!(s.key(), code, "{name}");
    }
    s.write("ROADMAP.md", "Slice 9: done, and tried\n");
    s.commit("another note");
    assert_eq!(s.key(), code);
    // So what passed before the note still counts after it.
    s.says("gh", "77\n");
    assert!(!s.ask("push", &[]).0);
    assert!(s.calls("gh")[0].contains(&format!("tests-passed-{code}")));

    // Anything else is a change: the README, a file of the same name
    // somewhere else, a file that goes.
    s.write("README.md", "How to use it\n");
    s.commit("the README");
    let with_readme = s.key();
    assert_ne!(with_readme, code);
    std::fs::create_dir_all(s.repo().join("doc")).unwrap();
    s.write("doc/ROADMAP.md.txt", "not the roadmap\n");
    s.commit("a file that only looks like it");
    assert_ne!(s.key(), with_readme);
    let before = s.key();
    s.git(&["rm", "-q", "code.txt"]);
    s.commit("a file goes");
    assert_ne!(s.key(), before);
    // And the same files reached another way are the same code.
    assert_eq!(s.git(&["rev-parse", "HEAD^{tree}"]).len(), 40);
    let out = s.command("sh").arg(KEY).arg("HEAD^{tree}").output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), s.key());
}

#[test]
fn a_merge_that_brings_in_only_its_branch_is_the_same_code() {
    let s = Scene::new();
    s.git(&["checkout", "-q", "-b", "wave-9"]);
    s.write("code.txt", "one\nand more\n");
    s.commit("work on the branch");
    let branch = s.key();
    // Merged into a main that has not moved, with a commit of its own for the merge.
    s.git(&["checkout", "-q", "main"]);
    s.git(&["merge", "-q", "--no-ff", "-m", "Merge wave-9", "wave-9"]);
    assert_eq!(s.key(), branch, "another commit, the same files");
    // So what the branch's run left is found by main's, and by a tag's.
    s.says("gh", "77\n");
    assert!(!s.ask("push", &[]).0);
    s.git(&["tag", "v9.0.0"]);
    assert!(!s.ask("push", &[]).0);
}

#[test]
fn a_pull_request_that_is_exactly_its_branch_is_left_to_the_push() {
    let s = Scene::new();
    s.git(&["checkout", "-q", "-b", "wave-9"]);
    s.write("code.txt", "one\nand more\n");
    s.commit("work on the branch");
    // What a pull request's run checks out: the branch merged into its base.
    s.git(&["checkout", "-q", "--detach", "main"]);
    s.git(&["merge", "-q", "--no-ff", "-m", "Merge wave-9 into main", "wave-9"]);
    let ours = [("PR_HEAD_REPO", "someone/astrum"), ("PR_HEAD_REF", "wave-9")];
    let (run, said) = s.ask("pull_request", &ours);
    assert!(!run && said.contains("the push to wave-9 is tested by a run of its own"), "{said}");
    assert!(s.calls("gh").is_empty(), "nothing had to be looked up");

    // The same merge, but not a branch whose pushes are tested, or not
    // this repository's branch at all: this run is the only one there is.
    assert!(s.ask("pull_request", &[("PR_HEAD_REPO", "someone/astrum"), ("PR_HEAD_REF", "fix-a-typo")]).0);
    assert!(s.ask("pull_request", &[("PR_HEAD_REPO", "stranger/astrum"), ("PR_HEAD_REF", "wave-9")]).0);
    assert!(s.ask("pull_request", &[("PR_HEAD_REF", "wave-9")]).0);
    // And a push is never taken for one.
    assert!(s.ask("push", &ours).0);
}

#[test]
fn a_pull_request_whose_base_has_moved_is_tested_as_the_merge_it_is() {
    let s = Scene::new();
    s.git(&["checkout", "-q", "-b", "wave-9"]);
    s.write("code.txt", "one\nand more\n");
    s.commit("work on the branch");
    s.git(&["checkout", "-q", "main"]);
    s.write("other.txt", "meanwhile, on main\n");
    s.commit("main moves on");
    s.git(&["checkout", "-q", "--detach", "main"]);
    s.git(&["merge", "-q", "--no-ff", "-m", "Merge wave-9 into main", "wave-9"]);
    // Merged, it is not what the push to the branch tested.
    let (run, said) = s.ask("pull_request", &[("PR_HEAD_REPO", "someone/astrum"), ("PR_HEAD_REF", "wave-9")]);
    assert!(run && said.contains("has not passed before"), "{said}");
    assert_eq!(s.calls("gh").len(), 1);
}

#[test]
fn whenever_it_cannot_tell_the_code_is_tested() {
    let s = Scene::new();
    // GitHub does not answer.
    s.ends("gh", 1);
    s.says("gh", "{\"message\":\"Not Found\"}\n");
    let (run, said) = s.ask("push", &[]);
    // (And it says what it was told, for whoever reads the run.)
    assert!(run && said.contains("could not be asked") && said.contains("Not Found"), "{said}");
    // It answers something that is not the number of a run.
    s.ends("gh", 0);
    for answer in ["null\n", "12 34\n", "; rm -rf /\n", "-5\n", ""] {
        s.says("gh", answer);
        assert!(s.ask("push", &[]).0, "{answer:?}");
    }
    s.says("gh", "null\n");
    assert!(s.ask("push", &[]).1.contains("not the number of a run"));
    // There is no `gh` to ask.
    s.ends("gh", 127);
    s.says("gh", "");
    assert!(s.ask("push", &[]).0);
}

#[test]
fn a_run_asked_for_by_hand_runs_everything() {
    let s = Scene::new();
    s.says("gh", "43152317434\n");
    assert!(!s.ask("workflow_dispatch", &[]).0, "by hand, but not told to run regardless (the release build)");
    let (run, said) = s.ask("workflow_dispatch", &[("FORCE", "true")]);
    assert!(run && said.contains("Asked for by hand"), "{said}");
    assert!(s.ask("workflow_dispatch", &[("FORCE", "false")]).1.contains("Not run again"));
}

// ---- by hand --------------------------------------------------------------------

#[test]
fn by_hand_the_suite_is_run_once_for_one_state_of_the_code() {
    script(BY_HAND);
    let s = Scene::new();
    let first = s.by_hand(&[]);
    assert!(first.status.success() && text(&first).contains("Passed, and noted"), "{}", text(&first));
    assert_eq!(s.calls("cargo"), ["test --locked"]);

    let second = s.by_hand(&[]);
    assert!(second.status.success() && text(&second).contains("Not run again: this code") && text(&second).contains("has passed here"), "{}", text(&second));
    assert!(s.calls("cargo").is_empty(), "nothing was run");

    // Told to run it all the same.
    assert!(s.by_hand(&["--again"]).status.success());
    assert_eq!(s.calls("cargo"), ["test --locked"]);
}

#[test]
fn by_hand_any_change_is_another_state_committed_or_not() {
    let s = Scene::new();
    s.by_hand(&[]);
    s.calls("cargo");
    // A file changed and not committed.
    s.write("code.txt", "edited\n");
    s.by_hand(&[]);
    assert_eq!(s.calls("cargo").len(), 1, "an edit");
    s.by_hand(&[]);
    assert!(s.calls("cargo").is_empty(), "the same edit, again");
    // A new file nobody has added.
    s.write("new.txt", "new\n");
    s.by_hand(&[]);
    assert_eq!(s.calls("cargo").len(), 1, "a new file");
    // Nothing was done to the index to find that out.
    assert_eq!(s.git(&["status", "--porcelain"]), "M code.txt\n?? new.txt", "(as `git` prints it, less the space it begins with)");
    // Committing what was tested does not make it new.
    s.commit("as tested");
    s.by_hand(&[]);
    assert!(s.calls("cargo").is_empty(), "committed as it was tested");
    // What is ignored is not the code. Another compiler is another matter.
    std::fs::create_dir_all(s.repo().join("target")).unwrap();
    s.write("target/built", "something built\n");
    s.by_hand(&[]);
    assert!(s.calls("cargo").is_empty(), "an ignored file");
    s.says("rustc", "rustc 2.0.0 (stand-in)\n");
    s.by_hand(&[]);
    assert_eq!(s.calls("cargo").len(), 1, "another compiler");
}

#[test]
fn by_hand_a_run_that_fails_is_not_remembered_and_a_part_of_the_suite_never_is() {
    let s = Scene::new();
    s.ends("cargo", 101);
    let failed = s.by_hand(&[]);
    assert_eq!(failed.status.code(), Some(101), "{}", text(&failed));
    assert!(!text(&failed).contains("noted"));
    s.calls("cargo");
    s.ends("cargo", 0);
    s.by_hand(&[]);
    assert_eq!(s.calls("cargo").len(), 1, "it had not passed");

    // Part of the suite: run as asked, every time, and it says nothing about the whole.
    let s = Scene::new();
    assert!(s.by_hand(&["--", "--test", "e2e", "a_tab"]).status.success());
    assert!(s.by_hand(&["--", "--test", "e2e", "a_tab"]).status.success());
    assert_eq!(s.calls("cargo"), ["test --locked --test e2e a_tab", "test --locked --test e2e a_tab"]);
    s.by_hand(&[]);
    assert_eq!(s.calls("cargo"), ["test --locked"]);
    // And something it does not know is not taken for a go-ahead.
    assert_eq!(s.by_hand(&["--agian"]).status.code(), Some(2));
    assert!(s.calls("cargo").is_empty());
}

#[test]
fn by_hand_what_ci_has_passed_is_not_run_here() {
    let s = Scene::new();
    s.says("gh", "43152317434\n");
    let out = s.by_hand(&[]);
    assert!(out.status.success() && text(&out).contains("passed in CI"), "{}", text(&out));
    assert!(s.calls("cargo").is_empty());
    assert!(s.calls("gh")[0].contains(&format!("tests-passed-{}", s.key())));
    // Only for code as it is committed: CI has seen nothing else.
    s.write("code.txt", "edited\n");
    s.by_hand(&[]);
    assert_eq!(s.calls("cargo").len(), 1);
    assert!(s.calls("gh").is_empty());
}
