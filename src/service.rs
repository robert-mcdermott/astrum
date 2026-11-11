//! An optional login service for a session.
//!
//! Normally a session's server is started by the first `astrum` that asks
//! for it. Installed as a service, it is started by the system's own
//! service manager when you log in, and started again if it stops:
//! launchd on macOS, the systemd user manager on Linux. On Linux, with
//! lingering turned on, that also carries the session through a logout.

use crate::paths;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Environment worth handing to a server that no shell starts: where its
/// files are, which shell to run, and the language settings shells expect.
const KEEP_ENV: &[&str] = &[
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "ASTRUM_CONFIG_DIR",
    "ASTRUM_DATA_DIR",
    "ASTRUM_RUNTIME_DIR",
    "ASTRUM_KEYCHAIN_FILE",
];

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// The service's name, as its manager knows it.
pub fn label(session: &str) -> String {
    if cfg!(target_os = "macos") { format!("io.github.robert-mcdermott.astrum.{session}") } else { format!("astrum-{session}.service") }
}

/// Where the service's definition lives.
pub fn unit_path(session: &str) -> PathBuf {
    if cfg!(target_os = "macos") {
        home().join("Library/LaunchAgents").join(format!("{}.plist", label(session)))
    } else {
        let config = std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(".config"));
        config.join("systemd/user").join(label(session))
    }
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// A value inside double quotes in a systemd unit file.
fn quoted(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%").replace('$', "$$")
}

/// The definition of the service, in the manager's own format.
pub fn launchd_unit(exe: &Path, session: &str, log: &Path, env: &[(String, String)]) -> String {
    let mut vars = String::new();
    for (name, value) in env {
        vars.push_str(&format!("        <key>{}</key>\n        <string>{}</string>\n", xml(name), xml(value)));
    }
    let (exe, log, home) = (xml(&exe.to_string_lossy()), xml(&log.to_string_lossy()), xml(&home().to_string_lossy()));
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>--session</string>
        <string>{session}</string>
        <string>__server</string>
        <string>--service</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ThrottleInterval</key>
    <integer>2</integer>
    <key>ProcessType</key>
    <string>Interactive</string>
    <key>WorkingDirectory</key>
    <string>{home}</string>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
    <key>EnvironmentVariables</key>
    <dict>
{vars}    </dict>
</dict>
</plist>
"#,
        label = xml(&label(session)),
    )
}

pub fn systemd_unit(exe: &Path, session: &str, env: &[(String, String)]) -> String {
    let mut vars = String::new();
    for (name, value) in env {
        vars.push_str(&format!("Environment=\"{}={}\"\n", quoted(name), quoted(value)));
    }
    format!(
        "[Unit]\nDescription=Astrum session {session}\n\n[Service]\nExecStart=\"{exe}\" --session {session} __server --service\nRestart=always\nRestartSec=1\n{vars}\n[Install]\nWantedBy=default.target\n",
        exe = quoted(&exe.to_string_lossy()),
    )
}

fn unit_text(session: &str) -> Result<String> {
    let exe = std::env::current_exe().context("locating the astrum executable")?;
    let mut env: Vec<(String, String)> = KEEP_ENV.iter().filter_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()).map(|v| (name.to_string(), v))).collect();
    // Shells started with no language setting at all garble anything beyond ASCII.
    if !env.iter().any(|(name, _)| name == "LANG" || name.starts_with("LC_")) {
        env.push(("LANG".into(), if cfg!(target_os = "macos") { "en_US.UTF-8" } else { "C.UTF-8" }.into()));
    }
    Ok(if cfg!(target_os = "macos") { launchd_unit(&exe, session, &paths::log_path(session)?, &env) } else { systemd_unit(&exe, session, &env) })
}

/// Run a command of the service manager; its output, or why it failed.
fn run(program: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(program).args(args).output().with_context(|| format!("running {program}"))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if !out.status.success() {
        bail!("`{program} {}` failed: {}", args.join(" "), text.trim());
    }
    Ok(text)
}

fn gui_domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

/// Lingering keeps a user's services running while they are logged out.
fn lingering() -> Option<bool> {
    let user = std::env::var("USER").ok()?;
    let out = run("loginctl", &["show-user", &user, "--property=Linger"]).ok()?;
    Some(out.trim().ends_with("=yes"))
}

/// What installing would write, and where, without doing it.
pub fn preview(session: &str) -> Result<String> {
    Ok(format!("# {}\n{}", unit_path(session).display(), unit_text(session)?))
}

