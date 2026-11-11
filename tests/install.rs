//! Tests of `install.sh`, run for real against a release laid out in a
//! directory on this machine (the script takes `file://` as well as
//! `https://`). The binary "released" is the one cargo just built.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_astrum");
static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// The name of the release file for the machine the tests run on.
fn asset() -> String {
    let os = if cfg!(target_os = "macos") { "darwin" } else { "linux" };
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "amd64" };
    format!("astrum-{os}-{arch}")
}

fn sha256(path: &Path) -> String {
    let try_with = |program: &str, args: &[&str]| {
        Command::new(program).args(args).arg(path).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().next().unwrap_or("").to_string())
    };
    try_with("sha256sum", &[]).or_else(|| try_with("shasum", &["-a", "256"])).expect("sha256sum or shasum")
}

/// A scratch directory with a release in it and somewhere to install to.
struct Scene {
    root: PathBuf,
}

impl Scene {
    fn new() -> Scene {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("astrum-install-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("release")).unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        let scene = Scene { root };
        std::fs::copy(BIN, scene.release().join(asset())).unwrap();
        scene.write_sums();
        scene
    }

    fn release(&self) -> PathBuf {
        self.root.join("release")
    }

    fn bin_dir(&self) -> PathBuf {
        self.root.join("home/.local/bin")
    }

    /// (Re)write SHA256SUMS for what is in the release directory, with an
    /// entry for another file as a real release has.
    fn write_sums(&self) {
        let ours = sha256(&self.release().join(asset()));
        let other = "0".repeat(64);
        std::fs::write(self.release().join("SHA256SUMS"), format!("{other}  astrum-other-machine\n{ours}  {}\n", asset())).unwrap();
    }

    /// Run the installer; its exit status and everything it printed.
    fn install(&self, env: &[(&str, &str)], args: &[&str]) -> (bool, String) {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("install.sh");
        let mut cmd = Command::new("sh");
        cmd.arg(script).args(args);
        for var in ["ASTRUM_VERSION", "ASTRUM_INSTALL_DIR", "ASTRUM_REPOSITORY", "ASTRUM_DOWNLOAD_BASE"] {
            cmd.env_remove(var);
        }
        cmd.env("HOME", self.root.join("home"));
        cmd.env("ASTRUM_DOWNLOAD_BASE", format!("file://{}", self.release().display()));
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr))
    }
}

