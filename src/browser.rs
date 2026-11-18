//! A browser that goes through a proxy, with a profile of its own.
//!
//! The browsers that can be told a proxy on their command line (Chrome
//! and its kin) are started with one and with a folder of their own for
//! everything they keep, so the user's own browsing, and the system's
//! settings, are as they were. Names are looked up through the proxy:
//! with SOCKS5 these browsers always do, and they are told as well to
//! ask nothing else.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// The browser to start: `ASTRUM_BROWSER_TOOL` if it names one (`none`:
/// none), or the first of those known that is on this machine.
pub fn find() -> Option<PathBuf> {
    match std::env::var("ASTRUM_BROWSER_TOOL").ok().filter(|t| !t.is_empty()).as_deref() {
        Some("none") => return None,
        Some(tool) => return Some(PathBuf::from(tool)),
        None => {}
    }
    if cfg!(target_os = "macos") {
        let apps = ["Google Chrome", "Chromium", "Microsoft Edge", "Brave Browser", "Vivaldi", "Arc"];
        let folders = [PathBuf::from("/Applications"), crate::paths::home().join("Applications")];
        return folders.iter().flat_map(|folder| apps.iter().map(move |app| folder.join(format!("{app}.app/Contents/MacOS/{app}")))).find(|p| p.is_file());
    }
    let names = ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "microsoft-edge", "brave-browser", "vivaldi"];
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).flat_map(|dir| names.iter().map(move |name| dir.join(name))).find(|p| p.is_file())
}

/// What the browser is started with.
fn arguments(proxy: u16, profile: &str, url: Option<&str>) -> Vec<String> {
    // (Only what is plainly a name goes into a path.)
    let profile: String = profile.chars().map(|c| if c.is_alphanumeric() || "._-".contains(c) { c } else { '_' }).take(80).collect();
    let folder = crate::paths::data_dir().join("browsers").join(if profile.is_empty() { "other".to_string() } else { profile });
    let mut args = vec![
        format!("--proxy-server=socks5://127.0.0.1:{proxy}"),
        // (No name is asked of this machine or its network: not even the
        // ones a browser looks up ahead of time.)
        "--host-resolver-rules=MAP * ~NOTFOUND , EXCLUDE 127.0.0.1".to_string(),
        format!("--user-data-dir={}", folder.display()),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
    ];
    // (A link, and nothing that a browser would take for an option.)
    args.extend(url.filter(|u| crate::pane::openable(u) && !u.starts_with('-')).map(str::to_string));
    args
}

/// Start it. (Nothing is said if it cannot be: whoever asked has looked
/// for one first.)
pub fn open(proxy: u16, profile: &str, url: Option<&str>) {
    let Some(tool) = find() else { return };
    if let Ok(mut child) = Command::new(tool).args(arguments(proxy, profile, url)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_browser_is_given_the_proxy_and_a_place_of_its_own() {
        let args = arguments(1080, "deploy@web-3", Some("https://wiki.internal/start"));
        assert_eq!(args[0], "--proxy-server=socks5://127.0.0.1:1080");
        assert!(args[1].starts_with("--host-resolver-rules=MAP * ~NOTFOUND"));
        assert!(args[2].starts_with("--user-data-dir=") && args[2].ends_with("/browsers/deploy_web-3"), "{}", args[2]);
        assert_eq!(args.last().unwrap(), "https://wiki.internal/start");
        // A name that would climb out of the folder does not; what is no
        // link, or looks like an option, is not passed on.
        assert!(arguments(1, "../../x", None)[2].ends_with("/browsers/.._.._x"));
        assert!(arguments(1, "", None)[2].ends_with("/browsers/other"));
        for not in ["--remote-debugging-port=9222", "file:///etc/passwd", "javascript:alert(1)"] {
            assert_eq!(arguments(1, "h", Some(not)).len(), 5, "{not}");
        }
    }
}
