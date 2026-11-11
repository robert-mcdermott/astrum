//! Filesystem locations used by Astrum.

use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::PathBuf;

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// Per-user directory that holds the session sockets. Kept short because
/// Unix socket paths are limited to ~104 bytes on macOS.
pub fn runtime_dir() -> anyhow::Result<PathBuf> {
    let uid = unsafe { libc::getuid() };
    let dir = match std::env::var_os("ASTRUM_RUNTIME_DIR") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(format!("/tmp/astrum-{uid}")),
    };
    if !dir.exists() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    }
    let meta = std::fs::metadata(&dir)?;
    if meta.uid() != uid {
        anyhow::bail!("{} is not owned by the current user", dir.display());
    }
    if meta.mode() & 0o077 != 0 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

pub fn socket_path(session: &str) -> anyhow::Result<PathBuf> {
    Ok(runtime_dir()?.join(format!("{session}.sock")))
}

pub fn log_path(session: &str) -> anyhow::Result<PathBuf> {
    Ok(runtime_dir()?.join(format!("{session}.log")))
}

pub fn config_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("ASTRUM_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(d) if !d.is_empty() => PathBuf::from(d).join("astrum"),
        _ => home().join(".config").join("astrum"),
    }
}

pub fn config_file() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("ASTRUM_DATA_DIR") {
        return PathBuf::from(d);
    }
    match std::env::var_os("XDG_DATA_HOME") {
        Some(d) if !d.is_empty() => PathBuf::from(d).join("astrum"),
        _ => home().join(".local").join("share").join("astrum"),
    }
}

/// Replace a leading $HOME with `~` for display.
pub fn tildify(path: &str) -> String {
    let h = home();
    let h = h.to_string_lossy();
    if h.len() > 1 {
        if path == h {
            return "~".into();
        }
        if let Some(rest) = path.strip_prefix(&*h) {
            if rest.starts_with('/') {
                return format!("~{rest}");
            }
        }
    }
    path.to_string()
}

/// Expand a leading `~` and resolve relative paths against `cwd`.
pub fn resolve(path: &str, cwd: &str) -> PathBuf {
    let p = if path == "~" {
        home()
    } else if let Some(rest) = path.strip_prefix("~/") {
        home().join(rest)
    } else {
        PathBuf::from(path)
    };
    if p.is_absolute() { p } else { PathBuf::from(cwd).join(p) }
}
