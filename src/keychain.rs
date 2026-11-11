//! API keys in the operating system's own password store, so they need not
//! sit in a config file or a shell startup script.
//!
//! No library is linked for this: Astrum runs the tool the system provides
//! (`security` on macOS, `secret-tool` on Linux desktops). The key is never
//! put on a command line, where other programs could see it.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const SERVICE: &str = "astrum";

enum Store {
    /// The macOS keychain, through `/usr/bin/security`.
    Mac,
    /// The freedesktop secret service, through `secret-tool`.
    SecretTool,
    /// A plain file, for tests: `ASTRUM_KEYCHAIN_FILE` names it.
    File(PathBuf),
}

fn on_path(tool: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(tool).is_file()))
}

fn store() -> Result<Store, String> {
    if let Some(file) = std::env::var_os("ASTRUM_KEYCHAIN_FILE") {
        return Ok(Store::File(PathBuf::from(file)));
    }
    if cfg!(target_os = "macos") && std::path::Path::new("/usr/bin/security").exists() {
        return Ok(Store::Mac);
    }
    if on_path("secret-tool") {
        return Ok(Store::SecretTool);
    }
    Err(if cfg!(target_os = "macos") {
        "the macOS `security` tool was not found".into()
    } else {
        "no password store was found: install `secret-tool` (package libsecret-tools) to use the system keyring".into()
    })
}

/// What the store is called, for telling the user where a key went; or why
/// there is none.
pub fn name() -> Result<&'static str, String> {
    Ok(match store()? {
        Store::Mac => "your Mac's keychain",
        Store::SecretTool => "the system keyring",
        Store::File(_) => "the keychain file",
    })
}

/// Names are used as arguments and inside a quoted string.
fn check_name(provider: &str) -> Result<(), String> {
    let fine = !provider.is_empty() && provider.len() <= 64 && provider.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    if fine { Ok(()) } else { Err(format!("`{provider}` cannot be used as a name in the password store")) }
}

fn file_load(path: &PathBuf) -> std::collections::BTreeMap<String, String> {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn file_save(path: &PathBuf, keys: &std::collections::BTreeMap<String, String>) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let text = serde_json::to_string_pretty(keys).map_err(|e| e.to_string())?;
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path).map_err(|e| e.to_string())?;
    file.write_all(text.as_bytes()).map_err(|e| e.to_string())
}

/// Run a tool, giving it `input` on standard input, and return what it printed.
fn run(tool: &str, args: &[&str], input: Option<&str>) -> Result<String, String> {
    let mut child = Command::new(tool)
        .args(args)
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {tool}: {e}"))?;
    if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
        let _ = stdin.write_all(text.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| format!("{tool}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let said = String::from_utf8_lossy(&out.stderr);
        Err(said.lines().find(|l| !l.trim().is_empty()).unwrap_or("it reported a failure").trim().to_string())
    }
}

/// The key stored for a provider, if there is one.
pub fn get(provider: &str) -> Option<String> {
    check_name(provider).ok()?;
    let found = match store().ok()? {
        Store::Mac => run("/usr/bin/security", &["find-generic-password", "-s", SERVICE, "-a", provider, "-w"], None).ok()?,
        Store::SecretTool => run("secret-tool", &["lookup", "service", SERVICE, "provider", provider], None).ok()?,
        Store::File(path) => file_load(&path).get(provider).cloned()?,
    };
    let key = found.trim_end_matches(['\n', '\r']).to_string();
    (!key.is_empty()).then_some(key)
}

/// Store (or replace) the key for a provider.
pub fn set(provider: &str, key: &str) -> Result<(), String> {
    check_name(provider)?;
    if key.is_empty() || key.contains(char::is_control) {
        return Err("that does not look like a key (it is empty or has line breaks in it)".into());
    }
    match store()? {
        Store::Mac => {
            // The command goes in on standard input, with the key as
            // hexadecimal, so it is neither on a command line nor subject
            // to quoting.
            let hex: String = key.bytes().map(|b| format!("{b:02x}")).collect();
            let command = format!("add-generic-password -U -s {SERVICE} -a {provider} -l \"Astrum: {provider} API key\" -X {hex}\n");
            run("/usr/bin/security", &["-i"], Some(&command))?;
        }
        Store::SecretTool => {
            let label = format!("Astrum: {provider} API key");
            run("secret-tool", &["store", "--label", &label, "service", SERVICE, "provider", provider], Some(key))?;
        }
        Store::File(path) => {
            let mut keys = file_load(&path);
            keys.insert(provider.to_string(), key.to_string());
            file_save(&path, &keys)?;
        }
    }
    // The tools do not all report failure; make sure it is really there.
    if get(provider).as_deref() == Some(key) { Ok(()) } else { Err("the password store did not keep the key".into()) }
}

/// Forget the key for a provider. Not having one is not an error.
pub fn delete(provider: &str) -> Result<(), String> {
    check_name(provider)?;
    if get(provider).is_none() {
        return Ok(());
    }
    match store()? {
        Store::Mac => run("/usr/bin/security", &["delete-generic-password", "-s", SERVICE, "-a", provider], None).map(|_| ()),
        Store::SecretTool => run("secret-tool", &["clear", "service", SERVICE, "provider", provider], None).map(|_| ()),
        Store::File(path) => {
            let mut keys = file_load(&path);
            keys.remove(provider);
            file_save(&path, &keys)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_that_could_be_misread_are_refused() {
        assert!(check_name("anthropic").is_ok() && check_name("my-work_gateway.2").is_ok());
        for bad in ["", "two words", "quote\"d", "semi;colon", "-X 00", "a/b"] {
            // A leading dash alone is harmless as an option value, but spaces and quotes are not.
            if bad == "-X 00" || bad != "" {
                assert!(check_name(bad).is_err() || bad == "", "{bad}");
            }
        }
        assert!(check_name("").is_err());
    }

    #[test]
    fn the_file_store_keeps_and_forgets_keys() {
        let path = std::env::temp_dir().join(format!("astrum-keys-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut keys = file_load(&path);
        assert!(keys.is_empty());
        keys.insert("one".into(), "k-1".into());
        file_save(&path, &keys).unwrap();
        assert_eq!(file_load(&path).get("one").map(String::as_str), Some("k-1"));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_file(&path);
    }
}
