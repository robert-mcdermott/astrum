//! Files given to the other machine: what is done there.
//!
//! A file pasted or dropped into a tab on another machine is on this one,
//! and its path means nothing there. It is sent over the link in pieces,
//! each a request of its own that is answered, and kept there under a
//! name made of what is in it (its SHA-256), so that:
//!
//! - the same file sent twice is one file, found before anything is sent;
//! - pieces can come in any order, and nothing has to be remembered of a
//!   file that is half there (a link that drops loses the pieces on their
//!   way and no more);
//! - what is kept is known to be what was sent, checked before it is
//!   given its place.
//!
//! It is kept in a folder of the user's own that nobody else can read,
//! and cleared away when it has not been asked for in a while.

use crate::paths;
use aws_lc_rs::digest;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The most one piece holds, before it is written for the link.
pub const PIECE: usize = 64 * 1024;
/// Nothing larger than this is taken (or sent).
pub const MOST: u64 = 2 * 1024 * 1024 * 1024;
/// How long a file is kept after it was last asked for.
const KEPT_FOR: Duration = Duration::from_secs(7 * 24 * 3600);
/// How long the pieces of one that never came whole are kept.
const PART_FOR: Duration = Duration::from_secs(24 * 3600);

/// Where files that were sent are kept: `~/.cache/astrum/received`.
pub fn dir() -> PathBuf {
    paths::cache_dir().join("received")
}

/// A SHA-256 as it is written: nothing else is made part of a path.
fn is_sum(sum: &str) -> bool {
    sum.len() == 64 && sum.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The name a file is kept under: the last part of what it was called,
/// with nothing in it that a shell reads as more than a letter, so that
/// its path can be pasted as it stands.
pub fn kept_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let mut kept: String = last.chars().map(|c| if c.is_alphanumeric() || "._+@=,-".contains(c) { c } else { '_' }).take(120).collect();
    // (Not hidden, and not something a command takes for an option.)
    while kept.starts_with(['.', '-']) {
        kept.remove(0);
    }
    if kept.is_empty() { "file".into() } else { kept }
}

