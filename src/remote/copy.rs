//! Finding the Astrum to put on another machine: the release build for
//! that machine's kind of system, of the version running here, with its
//! checksum verified on this machine before a byte of it is sent.
//!
//! It is looked for, in turn: where `download_base` (or
//! `ASTRUM_DOWNLOAD_BASE`) says the release files are, as for the install
//! script; among those fetched before; and in the release on GitHub. The
//! other machine needs no way out to the internet: the file goes to it
//! over the connection that is already there.

use crate::paths;
use aws_lc_rs::digest;
use std::path::{Path, PathBuf};

const REPOSITORY: &str = "robert-mcdermott/astrum";
const SUMS: &str = "SHA256SUMS";
/// A build is a few megabytes. Anything much larger is not one.
const MAX_BYTES: usize = 200 * 1024 * 1024;

pub struct Build {
    pub bytes: Vec<u8>,
    /// Where it came from, for telling the user.
    pub from: String,
}

fn sha256(bytes: &[u8]) -> String {
    digest::digest(&digest::SHA256, bytes).as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// The checksum a list of them gives for `asset`. (Lines of `sha256sum`:
/// the sum, then the name, which may have a `*` before it.)
fn listed(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        let (sum, name) = (words.next()?, words.next()?);
        (name.trim_start_matches('*') == asset && sum.len() == 64 && sum.chars().all(|c| c.is_ascii_hexdigit())).then(|| sum.to_ascii_lowercase())
    })
}

fn verified(bytes: Vec<u8>, sums: &str, asset: &str, from: String) -> Result<Build, String> {
    let Some(wanted) = listed(sums, asset) else {
        return Err(format!("the {SUMS} from {from} has no checksum for {asset}"));
    };
    let is = sha256(&bytes);
    if is != wanted {
        return Err(format!("{asset} from {from} does not match its checksum (it is {is}, and {SUMS} says {wanted}). Nothing was copied."));
    }
    Ok(Build { bytes, from })
}

fn from_dir(dir: &Path, asset: &str) -> Result<Build, String> {
    let read = |name: &str| std::fs::read(dir.join(name)).map_err(|e| format!("{}: {e}", dir.join(name).display()));
    let (bytes, sums) = (read(asset)?, read(SUMS)?);
    verified(bytes, &String::from_utf8_lossy(&sums), asset, paths::tildify(&dir.to_string_lossy()))
}

async fn fetch(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let reply = client.get(url).timeout(std::time::Duration::from_secs(120)).send().await.map_err(|e| format!("{url}: {e}"))?;
    if !reply.status().is_success() {
        return Err(format!("{url}: {}", reply.status()));
    }
    let bytes = reply.bytes().await.map_err(|e| format!("{url}: {e}"))?;
    if bytes.len() > MAX_BYTES {
        return Err(format!("{url}: {} bytes is too large to be a build of Astrum", bytes.len()));
    }
    Ok(bytes.to_vec())
}

async fn from_web(client: &reqwest::Client, base: &str, asset: &str) -> Result<Build, String> {
    let sums = fetch(client, &format!("{base}/{SUMS}")).await?;
    let bytes = fetch(client, &format!("{base}/{asset}")).await?;
    verified(bytes, &String::from_utf8_lossy(&sums), asset, base.to_string())
}

fn kept_dir() -> PathBuf {
    paths::data_dir().join("builds").join(env!("CARGO_PKG_VERSION"))
}

/// The build named `asset` (see `super::asset`) of this version.
/// `base`: where the user has said the release files are, if they have.
pub async fn find(asset: &str, base: Option<&str>, client: &reqwest::Client) -> Result<Build, String> {
    let version = env!("CARGO_PKG_VERSION");
    let base = std::env::var("ASTRUM_DOWNLOAD_BASE").ok().filter(|b| !b.is_empty()).or_else(|| base.map(str::to_string));
    if let Some(base) = base {
        let base = base.trim_end_matches('/');
        return match base.strip_prefix("file://") {
            Some(dir) if dir.starts_with('/') => from_dir(Path::new(dir), asset),
            None if base.starts_with("https://") => from_web(client, base, asset).await,
            _ => Err("download_base must start with https:// or file:///".into()),
        };
    }
    let kept = kept_dir();
    if let Ok(build) = from_dir(&kept, asset) {
        return Ok(build);
    }
    if version.contains('-') {
        return Err(format!(
            "this is a development build of Astrum ({version}), and there is no release of it to copy. Put Astrum there yourself, or build it for that machine and set `download_base` to file:// and the folder holding {asset} and a {SUMS} for it."
        ));
    }
    let base = format!("https://github.com/{REPOSITORY}/releases/download/v{version}");
    let build = from_web(client, &base, asset).await?;
    // Kept for the next machine, or the next time.
    let keep = || -> std::io::Result<()> {
        std::fs::create_dir_all(&kept)?;
        std::fs::write(kept.join(asset), &build.bytes)?;
        let line = format!("{}  {asset}\n", sha256(&build.bytes));
        let mut sums = std::fs::read_to_string(kept.join(SUMS)).unwrap_or_default();
        if listed(&sums, asset).is_none() {
            sums.push_str(&line);
            std::fs::write(kept.join(SUMS), sums)?;
        }
        Ok(())
    };
    let _ = keep();
    Ok(build)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("astrum-copy-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_checksum_is_known_to_be_right() {
        // (The one everybody knows: of nothing at all.)
        assert_eq!(sha256(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn the_list_is_read_as_sha256sum_writes_it() {
        let sums = format!("{}  astrum-linux-amd64\n{} *astrum-linux-arm64\nnot a line\nabc  astrum-darwin-arm64\n", "a".repeat(64), "B".repeat(64));
        assert_eq!(listed(&sums, "astrum-linux-amd64"), Some("a".repeat(64)));
        assert_eq!(listed(&sums, "astrum-linux-arm64"), Some("b".repeat(64)));
        assert_eq!(listed(&sums, "astrum-darwin-arm64"), None);
        assert_eq!(listed(&sums, "astrum"), None);
    }

    #[test]
    fn only_a_build_that_matches_its_checksum_is_taken() {
        let dir = scratch("verified");
        std::fs::write(dir.join("astrum-linux-amd64"), b"a build").unwrap();
        std::fs::write(dir.join(SUMS), format!("{}  astrum-linux-amd64\n", sha256(b"a build"))).unwrap();
        assert_eq!(from_dir(&dir, "astrum-linux-amd64").map(|b| b.bytes).unwrap(), b"a build");

        // Changed since the list was made.
        std::fs::write(dir.join("astrum-linux-amd64"), b"another build").unwrap();
        let why = from_dir(&dir, "astrum-linux-amd64").err().unwrap();
        assert!(why.contains("does not match its checksum") && why.contains("Nothing was copied"), "{why}");

        // Not in the list at all, and not there at all.
        std::fs::write(dir.join("astrum-linux-arm64"), b"a build").unwrap();
        assert!(from_dir(&dir, "astrum-linux-arm64").err().unwrap().contains("no checksum for astrum-linux-arm64"));
        assert!(from_dir(&dir, "astrum-darwin-arm64").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
