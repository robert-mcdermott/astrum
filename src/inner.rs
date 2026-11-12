//! A shell inside the pane's own: the one at the other end of `ssh`, in a
//! container, or after `sudo -s`.
//!
//! Astrum follows a pane's shell through marks the shell prints around its
//! prompts and commands (see `osc`). The pane's own shell prints them
//! because Astrum started it that way. A shell reached from it knows
//! nothing of Astrum, and the agent would have no way to run a command
//! there and see how it ended. So, when asked, Astrum types the same
//! arrangement into it: one line to find out what kind of shell it is, one
//! that has it read the rest without showing it, and the rest. Nothing is
//! written to the machine it runs on, and it lasts as long as that shell.
//!
//! The marks carry a secret so that output cannot forge them. The shell
//! inside is given one of its own, typed in with everything else: whoever
//! runs that machine could see it, and so could forge that shell's marks.
//! They never see the pane's own.

/// The shells this can be done in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Bash,
    Zsh,
    Fish,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Bash => "bash",
            Kind::Zsh => "zsh",
            Kind::Fish => "fish",
        }
    }
}

/// A shell the pane has reached and Astrum has been carried into.
#[derive(Clone, Debug, PartialEq)]
pub struct Inner {
    pub kind: Kind,
    pub user: String,
    pub host: String,
    /// What `uname -s` says there: Linux, Darwin, FreeBSD.
    pub os: String,
    pub cwd: Option<String>,
}

impl Inner {
    /// `user@host`, as far as each is known.
    pub fn label(&self) -> String {
        match (self.user.is_empty(), self.host.is_empty()) {
            (false, false) => format!("{}@{}", self.user, self.host),
            (true, false) => self.host.clone(),
            (false, true) => self.user.clone(),
            (true, true) => "another shell".to_string(),
        }
    }
}

/// The first line typed: it answers, in a mark, with the version of
/// whichever of the three shells is reading it. Every one of them (and
/// plain `sh`) can run it; a shell that is none of them answers with
/// nothing between the bars. The space in front keeps it out of the
/// history of shells set up to leave such lines out.
pub fn probe(secret: &str) -> String {
    format!(" printf '\\033]133;S;k={secret};%s|%s|%s\\a' \"$FISH_VERSION\" \"$ZSH_VERSION\" \"$BASH_VERSION\"")
}

/// Which shell answered the probe: what was between the bars.
pub fn kind_from(answer: &str) -> Option<Kind> {
    let mut versions = answer.split('|');
    let (fish, zsh, bash) = (versions.next()?, versions.next()?, versions.next()?);
    if !fish.is_empty() {
        Some(Kind::Fish)
    } else if !zsh.is_empty() {
        Some(Kind::Zsh)
    } else if !bash.is_empty() {
        Some(Kind::Bash)
    } else {
        None
    }
}

/// The second line typed: it has the shell take one more line from the
/// terminal, without that line being shown, and run it. It says when it
/// is ready for the line, so that nothing is sent while typing is still
/// being echoed. (The reading is done by `sh`, which every machine has and
/// which reads a line the same way everywhere.)
pub fn reader(kind: Kind, secret: &str) -> String {
    let read = format!("sh -c 'stty -echo; printf \"\\033]133;S;k={secret};ready\\a\" >/dev/tty; IFS= read -r x; stty echo; printf %s \"$x\"'");
    match kind {
        Kind::Bash | Kind::Zsh => format!(" eval \"$({read})\""),
        Kind::Fish => format!(" eval ({read})"),
    }
}