/// The name one sending of a file goes by, which the sender makes up:
/// two that send the same file at once, or one that stops and starts
/// again, do not write into each other's pieces.
fn is_put(put: &str) -> bool {
    (1..=32).contains(&put.len()) && put.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn part(dir: &Path, sum: &str, put: &str) -> PathBuf {
    dir.join(format!(".part-{sum}-{put}"))
}

fn place(dir: &Path, sum: &str, name: &str) -> PathBuf {
    dir.join(&sum[..10]).join(kept_name(name))
}

fn ready(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    // (It may have been there, made by something more generous.)
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

fn older(path: &Path, than: Duration) -> bool {
    let touched = std::fs::metadata(path).and_then(|m| m.modified()).unwrap_or_else(|_| SystemTime::now());
    SystemTime::now().duration_since(touched).is_ok_and(|age| age > than)
}

/// Clear away what has not been asked for in a while.
fn tidy(dir: &Path) {
    let Ok(all) = std::fs::read_dir(dir) else { return };
    for entry in all.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(".part-") {
            if older(&path, PART_FOR) {
                let _ = std::fs::remove_file(&path);
            }
        } else if path.is_dir() && older(&path, KEPT_FOR) {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

fn sha256_of(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut context = digest::Context::new(&digest::SHA256);
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        match file.read(&mut buf)? {
            0 => break,
            n => context.update(&buf[..n]),
        }
    }
    Ok(context.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect())
}

/// The SHA-256 of a file, as it is written. (On this machine, of what is
/// about to be sent; there, of what came.)
pub async fn sum_of(path: PathBuf) -> std::io::Result<String> {
    tokio::task::spawn_blocking(move || sha256_of(&path)).await.unwrap_or_else(|e| Err(std::io::Error::other(e)))
}

/// Is the file there already? Its path if it is. (And the moment to make
/// the folder, and to clear old things out of it.)
pub fn have(sum: &str, name: &str, size: u64) -> Result<Option<String>, String> {
    have_in(&dir(), sum, name, size)
}

fn have_in(dir: &Path, sum: &str, name: &str, size: u64) -> Result<Option<String>, String> {
    if !is_sum(sum) {
        return Err("That is not the mark of a file.".into());
    }
    if size > MOST {
        return Err("The file is larger than is taken this way.".into());
    }
    ready(dir).map_err(|e| format!("The folder for files could not be made: {e}"))?;
    tidy(dir);
    let place = place(dir, sum, name);
    match std::fs::metadata(&place) {
        Ok(meta) if meta.is_file() && meta.len() == size => {
            // (Asked for again: kept a while longer.)
            let now = std::fs::FileTimes::new().set_modified(SystemTime::now());
            for path in [place.as_path(), place.parent().unwrap_or(&place)] {
                if let Ok(file) = std::fs::File::open(path) {
                    let _ = file.set_times(now);
                }
            }
            Ok(Some(place.to_string_lossy().into_owned()))
        }
        _ => Ok(None),
    }
}

/// A piece of a file: `bytes`, which belong at `at` of its `size`.
pub fn piece(sum: &str, put: &str, size: u64, at: u64, bytes: &[u8]) -> Result<(), String> {
    piece_in(&dir(), sum, put, size, at, bytes)
}

fn piece_in(dir: &Path, sum: &str, put: &str, size: u64, at: u64, bytes: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::{FileExt, OpenOptionsExt};
    if !is_sum(sum) || !is_put(put) {
        return Err("That is not the mark of a file.".into());
    }
    if size > MOST || bytes.len() > 4 * PIECE || at.checked_add(bytes.len() as u64).is_none_or(|end| end > size) {
        return Err("That piece does not belong to the file it is said to.".into());
    }
    ready(dir).map_err(|e| format!("The folder for files could not be made: {e}"))?;
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(part(dir, sum, put)).map_err(|e| format!("The file could not be written: {e}"))?;
    file.write_all_at(bytes, at).map_err(|e| format!("The file could not be written: {e}"))
}

/// Every piece has been sent: if what came is what was sent, it is given
/// its place, and that is the answer.
pub fn keep(sum: &str, put: &str, name: &str, size: u64) -> Result<String, String> {
    keep_in(&dir(), sum, put, name, size)
}

fn keep_in(dir: &Path, sum: &str, put: &str, name: &str, size: u64) -> Result<String, String> {
    use std::os::unix::fs::DirBuilderExt;
    if !is_sum(sum) || !is_put(put) {
        return Err("That is not the mark of a file.".into());
    }
    let part = part(dir, sum, put);
    let whole = std::fs::metadata(&part).is_ok_and(|m| m.len() == size) && sha256_of(&part).is_ok_and(|found| found == sum);
    if !whole {
        let _ = std::fs::remove_file(&part);
        return Err("What arrived is not what was sent. Nothing was kept: send it again.".into());
    }
    let place = place(dir, sum, name);
    let moved = place.parent().map_or(Ok(()), |dir| std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)).and_then(|()| std::fs::rename(&part, &place));
    match moved {
        Ok(()) => Ok(place.to_string_lossy().into_owned()),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(format!("The file could not be kept: {e}"))
        }
    }
}

/// Sending was stopped: what came of it is not wanted.
pub fn forget(sum: &str, put: &str) {
    if is_sum(sum) && is_put(put) {
        let _ = std::fs::remove_file(part(&dir(), sum, put));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_kept_without_what_a_shell_would_read() {
        assert_eq!(kept_name("screenshot.png"), "screenshot.png");
        assert_eq!(kept_name("/Users/ada/Desktop/Screenshot 2026-11-17 at 9.03.41 PM.png"), "Screenshot_2026-11-17_at_9.03.41_PM.png");
        assert_eq!(kept_name("C:\\Users\\ada\\notes.txt"), "notes.txt");
        assert_eq!(kept_name("a;rm -rf $HOME`x`'\"|&.sh"), "a_rm_-rf__HOME_x_____.sh");
        assert_eq!(kept_name("résumé (final).pdf"), "résumé__final_.pdf");
        assert_eq!(kept_name("..hidden"), "hidden");
        assert_eq!(kept_name("-rf"), "rf");
        assert_eq!(kept_name("../../etc/passwd"), "passwd");
        assert_eq!(kept_name(""), "file");
        assert_eq!(kept_name("///"), "file");
        assert!(kept_name(&"x".repeat(500)).len() <= 120);
    }

    #[test]
    fn only_a_checksum_is_made_part_of_a_path() {
        assert!(is_sum(&"0f".repeat(32)));
        for not in ["", "0f", &"0F".repeat(32), &"zz".repeat(32), &format!("../{}", "0".repeat(61)), &"0".repeat(65)] {
            assert!(!is_sum(not), "{not}");
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("astrum-files-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn sum(bytes: &[u8]) -> String {
        digest::digest(&digest::SHA256, bytes).as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn a_file_comes_in_pieces_in_any_order_and_is_kept_once() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("pieces");
        let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let (sum, size) = (sum(&bytes), bytes.len() as u64);
        assert_eq!(have_in(&dir, &sum, "My Photo.png", size), Ok(None));
        // The last piece first.
        let pieces: Vec<(u64, &[u8])> = bytes.chunks(PIECE).enumerate().map(|(i, c)| ((i * PIECE) as u64, c)).collect();
        for (at, piece) in pieces.iter().rev() {
            piece_in(&dir, &sum, "a1", size, *at, piece).unwrap();
        }
        let path = keep_in(&dir, &sum, "a1", "My Photo.png", size).unwrap();
        assert!(path.ends_with(&format!("{}/My_Photo.png", &sum[..10])), "{path}");
        assert!(std::fs::read(&path).unwrap() == bytes);
        // Nobody else can read it, or see what is in the folder.
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        // Another sending of the same file, begun before this one was
        // kept and finished after: neither minds the other.
        piece_in(&dir, &sum, "b2", size, 0, &bytes[..PIECE]).unwrap();
        for (at, piece) in &pieces {
            piece_in(&dir, &sum, "b2", size, *at, piece).unwrap();
        }
        assert_eq!(keep_in(&dir, &sum, "b2", "My Photo.png", size), Ok(path.clone()));
        assert!(!part(&dir, &sum, "b2").exists());
        // Asked for again, it is there: nothing has to be sent.
        assert_eq!(have_in(&dir, &sum, "My Photo.png", size), Ok(Some(path.clone())));
        // (Under another name it is another file to the one who asks.)
        assert_eq!(have_in(&dir, &sum, "other.png", size), Ok(None));
        assert!(!part(&dir, &sum, "a1").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_did_not_come_whole_is_not_kept() {
        let dir = scratch("broken");
        let bytes = b"what was meant to be sent".to_vec();
        let (sum, size) = (sum(&bytes), bytes.len() as u64);
        // A piece is missing.
        piece_in(&dir, &sum, "a1", size, 0, &bytes[..10]).unwrap();
        assert!(keep_in(&dir, &sum, "a1", "note.txt", size).is_err());
        assert!(!part(&dir, &sum, "a1").exists());
        // It is all there, and not what it is said to be.
        piece_in(&dir, &sum, "a1", size, 0, &bytes.to_ascii_uppercase()).unwrap();
        assert!(keep_in(&dir, &sum, "a1", "note.txt", size).is_err());
        assert_eq!(have_in(&dir, &sum, "note.txt", size), Ok(None));
        // A piece that would go past the end, or wrap around, is refused.
        assert!(piece_in(&dir, &sum, "a1", size, size - 1, b"ab").is_err());
        assert!(piece_in(&dir, &sum, "a1", size, u64::MAX, b"a").is_err());
        assert!(piece_in(&dir, "../../escape", "a1", size, 0, b"a").is_err());
        assert!(piece_in(&dir, &sum, "../a", size, 0, b"a").is_err());
        assert!(piece_in(&dir, &sum, "", size, 0, b"a").is_err());
        assert!(have_in(&dir, &sum, "note.txt", MOST + 1).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_has_not_been_asked_for_in_a_while_is_cleared_away() {
        let dir = scratch("old");
        let bytes = b"kept a while".to_vec();
        let (sum, size) = (sum(&bytes), bytes.len() as u64);
        piece_in(&dir, &sum, "a1", size, 0, &bytes).unwrap();
        let path = PathBuf::from(keep_in(&dir, &sum, "a1", "old.txt", size).unwrap());
        let long_ago = std::fs::FileTimes::new().set_modified(SystemTime::now() - KEPT_FOR - Duration::from_secs(60));
        std::fs::File::open(path.parent().unwrap()).unwrap().set_times(long_ago).unwrap();
        let other = sum.replace(|c: char| c.is_ascii_digit(), "a");
        piece_in(&dir, &other, "a1", 3, 0, b"abc").unwrap();
        std::fs::File::options().write(true).open(part(&dir, &other, "a1")).unwrap().set_times(long_ago).unwrap();
        // The next file asked about is the moment things are cleared.
        assert_eq!(have_in(&dir, &"0".repeat(64), "new.txt", 1), Ok(None));
        assert!(!path.exists());
        assert!(!part(&dir, &other, "a1").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
