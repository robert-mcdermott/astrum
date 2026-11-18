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

/// A browser on this machine that can be started with a proxy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Browser {
    /// As it is called in a menu: `Brave`, `Chrome`.
    pub name: String,
    pub path: PathBuf,
}

/// The browsers that can be started with a proxy: the one
/// `ASTRUM_BROWSER_TOOL` names if it names one (`none`: none), or those
/// known that are on this machine, each kind once.
///
/// (Whether a browser's settings are laid down by an organization, which
/// can include the proxy it uses whatever it is started with, is not
/// guessed at. It was, from the files such settings are kept in, and
/// was wrong for a browser managed some other way: a warning on one and
/// none on another said more than was known. Every browser is offered,
/// and the documentation says what a managed one may do.)
pub fn found() -> Vec<Browser> {
    match std::env::var("ASTRUM_BROWSER_TOOL").ok().filter(|t| !t.is_empty()).as_deref() {
        Some("none") => return Vec::new(),
        Some(tool) => return vec![Browser { name: "a browser".into(), path: PathBuf::from(tool) }],
        None => {}
    }
    // (The program, and what it is called.)
    let candidates: Vec<(PathBuf, &str)> = if cfg!(target_os = "macos") {
        let apps = [("Google Chrome", "Chrome"), ("Chromium", "Chromium"), ("Brave Browser", "Brave"), ("Microsoft Edge", "Edge"), ("Vivaldi", "Vivaldi")];
        let folders = [PathBuf::from("/Applications"), crate::paths::home().join("Applications")];
        apps.iter().flat_map(|(app, name)| folders.iter().map(move |folder| (folder.join(format!("{app}.app/Contents/MacOS/{app}")), *name))).collect()
    } else {
        let programs = [("google-chrome", "Chrome"), ("google-chrome-stable", "Chrome"), ("chromium", "Chromium"), ("chromium-browser", "Chromium"), ("brave-browser", "Brave"), ("microsoft-edge", "Edge"), ("vivaldi", "Vivaldi")];
        let dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|path| std::env::split_paths(&path).collect()).unwrap_or_default();
        programs.iter().flat_map(|(program, name)| dirs.iter().map(move |dir| (dir.join(program), *name))).collect()
    };
    let mut all: Vec<Browser> = Vec::new();
    for (path, name) in candidates {
        if path.is_file() && all.iter().all(|b| b.name != name) {
            all.push(Browser { name: name.into(), path });
        }
    }
    all
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
    // (And not one made of dots, which would be a folder above.)
    let profile: String = profile.chars().map(|c| if c.is_alphanumeric() || "._-".contains(c) { c } else { '_' }).take(80).collect();
    let profile = profile.trim_matches('.');
    let folder = crate::paths::data_dir().join("browsers").join(if profile.is_empty() { "other" } else { profile });
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

/// Start the one called `which`, as `found` finds it here: what is run
/// is this machine's own browser, whatever was asked. (Nothing is said
/// if there is none: whoever asked has found it first.)
pub fn open(which: &str, proxy: u16, profile: &str, name: &str, url: Option<&str>) {
    let Some(tool) = found().into_iter().find(|b| b.name == which).map(|b| b.path) else { return };
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
        assert!(arguments(1, "../../x", "x", None)[2].ends_with("/browsers/_.._x"));
        assert!(arguments(1, "..", "x", None)[2].ends_with("/browsers/other"));
        assert!(arguments(1, ".", "x", None)[2].ends_with("/browsers/other"));
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