/// The third line, the one that is read and not shown: the marks
/// themselves, as one line of the shell's own language. It ends by saying
/// it is done and who and where it is.
///
/// (One line, and a short one: a terminal takes a line of limited length
/// from the keyboard at once, 1024 bytes on some systems.)
pub fn script(kind: Kind, secret: &str) -> String {
    let text = match kind {
        Kind::Bash => concat!(
            "_astrum_k=SECRET;_astrum_x=;_astrum_p=;",
            "_astrum_s(){ _astrum_r=$?;[ -n \"$_astrum_x\" ]&&printf '\\033]133;D;k=%s;%s\\a' \"$_astrum_k\" \"$_astrum_r\";_astrum_x=;return $_astrum_r;};",
            "_astrum_e(){ printf '\\033]7;file://%s%s\\a\\033]133;A;k=%s\\a' \"$HOSTNAME\" \"${PWD//%/%25}\" \"$_astrum_k\";_astrum_p=1;};",
            "_astrum_d(){ [ -n \"$COMP_LINE\" ]&&return;[ \"$BASH_COMMAND\" = _astrum_s ]&&{ _astrum_p=;return;};[ -z \"$_astrum_p\" ]&&return;_astrum_p=;_astrum_x=1;",
            "printf '\\033]133;C;k=%s;cmd=%s\\a' \"$_astrum_k\" \"$(HISTTIMEFORMAT= builtin history 1|sed 's/^ *[0-9]* *//')\";};",
            "trap _astrum_d DEBUG;PROMPT_COMMAND=\"_astrum_s${PROMPT_COMMAND:+;$PROMPT_COMMAND};_astrum_e\";",
            "printf '\\033]133;S;k=%s;ok;%s|%s|%s\\a' \"$_astrum_k\" \"${USER:-$(id -un)}\" \"${HOSTNAME:-$(hostname)}\" \"$(uname -s)\"",
        ),
        Kind::Zsh => concat!(
            "_astrum_k=SECRET;_astrum_x=;",
            "_astrum_precmd(){ local r=$?;[[ -n $_astrum_x ]]&&printf '\\033]133;D;k=%s;%s\\a' $_astrum_k $r;_astrum_x=;printf '\\033]7;file://%s%s\\a\\033]133;A;k=%s\\a' \"$HOST\" \"${PWD//\\%/%25}\" $_astrum_k;};",
            "_astrum_preexec(){ _astrum_x=1;printf '\\033]133;C;k=%s;cmd=%s\\a' $_astrum_k \"${1//[[:cntrl:]]/ }\";};",
            "autoload -Uz add-zsh-hook;add-zsh-hook precmd _astrum_precmd;add-zsh-hook preexec _astrum_preexec;",
            "printf '\\033]133;S;k=%s;ok;%s|%s|%s\\a' $_astrum_k \"${USER:-$(id -un)}\" \"$HOST\" \"$(uname -s)\"",
        ),
        Kind::Fish => concat!(
            "set -g _astrum_k SECRET;",
            "function _astrum_preexec --on-event fish_preexec;printf '\\e]133;C;k=%s;cmd=%s\\a' $_astrum_k (string replace -ra '[[:cntrl:]]' ' ' -- \"$argv[1]\");end;",
            "function _astrum_postexec --on-event fish_postexec;printf '\\e]133;D;k=%s;%s\\a' $_astrum_k $status;end;",
            "function _astrum_prompt --on-event fish_prompt;printf '\\e]7;file://%s%s\\a\\e]133;A;k=%s\\a' (hostname) (string replace -a '%' '%25' -- \"$PWD\") $_astrum_k;end;",
            "printf '\\e]133;S;k=%s;ok;%s|%s|%s\\a' $_astrum_k \"$USER\" (hostname) (uname -s)",
        ),
    };
    text.replace("SECRET", secret)
}

