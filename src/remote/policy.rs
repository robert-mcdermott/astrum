//! What whoever runs a machine allows Astrum to carry through it.
//!
//! A port of the machine itself, used from another, harms nobody who
//! could not already sign in. Reaching *other* machines through it is
//! different: it carries traffic into whatever network the machine is in,
//! and does so in Astrum's own line, where the `ssh` server's own
//! settings (`AllowTcpForwarding`) have no say. So the machine's
//! administrator is given one: a file only root can write,
//! `/etc/astrum/policy.toml`, with one line in it.
//!
//! ```toml
//! forward = "local"   # or "any" (as with no file at all), or "none"
//! ```
//!
//! It is read by the program keeping the shells, which is the one that
//! makes the connections, each time one is asked for. It is a rule kept
//! by a program the user runs, for users who mean to keep the rules; it
//! is not what stops one who does not (that is a firewall's work).

use std::path::{Path, PathBuf};

/// Where the rule is written.
pub const FILE: &str = "/etc/astrum/policy.toml";

/// What may be forwarded through this machine.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Forward {
    /// Ports of any machine this one can reach.
    #[default]
    Any,
    /// Ports of this machine itself only.
    Local,
    /// Nothing.
    Nothing,
}

impl Forward {
    pub fn named(name: &str) -> Option<Forward> {
        match name {
            "any" => Some(Forward::Any),
            "local" => Some(Forward::Local),
            "none" => Some(Forward::Nothing),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Forward::Any => "any",
            Forward::Local => "local",
            Forward::Nothing => "none",
        }
    }

    /// As another machine said it of itself. (A word this Astrum does
    /// not know is taken for no rule: the other machine keeps its own.)
    pub fn said(name: &str) -> Forward {
        Forward::named(name).unwrap_or_default()
    }
}

/// The rule in a file. No file is no rule. One that is not root's, or
/// that others than root can write, is not the administrator's word and
/// says nothing (whoever could write it could as well remove it). One
/// that is the administrator's and cannot be read or understood allows
/// nothing: a slip of the hand there must not open what was meant shut.
fn read_from(path: &Path, must_be_roots: bool) -> Forward {
    use std::os::unix::fs::MetadataExt;
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Forward::Any,
        Err(_) => return Forward::Nothing,
    };
    if must_be_roots && (meta.uid() != 0 || meta.mode() & 0o022 != 0) {
        return Forward::Any;
    }
    let Ok(table) = std::fs::read_to_string(path).map_err(|_| ()).and_then(|text| text.parse::<toml::Table>().map_err(|_| ())) else { return Forward::Nothing };
    match table.get("forward") {
        None => Forward::Any,
        Some(value) => value.as_str().and_then(Forward::named).unwrap_or(Forward::Nothing),
    }
}

/// What this machine's administrator allows, as it stands now.
pub fn forward() -> Forward {
    // (A stand-in for another machine, in the tests, has no `/etc` of its own.)
    if cfg!(debug_assertions) {
        if let Some(file) = std::env::var_os("ASTRUM_POLICY_FILE") {
            return read_from(&PathBuf::from(file), false);
        }
    }
    read_from(Path::new(FILE), true)
}

/// Why something was refused, as it is said on the machine that asked.
pub fn refusal(allowed: Forward, beyond: bool) -> Option<String> {
    match (allowed, beyond) {
        (Forward::Nothing, _) => Some(format!("this machine does not allow ports to be forwarded through Astrum (its administrator has said so in {FILE})")),
        (Forward::Local, true) => Some(format!("this machine allows only its own ports to be forwarded, not those of other machines (its administrator has said so in {FILE})")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("astrum-policy-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_rule_is_read_and_a_slip_allows_nothing() {
        let dir = scratch("read");
        let file = dir.join("policy.toml");
        assert_eq!(read_from(&file, false), Forward::Any);
        for (text, rule) in [
            ("forward = \"any\"\n", Forward::Any),
            ("forward = \"local\"\n", Forward::Local),
            ("# nothing through here\nforward = \"none\"\n", Forward::Nothing),
            // Says nothing of forwarding: no rule.
            ("", Forward::Any),
            ("other = 1\n", Forward::Any),
            // A slip of the hand: shut, not open.
            ("forward = \"locl\"\n", Forward::Nothing),
            ("forward = true\n", Forward::Nothing),
            ("forward = local\n", Forward::Nothing),
            ("forward \"none\"", Forward::Nothing),
        ] {
            std::fs::write(&file, text).unwrap();
            assert_eq!(read_from(&file, false), rule, "{text:?}");
        }
        // A file that is not root's is not the administrator's word.
        std::fs::write(&file, "forward = \"none\"\n").unwrap();
        assert_eq!(read_from(&file, true), Forward::Any);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_is_refused_and_what_is_said() {
        assert_eq!(refusal(Forward::Any, false), None);
        assert_eq!(refusal(Forward::Any, true), None);
        assert_eq!(refusal(Forward::Local, false), None);
        assert!(refusal(Forward::Local, true).unwrap().contains("only its own ports"));
        assert!(refusal(Forward::Nothing, false).unwrap().contains(FILE));
        assert!(refusal(Forward::Nothing, true).is_some());
        for rule in [Forward::Any, Forward::Local, Forward::Nothing] {
            assert_eq!(Forward::said(rule.name()), rule);
        }
        assert_eq!(Forward::said(""), Forward::Any);
        assert_eq!(Forward::said("something newer"), Forward::Any);
    }
}