impl Drop for Scene {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn version() -> String {
    format!("astrum {}", env!("CARGO_PKG_VERSION"))
}

#[test]
fn a_release_is_verified_and_installed() {
    let scene = Scene::new();
    let (ok, out) = scene.install(&[], &[]);
    assert!(ok, "{out}");
    assert!(out.contains("Checksum verified") && out.contains(&version()), "{out}");
    let installed = scene.bin_dir().join("astrum");
    assert!(out.contains(&format!("Installed at {}", installed.display())), "{out}");
    // It is there, runs, and says where it was put when that is not on PATH.
    let said = Command::new(&installed).arg("--version").output().unwrap();
    assert_eq!(String::from_utf8_lossy(&said.stdout).trim(), version());
    assert!(out.contains("is not in PATH"), "{out}");
    // Nothing is left behind beside it.
    let beside: Vec<String> = std::fs::read_dir(scene.bin_dir()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(beside, vec!["astrum".to_string()]);

    // A version and a directory can be asked for.
    let elsewhere = scene.root.join("tools");
    let wanted = format!("v{}", env!("CARGO_PKG_VERSION"));
    let (ok, out) = scene.install(&[], &["--version", &wanted, "--install-dir", &elsewhere.to_string_lossy()]);
    assert!(ok, "{out}");
    assert!(elsewhere.join("astrum").exists());
    // And with the directory on PATH there is nothing to warn about.
    let path = format!("{}:{}", elsewhere.display(), std::env::var("PATH").unwrap_or_default());
    let (ok, out) = scene.install(&[("ASTRUM_INSTALL_DIR", &elsewhere.to_string_lossy()), ("PATH", &path)], &[]);
    assert!(ok && !out.contains("is not in PATH"), "{out}");
}

#[test]
fn a_download_that_does_not_match_its_checksum_is_refused() {
    let scene = Scene::new();
    let (ok, out) = scene.install(&[], &[]);
    assert!(ok, "{out}");
    let installed = scene.bin_dir().join("astrum");
    let before = sha256(&installed);

    // The file changes after the checksums were written: tampered with, or cut short.
    let released = scene.release().join(asset());
    let mut bytes = std::fs::read(&released).unwrap();
    bytes.extend_from_slice(b"extra");
    std::fs::write(&released, bytes).unwrap();
    let (ok, out) = scene.install(&[], &[]);
    assert!(!ok && out.contains("checksum verification failed"), "{out}");
    // The copy that was working is still the one installed.
    assert_eq!(sha256(&installed), before);

    // No entry for this machine's file, or more than one: refused too.
    std::fs::write(scene.release().join("SHA256SUMS"), format!("{}  astrum-other-machine\n", "0".repeat(64))).unwrap();
    let (ok, out) = scene.install(&[], &[]);
    assert!(!ok && out.contains("does not contain exactly one entry"), "{out}");
    let sum = sha256(&released);
    std::fs::write(scene.release().join("SHA256SUMS"), format!("{sum}  {a}\n{sum}  {a}\n", a = asset())).unwrap();
    let (ok, out) = scene.install(&[], &[]);
    assert!(!ok && out.contains("does not contain exactly one entry"), "{out}");
    // Something that is not a checksum at all.
    std::fs::write(scene.release().join("SHA256SUMS"), format!("not-a-checksum  {}\n", asset())).unwrap();
    let (ok, out) = scene.install(&[], &[]);
    assert!(!ok && out.contains("invalid SHA-256 value"), "{out}");
    assert_eq!(sha256(&installed), before);
}

#[test]
fn a_file_that_is_not_the_program_asked_for_is_not_installed() {
    let scene = Scene::new();
    let (ok, out) = scene.install(&[], &[]);
    assert!(ok, "{out}");
    let installed = scene.bin_dir().join("astrum");
    let before = sha256(&installed);
    let released = scene.release().join(asset());

    // The checksum matches, but what was released does not run.
    std::fs::write(&released, "#!/bin/sh\nexit 1\n").unwrap();
    scene.write_sums();
    let (ok, out) = scene.install(&[], &[]);
    assert!(!ok && out.contains("did not pass its version check"), "{out}");
    // It runs, but is something else.
    std::fs::write(&released, "#!/bin/sh\necho 'something else 1.0'\n").unwrap();
    scene.write_sums();
    let (ok, out) = scene.install(&[], &[]);
    assert!(!ok && out.contains("unexpected version"), "{out}");
    // It is Astrum, but not the version asked for.
    std::fs::copy(BIN, &released).unwrap();
    scene.write_sums();
    let (ok, out) = scene.install(&[("ASTRUM_VERSION", "v99.0.0")], &[]);
    assert!(!ok && out.contains("unexpected version"), "{out}");

    assert_eq!(sha256(&installed), before, "the working copy was never replaced");
    let beside: Vec<String> = std::fs::read_dir(scene.bin_dir()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(beside, vec!["astrum".to_string()], "nothing half-installed is left behind");
}

#[test]
fn what_it_is_asked_to_do_is_checked_first() {
    let scene = Scene::new();
    for (env, args, complaint) in [
        (vec![("ASTRUM_DOWNLOAD_BASE", "http://example.com/files")], vec![], "must start with https:// or file:///"),
        (vec![("ASTRUM_INSTALL_DIR", "relative/dir")], vec![], "must be an absolute path"),
        (vec![("ASTRUM_VERSION", "newest")], vec![], "invalid release version"),
        (vec![("ASTRUM_REPOSITORY", "not a repository")], vec![], "ASTRUM_REPOSITORY"),
        (vec![], vec!["--frobnicate"], "unknown argument"),
        (vec![], vec!["--version"], "requires a value"),
    ] {
        let (ok, out) = scene.install(&env, &args);
        assert!(!ok && out.contains(complaint), "{env:?} {args:?}: {out}");
    }
    assert!(!scene.bin_dir().join("astrum").exists());
    let (ok, out) = scene.install(&[], &["--help"]);
    assert!(ok && out.contains("verifies the SHA-256 checksum"), "{out}");
}