/// What the last line said of where it is: `user|host|system`.
pub fn arrived(kind: Kind, answer: &str) -> Inner {
    // (It is that shell's word for who and where it is, shown on the
    // status bar: nothing but plain characters, and not many.)
    let plain = |part: &str| part.trim().chars().filter(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_')).take(40).collect::<String>();
    let mut parts = answer.split('|').map(plain);
    let mut next = || parts.next().unwrap_or_default();
    let (user, host, os) = (next(), next(), next());
    // (The short name: what a prompt shows.)
    let host = host.split('.').next().unwrap_or_default().to_string();
    Inner { kind, user, host, os, cwd: None }
}

/// Whether a command line starts a program that usually ends in another
/// shell: worth offering to carry Astrum into. (Anything can be asked for
/// from the menu; this is only what is offered unasked.)
pub fn leads_to_a_shell(cmd: &str) -> bool {
    const PROGRAMS: [&str; 22] = [
        "ssh", "mosh", "autossh", "sshpass", "et", "telnet", "docker", "podman", "kubectl", "oc", "nsenter", "lxc", "incus", "machinectl", "toolbox", "distrobox", "vagrant", "sudo", "su", "doas",
        "nix-shell", "chroot",
    ];
    const SHELLS: [&str; 3] = ["bash", "zsh", "fish"];
    // Past any `VAR=value` in front (the value may be quoted, with spaces
    // in it), and by the program's own name.
    let mut rest = cmd.trim_start();
    loop {
        let name_ends = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(rest.len());
        if name_ends == 0 || !rest[name_ends..].starts_with('=') {
            break;
        }
        let mut value = rest[name_ends + 1..].chars();
        let mut quote = None;
        let mut taken = name_ends + 1;
        for c in value.by_ref() {
            match quote {
                Some(q) if c == q => quote = None,
                Some(_) => {}
                None if c == '\'' || c == '"' => quote = Some(c),
                None if c.is_whitespace() => break,
                None => {}
            }
            taken += c.len_utf8();
        }
        rest = rest[taken..].trim_start();
    }
    let Some(first) = rest.split_whitespace().next() else { return false };
    let name = first.rsplit('/').next().unwrap_or(first);
    PROGRAMS.contains(&name) || SHELLS.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_line_typed_is_one_line_a_terminal_will_take() {
        let secret = "0123456789abcdef0123456789abcdef";
        for kind in [Kind::Bash, Kind::Zsh, Kind::Fish] {
            for line in [probe(secret), reader(kind, secret), script(kind, secret)] {
                assert!(!line.contains('\n') && !line.contains('\r'), "{kind:?}: {line}");
                // Under what the smallest keyboard line of the systems it
                // may be typed into holds, with room to spare.
                assert!(line.len() < 950, "{kind:?}: {} bytes", line.len());
                assert!(line.contains(secret) && !line.contains("SECRET"), "{line}");
                assert!(line.is_ascii(), "{line}");
            }
        }
    }

    #[test]
    fn the_shell_that_answers_is_told_apart() {
        assert_eq!(kind_from("3.6.0||"), Some(Kind::Fish));
        assert_eq!(kind_from("|5.9|"), Some(Kind::Zsh));
        assert_eq!(kind_from("||5.2.15(1)-release"), Some(Kind::Bash));
        // Plain sh, dash, ksh: none of the three.
        assert_eq!(kind_from("||"), None);
        assert_eq!(kind_from("nonsense"), None);
        let there = arrived(Kind::Bash, "deploy|web-3.example.com|Linux");
        assert_eq!((there.label().as_str(), there.host.as_str(), there.os.as_str()), ("deploy@web-3", "web-3", "Linux"));
        assert_eq!(arrived(Kind::Fish, "||").label(), "another shell");
        assert_eq!(arrived(Kind::Zsh, "root").label(), "root");
        // What it says of itself is not trusted to be a name.
        let odd = arrived(Kind::Bash, &format!("me\x1b[31m; rm|{}|Lin ux", "h".repeat(90)));
        assert_eq!((odd.user.as_str(), odd.host.len(), odd.os.as_str()), ("me31mrm", 40, "Linux"));
    }

    #[test]
    fn programs_that_end_in_a_shell_are_known() {
        for cmd in ["ssh web-3", "/usr/bin/ssh -p 2222 me@host", "sudo -s", "docker exec -it db bash", "LANG=C ssh host", "kubectl exec -it pod -- sh", "bash", "su - deploy", "PS1='inner$ ' bash --norc", "A=1 B=\"two words\" ssh host"] {
            assert!(leads_to_a_shell(cmd), "{cmd}");
        }
        for cmd in ["vim notes.txt", "sshd -D", "make", "", "less ssh", "echo ssh", "X='ssh host' vim", "SSH=1"] {
            assert!(!leads_to_a_shell(cmd), "{cmd}");
        }
    }
}
