//! A newer release, looked for and installed: what `astrum upgrade` does
//! before it moves sessions, so that one command brings a machine up to
//! date.
//!
//! The installing is done by the same script as a first install (it is
//! built into the program), so the download is checked the same way: over
//! https, against the release's checksums, and tried before it replaces
//! the copy that works.

use anyhow::{Context, Result, bail};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

const INSTALLER: &str = include_str!("../install.sh");
const REPOSITORY: &str = "robert-mcdermott/astrum";
const THIS: &str = env!("CARGO_PKG_VERSION");

/// A version's numbers: `v0.9.0`, `0.9.0` and `0.9.0-rc.1` alike.
fn numbers(version: &str) -> Option<(u64, u64, u64)> {
    let version = version.trim().trim_start_matches('v');
    let mut parts = version.split(['-', '+']).next()?.split('.');
    let found = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(found)
}

fn newer(version: &str, than: &str) -> bool {
    matches!((numbers(version), numbers(than)), (Some(a), Some(b)) if a > b)
}

/// The latest release's version, if it can be learned: where the address
/// of "the latest release" leads says it.
fn latest() -> Option<String> {
    // (A test says what is out; nothing built for testing asks the network.)
    if cfg!(debug_assertions) {
        return std::env::var("ASTRUM_TEST_LATEST").ok();
    }
    let repository = std::env::var("ASTRUM_REPOSITORY").unwrap_or_else(|_| REPOSITORY.into());
    let out = Command::new("curl")
        .args([
            "--proto",
            "=https",
            "--tlsv1.2",
            "--fail",
            "--silent",
            "--location",
            "--head",
            "--connect-timeout",
            "3",
            "--max-time",
            "6",
            "--output",
            "/dev/null",
            "--write-out",
            "%{url_effective}",
        ])
        .arg(format!("https://github.com/{repository}/releases/latest"))
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let url = String::from_utf8(out.stdout).ok()?;
    let version = url.trim().rsplit_once("/releases/tag/v")?.1;
    numbers(version).map(|_| version.to_string())
}

/// Where this program is, and so where a newer one goes.
fn install_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("ASTRUM_INSTALL_DIR").filter(|d| !d.is_empty()) {
        return Ok(dir.into());
    }
    // (Where it really is: a link to it is left a link, and the program it leads to is the one replaced.)
    let exe = std::env::current_exe().context("locating the astrum executable")?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    exe.parent().map(PathBuf::from).context("locating the astrum executable's folder")
}

/// Can a file be put in this folder (or the folder be made)?
fn writable(dir: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Some(there) = dir.ancestors().find(|d| d.exists()) else { return false };
    let Ok(path) = std::ffi::CString::new(there.as_os_str().as_bytes()) else { return false };
    there.is_dir() && unsafe { libc::access(path.as_ptr(), libc::W_OK) } == 0
}

fn asked(question: &str) -> bool {
    print!("{question} [Y/n] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    // (Nothing read at all, as when the terminal goes away: that is no answer, and so no.)
    if !matches!(std::io::stdin().read_line(&mut line), Ok(read) if read > 0) {
        println!();
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes")
}

/// Look for a newer release. If there is one it is installed, when that is
/// asked for (`fetch`) or agreed to at a terminal, and the new program
/// carries on with `then` in this one's place: this returns only when
/// nothing was installed.
pub fn offer(fetch: bool, then: &[String]) -> Result<()> {
    let Some(latest) = latest() else {
        if fetch {
            bail!(
                "could not learn what the latest release is (is this machine online, and is curl installed?)"
            );
        }
        return Ok(());
    };
    if !newer(&latest, THIS) {
        if fetch {
            println!("There is no release newer than this Astrum ({THIS}): nothing to install.");
        }
        return Ok(());
    }
    let dir = install_dir()?;
    let dest = dir.join("astrum");
    let shown = crate::paths::tildify(&dest.to_string_lossy());
    // Somewhere this user cannot write (a system folder, say): said before anything is asked.
    if !writable(&dir) {
        let why = format!("Astrum {latest} is out (this is {THIS}), but it cannot be installed at {shown}: that folder cannot be written. Install it the way this one was installed, then run `astrum upgrade`");
        if fetch {
            bail!("{why}");
        }
        eprintln!("{why}.");
        return Ok(());
    }
    if !fetch {
        if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
            eprintln!(
                "Astrum {latest} is out (this is {THIS}). `astrum upgrade --fetch` installs it and moves the sessions onto it."
            );
            return Ok(());
        }
        println!("Astrum {latest} is out (this is {THIS}).");
        if !asked(&format!(
            "Install it at {shown}, and move the sessions onto it?"
        )) {
            println!("Not installed.");
            return Ok(());
        }
    }
    let mut installer = Command::new("sh")
        .args([
            "-s",
            "--",
            "--version",
            &format!("v{latest}"),
            "--install-dir",
        ])
        .arg(&dir)
        .env("ASTRUM_FROM_UPGRADE", "1")
        .stdin(Stdio::piped())
        .spawn()
        .context("starting the installer")?;
    if let Some(mut stdin) = installer.stdin.take() {
        stdin
            .write_all(INSTALLER.as_bytes())
            .context("starting the installer")?;
    }
    if !installer
        .wait()
        .context("waiting for the installer")?
        .success()
    {
        bail!(
            "Astrum {latest} was not installed (the installer says why, above). Nothing was changed: the sessions carry on as they were"
        );
    }
    println!();
    // The rest is the new program's to do: it is the one sessions move onto.
    use std::os::unix::process::CommandExt;
    let failed = Command::new(&dest).args(then).exec();
    Err(failed).with_context(|| format!("starting the Astrum just installed at {shown}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_compared_by_their_numbers() {
        assert!(newer("0.10.0", "0.9.0"));
        assert!(newer("v1.0.0", "0.99.99"));
        assert!(!newer("0.9.0", "0.9.0"));
        assert!(!newer("0.8.3", "0.9.0"));
        assert!(newer("0.9.1-rc.1", "0.9.0"));
        // (What is not a version is never newer.)
        assert!(!newer("latest", "0.9.0"));
        assert!(!newer("9.9", "0.9.0"));
    }
}