pub fn install(session: &str) -> Result<String> {
    // Is the session already running, started the ordinary way?
    let already = crate::client::info(session).is_some();
    let path = unit_path(session);
    let text = unit_text(session)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    let name = label(session);
    let mut report = format!("Installed {}\n", path.display());
    if cfg!(target_os = "macos") {
        let domain = gui_domain();
        // Replace one that is already loaded.
        let _ = run("launchctl", &["bootout", &format!("{domain}/{name}")]);
        run("launchctl", &["bootstrap", &domain, &path.to_string_lossy()])?;
        report.push_str(&format!("Session `{session}` now starts when you log in, and starts again if it stops.\n"));
        report.push_str("Logging out of macOS still ends it; its tabs come back at the next login.\n");
    } else {
        run("systemctl", &["--user", "daemon-reload"])?;
        run("systemctl", &["--user", "enable", "--now", &name])?;
        report.push_str(&format!("Session `{session}` now starts when you log in, and starts again if it stops.\n"));
        match lingering() {
            Some(true) => report.push_str("Lingering is on: the session stays up while you are logged out.\n"),
            _ => report.push_str("To keep it running while you are logged out as well, turn on lingering:\n    loginctl enable-linger\n"),
        }
    }
    if already {
        report.push_str(&format!(
            "Session `{session}` is running already and stays as it is. The service takes over the next time it is stopped (`astrum kill`), and its tabs come back.\n"
        ));
    }
    Ok(report.trim_end().to_string())
}

pub fn uninstall(session: &str) -> Result<String> {
    let path = unit_path(session);
    if !path.exists() {
        return Ok(format!("No service is installed for session `{session}`."));
    }
    let name = label(session);
    if cfg!(target_os = "macos") {
        let _ = run("launchctl", &["bootout", &format!("{}/{name}", gui_domain())]);
    } else {
        let _ = run("systemctl", &["--user", "disable", "--now", &name]);
    }
    std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    if !cfg!(target_os = "macos") {
        let _ = run("systemctl", &["--user", "daemon-reload"]);
    }
    Ok(format!("Removed {}\nSession `{session}` was stopped; its tabs come back the next time it is started.", path.display()))
}

pub fn status(session: &str) -> Result<String> {
    let path = unit_path(session);
    if !path.exists() {
        return Ok(format!("No service is installed for session `{session}`. `astrum service install` sets one up."));
    }
    let name = label(session);
    let running = if cfg!(target_os = "macos") {
        run("launchctl", &["print", &format!("{}/{name}", gui_domain())]).map(|out| out.lines().any(|l| l.trim() == "state = running")).unwrap_or(false)
    } else {
        run("systemctl", &["--user", "is-active", &name]).map(|out| out.trim() == "active").unwrap_or(false)
    };
    let mut report = format!("Service for session `{session}`: {}\n{}", if running { "running" } else { "installed, not running" }, path.display());
    if !cfg!(target_os = "macos") && lingering() != Some(true) {
        report.push_str("\nLingering is off: the session stops when you log out. `loginctl enable-linger` keeps it running.");
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Vec<(String, String)> {
        vec![("SHELL".into(), "/bin/zsh".into()), ("LANG".into(), "en_US.UTF-8".into())]
    }

    #[test]
    fn the_launchd_definition_runs_the_server_and_keeps_it_up() {
        let text = launchd_unit(Path::new("/Users/me/My Apps/astrum&co/astrum"), "work", Path::new("/tmp/astrum-501/work.log"), &env());
        assert!(text.starts_with("<?xml"));
        // The program and its arguments, one string each, with XML's own characters escaped.
        assert!(text.contains("<string>/Users/me/My Apps/astrum&amp;co/astrum</string>\n        <string>--session</string>\n        <string>work</string>\n        <string>__server</string>\n        <string>--service</string>"), "{text}");
        assert!(text.contains("<key>RunAtLoad</key>\n    <true/>") && text.contains("<key>KeepAlive</key>\n    <true/>"));
        assert!(text.contains("<key>SHELL</key>\n        <string>/bin/zsh</string>") && text.contains("<key>LANG</key>"));
        assert!(text.contains("<string>/tmp/astrum-501/work.log</string>"));
        // Every tag that opens closes.
        for tag in ["dict", "array", "plist"] {
            assert_eq!(text.matches(&format!("<{tag}")).count(), text.matches(&format!("</{tag}>")).count(), "{tag}");
        }
    }

    #[test]
    fn the_systemd_definition_runs_the_server_and_keeps_it_up() {
        let text = systemd_unit(Path::new("/home/me/50% off/astrum"), "default", &env());
        assert!(text.contains("ExecStart=\"/home/me/50%% off/astrum\" --session default __server --service\n"), "{text}");
        assert!(text.contains("Restart=always\n") && text.contains("WantedBy=default.target\n"));
        assert!(text.contains("Environment=\"SHELL=/bin/zsh\"\n") && text.contains("Environment=\"LANG=en_US.UTF-8\"\n"));
        assert!(text.starts_with("[Unit]\n") && text.contains("\n[Service]\n") && text.contains("\n[Install]\n"));
    }
}
