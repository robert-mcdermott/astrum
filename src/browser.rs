//! A browser that goes through a proxy, with a profile of its own.
//!
//! The browsers that can be told a proxy on their command line (Chrome
//! and its kin) are started with one and with a folder of their own for
//! everything they keep, so the user's own browsing, and the system's
//! settings, are as they were. Names are looked up through the proxy:
//! with SOCKS5 these browsers always do.
//!
//! It opens at a page of Astrum's own that says which machine the window
//! goes through, so that it is not taken for the user's other windows,
//! and does not begin by asking a network that may have no way out for
//! the browser's own start page.

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

/// The page such a browser opens at, when it is given no link: what the
/// window is, written into its own address (nothing is fetched for it).
fn start_page(name: &str) -> String {
    let name: String = name.chars().filter(|c| !c.is_control()).take(80).collect::<String>().replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let page = format!(
        "<!doctype html><meta charset=utf-8><title>Through {name} · Astrum</title>\
         <body style='font:16px/1.5 system-ui,sans-serif;max-width:34em;margin:12vh auto;padding:0 1.5em;color:#222;background:#f6f5f2'>\
         <h1 style='font-size:1.5em'>This window goes through {name}</h1>\
         <p>What you open here is reached from {name}, where its name is looked up: type an address of that network above, as you would there.</p>\
         <p>Your other browser windows are as they were, and so are this machine's network settings.</p>\
         <p style='color:#666'>It works for as long as the proxy is open in Astrum (⇄ proxy on the status bar).</p>"
    );
    let mut address = String::from("data:text/html;charset=utf-8,");
    for byte in page.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => address.push(byte as char),
            other => address.push_str(&format!("%{other:02X}")),
        }
    }
    address
}

/// What the browser is started with.
fn arguments(proxy: u16, profile: &str, name: &str, url: Option<&str>) -> Vec<String> {
    // (Only what is plainly a name goes into a path.)
    let profile: String = profile.chars().map(|c| if c.is_alphanumeric() || "._-".contains(c) { c } else { '_' }).take(80).collect();
    let folder = crate::paths::data_dir().join("browsers").join(if profile.is_empty() { "other".to_string() } else { profile });
    let mut args = vec![
        format!("--proxy-server=socks5://127.0.0.1:{proxy}"),
        // (Names are not looked up ahead of time, which would be asked
        // of this machine's own network. The stricter way of saying so,
        // `--host-resolver-rules`, was used at first: the browser opens
        // with a warning across its window that it is unsafe, which
        // reads as something having gone wrong.)
        "--dns-prefetch-disable".to_string(),
        format!("--user-data-dir={}", folder.display()),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
    ];
    // (A link, and nothing that a browser would take for an option;
    // without one, the page that says what this window is.)
    args.push(url.filter(|u| crate::pane::openable(u) && !u.starts_with('-')).map(str::to_string).unwrap_or_else(|| start_page(name)));
    args
}

/// Start it. (Nothing is said if it cannot be: whoever asked has looked
/// for one first.)
pub fn open(proxy: u16, profile: &str, name: &str, url: Option<&str>) {
    let Some(tool) = find() else { return };
    if let Ok(mut child) = Command::new(tool).args(arguments(proxy, profile, name, url)).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
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
        let args = arguments(1080, "deploy@web-3", "web-3", Some("https://wiki.internal/start"));
        assert_eq!(args[0], "--proxy-server=socks5://127.0.0.1:1080");
        assert!(args[2].starts_with("--user-data-dir=") && args[2].ends_with("/browsers/deploy_web-3"), "{}", args[2]);
        assert_eq!(args.last().unwrap(), "https://wiki.internal/start");
        // Nothing the browser warns about across its window when it opens.
        assert!(!args.iter().any(|a| a.contains("host-resolver-rules") || a.contains("host-rules")), "{args:?}");
        // A name that would climb out of the folder does not.
        assert!(arguments(1, "../../x", "x", None)[2].ends_with("/browsers/.._.._x"));
        assert!(arguments(1, "", "x", None)[2].ends_with("/browsers/other"));
    }

    #[test]
    fn with_no_link_it_opens_at_a_page_that_says_what_it_is() {
        // What is no link, or looks like an option, is not passed on:
        // the page of Astrum's own is opened in its place.
        for not in [None, Some("--remote-debugging-port=9222"), Some("file:///etc/passwd"), Some("javascript:alert(1)")] {
            let args = arguments(1, "h", "web-3", not);
            let page = args.last().unwrap();
            assert!(args.len() == 6 && page.starts_with("data:text/html;charset=utf-8,%3C%21doctype"), "{not:?}: {page}");
            assert!(page.contains("This%20window%20goes%20through%20web-3"), "{page}");
            // (One word to the browser, with nothing in it to take apart.)
            assert!(page["data:text/html;charset=utf-8,".len()..].bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~%".contains(&b)), "{page}");
        }
        // A name is written as text, whatever is in it.
        let page = start_page("<script>x</script>&");
        assert!(page.contains("%26lt%3Bscript%26gt%3B") && !page.contains("%3Cscript"), "{page}");
    }
}
