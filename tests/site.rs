//! The website builds against this Astrum: every key, setting and theme it
//! lists comes from the binary, so a setting with no description in the
//! starter file, or a page that links to nowhere, fails here and not at the
//! next release. Skipped where there is no Node to run the generator.

use std::process::Command;

#[test]
fn the_website_builds_and_checks() {
    let Ok(node) = Command::new("node").arg("--version").output() else {
        eprintln!("no node: the website is not built");
        return;
    };
    let version = String::from_utf8_lossy(&node.stdout);
    let major: u32 = version.trim().trim_start_matches('v').split('.').next().and_then(|m| m.parse().ok()).unwrap_or(0);
    if major < 18 {
        eprintln!("node {version} is too old for the website's generator (18 or newer)");
        return;
    }
    let out = std::env::temp_dir().join(format!("astrum-site-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    let built = Command::new("node")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/site/build.mjs"))
        .args(["--out".as_ref(), out.as_os_str(), "--astrum".as_ref(), env!("CARGO_BIN_EXE_astrum").as_ref()])
        .output()
        .unwrap();
    let said = format!("{}{}", String::from_utf8_lossy(&built.stdout), String::from_utf8_lossy(&built.stderr));
    assert!(built.status.success(), "the website does not build:\n{said}");
    for page in ["index.html", "404.html", "docs/index.html", "docs/settings.html", "docs/mouse-and-keys.html", "search.json"] {
        assert!(out.join(page).exists(), "no {page} in the built site:\n{said}");
    }
    // What the binary says is on the pages.
    let settings = std::fs::read_to_string(out.join("docs/settings.html")).unwrap();
    assert!(settings.contains("<code>keep_conversations_days</code>"), "the settings page lacks a setting");
    let keys = std::fs::read_to_string(out.join("docs/mouse-and-keys.html")).unwrap();
    assert!(keys.contains("<code>split_right</code>"), "the keys page lacks an action");
    let _ = std::fs::remove_dir_all(&out);
}
