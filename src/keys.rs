//! What Astrum's own keys do, and which keys those are.
//!
//! Everything reachable from the keyboard is an `Action`. A `Keymap` says
//! which keys run which action: either after the prefix key (`c` for a new
//! tab) or at once, with no prefix (`alt+t`). It starts from the defaults
//! and takes whatever the `[keys]` table of the config file changes.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    NewTab,
    NextTab,
    PrevTab,
    /// Go to a tab by its place, counting from zero.
    Tab(u8),
    SplitRight,
    SplitDown,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    Zoom,
    Close,
    MoveTabLeft,
    MoveTabRight,
    Detach,
    DetachAll,
    AgentPanel,
    Rename,
    Explain,
    Fix,
    Watch,
    Model,
    Theme,
    Help,
    Find,
    CopyMode,
    PageUp,
    PageDown,
    Menu,
    Settings,
}

/// The actions, by the name each has in the config file, with a line
/// saying what it does.
pub const ACTIONS: &[(&str, Action, &str)] = &[
    ("new_tab", Action::NewTab, "open a new tab"),
    ("next_tab", Action::NextTab, "go to the next tab"),
    ("prev_tab", Action::PrevTab, "go to the previous tab"),
    ("tab_1", Action::Tab(0), "go to tab 1"),
    ("tab_2", Action::Tab(1), "go to tab 2"),
    ("tab_3", Action::Tab(2), "go to tab 3"),
    ("tab_4", Action::Tab(3), "go to tab 4"),
    ("tab_5", Action::Tab(4), "go to tab 5"),
    ("tab_6", Action::Tab(5), "go to tab 6"),
    ("tab_7", Action::Tab(6), "go to tab 7"),
    ("tab_8", Action::Tab(7), "go to tab 8"),
    ("tab_9", Action::Tab(8), "go to tab 9"),
    ("split_right", Action::SplitRight, "split the pane, side by side"),
    ("split_down", Action::SplitDown, "split the pane, one above the other"),
    ("focus_left", Action::FocusLeft, "move to the pane on the left (or the previous tab)"),
    ("focus_right", Action::FocusRight, "move to the pane on the right (or the next tab)"),
    ("focus_up", Action::FocusUp, "move to the pane above"),
    ("focus_down", Action::FocusDown, "move to the pane below"),
    ("zoom", Action::Zoom, "let the pane fill the tab, and back"),
    ("close", Action::Close, "close the pane, or the tab when it is the last"),
    ("move_tab_left", Action::MoveTabLeft, "move the tab one place left"),
    ("move_tab_right", Action::MoveTabRight, "move the tab one place right"),
    ("detach", Action::Detach, "leave, keeping everything running"),
    ("detach_all", Action::DetachAll, "detach every terminal from the session"),
    ("agent_panel", Action::AgentPanel, "show or hide the agent panel"),
    ("rename", Action::Rename, "rename the tab"),
    ("explain", Action::Explain, "have the agent explain the last command"),
    ("fix", Action::Fix, "have the agent fix the last command"),
    ("watch", Action::Watch, "have the agent watch the running command"),
    ("model", Action::Model, "choose a model"),
    ("theme", Action::Theme, "choose a theme"),
    ("help", Action::Help, "show the keys"),
    ("find", Action::Find, "search the scrollback"),
    ("copy_mode", Action::CopyMode, "select with the keyboard"),
    ("page_up", Action::PageUp, "scroll back a page"),
    ("page_down", Action::PageDown, "scroll forward a page"),
    ("menu", Action::Menu, "open the menu of all of these"),
    ("settings", Action::Settings, "open the settings"),
];

impl Action {
    pub fn name(self) -> &'static str {
        ACTIONS.iter().find(|(_, a, _)| *a == self).map(|(n, _, _)| *n).unwrap_or("")
    }

    pub fn named(name: &str) -> Option<Action> {
        let wanted = name.trim().to_ascii_lowercase().replace('-', "_");
        ACTIONS.iter().find(|(n, _, _)| *n == wanted).map(|(_, a, _)| *a)
    }
}

/// One key, with Ctrl and Alt if held. Letters are told apart by case:
/// `D` is Shift and `d`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Key {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
}

impl Key {
    pub fn parse(spec: &str) -> Option<Key> {
        let spec = spec.trim();
        // The key is what follows the last `+`. The plus key itself is
        // written `+`, or `ctrl++` with something held.
        let (mods, name) = if spec == "+" {
            ("", "+")
        } else if let Some(rest) = spec.strip_suffix("++") {
            (rest, "+")
        } else {
            match spec.rfind('+') {
                Some(at) => (&spec[..at], &spec[at + 1..]),
                None => ("", spec),
            }
        };
        let (mut ctrl, mut alt, mut shift) = (false, false, false);
        for part in mods.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => ctrl = true,
                "alt" | "option" | "meta" => alt = true,
                "shift" => shift = true,
                _ => return None,
            }
        }
        let name = name.trim();
        let code = match name.to_ascii_lowercase().as_str() {
            "space" => KeyCode::Char(' '),
            "enter" | "return" => KeyCode::Enter,
            "tab" if shift => KeyCode::BackTab,
            "tab" => KeyCode::Tab,
            "backtab" => KeyCode::BackTab,
            "esc" | "escape" => KeyCode::Esc,
            "backspace" => KeyCode::Backspace,
            "delete" | "del" => KeyCode::Delete,
            "insert" => KeyCode::Insert,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" | "pgup" => KeyCode::PageUp,
            "pagedown" | "pgdn" => KeyCode::PageDown,
            "backslash" => KeyCode::Char('\\'),
            "plus" => KeyCode::Char('+'),
            "minus" => KeyCode::Char('-'),
            lower if lower.len() > 1 && lower.starts_with('f') && lower[1..].chars().all(|c| c.is_ascii_digit()) => KeyCode::F(lower[1..].parse().ok().filter(|n| (1..=24).contains(n))?),
            _ => {
                let mut chars = name.chars();
                let c = chars.next()?;
                if chars.next().is_some() {
                    return None;
                }
                // "shift+d" is the same key as "D".
                KeyCode::Char(if shift { c.to_ascii_uppercase() } else { c })
            }
        };
        // Shift is told apart only where it makes another character (a
        // capital, Shift+Tab). With an arrow or a function key, or with
        // Ctrl held, most terminals do not say whether it was down: a key
        // written with it there would fire without it, and take the key
        // from whatever has the plain one.
        let shifts = matches!(code, KeyCode::BackTab) || matches!(code, KeyCode::Char(c) if c.is_alphabetic() && !ctrl);
        if shift && !shifts {
            return None;
        }
        Some(Key { code, ctrl, alt })
    }

    pub fn matches(&self, key: &KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) != self.ctrl || key.modifiers.contains(KeyModifiers::ALT) != self.alt {
            return false;
        }
        match (self.code, key.code) {
            // With Ctrl held a terminal does not say whether Shift was too,
            // and reports Ctrl+\ and Ctrl+4 (and three more such pairs) as
            // whichever it likes.
            (KeyCode::Char(want), KeyCode::Char(got)) if self.ctrl => crate::config::ctrl_alias(want, true).eq_ignore_ascii_case(&crate::config::ctrl_alias(got, true)),
            (KeyCode::Char(want), KeyCode::Char(got)) => want == got,
            (want, got) => want == got,
        }
    }

    /// As shown on screen: `c`, `^T`, `Alt+1`, `PgUp`.
    pub fn label(&self) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push('^');
        }
        if self.alt {
            s.push_str("Alt+");
        }
        match self.code {
            KeyCode::Char(' ') => s.push_str("Space"),
            KeyCode::Char(c) if self.ctrl => s.push(c.to_ascii_uppercase()),
            KeyCode::Char(c) => s.push(c),
            KeyCode::F(n) => s.push_str(&format!("F{n}")),
            KeyCode::Enter => s.push_str("Enter"),
            KeyCode::Tab => s.push_str("Tab"),
            KeyCode::BackTab => s.push_str("Shift+Tab"),
            KeyCode::Esc => s.push_str("Esc"),
            KeyCode::Backspace => s.push_str("Backspace"),
            KeyCode::Delete => s.push_str("Del"),
            KeyCode::Insert => s.push_str("Ins"),
            KeyCode::Up => s.push('↑'),
            KeyCode::Down => s.push('↓'),
            KeyCode::Left => s.push('←'),
            KeyCode::Right => s.push('→'),
            KeyCode::Home => s.push_str("Home"),
            KeyCode::End => s.push_str("End"),
            KeyCode::PageUp => s.push_str("PgUp"),
            KeyCode::PageDown => s.push_str("PgDn"),
            _ => s.push('?'),
        }
        s
    }

    /// Would this key, pressed with no prefix, get in the way of typing?
    fn types_text(&self) -> bool {
        !self.ctrl && !self.alt && matches!(self.code, KeyCode::Char(_) | KeyCode::Enter | KeyCode::Tab | KeyCode::Backspace | KeyCode::Esc)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Binding {
    pub key: Key,
    /// Pressed after the prefix key, or by itself.
    pub after_prefix: bool,
}

impl Binding {
    /// `c` or `prefix c` (after the prefix), `ctrl+t` (by itself).
    pub fn parse(spec: &str) -> Result<Binding, String> {
        let spec = spec.trim();
        let lower = spec.to_ascii_lowercase();
        let (explicit, rest) = match lower.strip_prefix("prefix") {
            Some(after) if after.starts_with([' ', '+']) => (true, spec[6..].trim_start_matches([' ', '+'])),
            _ => (false, spec),
        };
        let key = Key::parse(rest).ok_or_else(|| format!("`{spec}` is not a key"))?;
        // A key that types something can only be meant for after the prefix.
        Ok(Binding { key, after_prefix: explicit || key.types_text() || (!key.ctrl && !key.alt) })
    }
}

/// One entry of the `[keys]` table: a key, or several.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum KeySpec {
    One(String),
    Many(Vec<String>),
}

#[derive(Clone, Debug)]
pub struct Keymap {
    bindings: Vec<(Binding, Action)>,
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap::new(&BTreeMap::new()).0
    }
}

impl Keymap {
    fn defaults() -> Vec<(&'static str, Action)> {
        let mut all = vec![
            ("c", Action::NewTab),
            ("t", Action::NewTab),
            ("n", Action::NextTab),
            ("tab", Action::NextTab),
            ("p", Action::PrevTab),
            ("shift+tab", Action::PrevTab),
            ("|", Action::SplitRight),
            ("%", Action::SplitRight),
            ("-", Action::SplitDown),
            ("\"", Action::SplitDown),
            ("left", Action::FocusLeft),
            ("right", Action::FocusRight),
            ("up", Action::FocusUp),
            ("down", Action::FocusDown),
            ("z", Action::Zoom),
            ("x", Action::Close),
            ("w", Action::Close),
            ("<", Action::MoveTabLeft),
            (">", Action::MoveTabRight),
            ("d", Action::Detach),
            ("q", Action::Detach),
            ("D", Action::DetachAll),
            ("a", Action::AgentPanel),
            (",", Action::Rename),
            ("r", Action::Rename),
            ("e", Action::Explain),
            ("f", Action::Fix),
            ("o", Action::Watch),
            ("m", Action::Model),
            ("s", Action::Theme),
            ("T", Action::Theme),
            ("?", Action::Help),
            ("h", Action::Help),
            ("/", Action::Find),
            ("[", Action::CopyMode),
            ("pageup", Action::PageUp),
            ("pagedown", Action::PageDown),
            ("space", Action::Menu),
            (".", Action::Settings),
        ];
        for (i, key) in ["1", "2", "3", "4", "5", "6", "7", "8", "9"].into_iter().enumerate() {
            all.push((key, Action::Tab(i as u8)));
        }
        all
    }

    /// The default keys with the changes a `[keys]` table asks for, and
    /// what could not be made sense of in it. Naming an action replaces all
    /// of its keys; `"none"` (or an empty list) leaves it without one.
    pub fn new(changes: &BTreeMap<String, KeySpec>) -> (Keymap, Vec<String>) {
        let mut warnings = Vec::new();
        let mut bindings: Vec<(Binding, Action)> =
            Keymap::defaults().into_iter().filter_map(|(spec, action)| Binding::parse(spec).ok().map(|b| (b, action))).collect();
        // What the table names gives up the keys it had first, all of it
        // before any is given new ones: two actions may swap keys.
        for action in changes.keys().filter_map(|name| Action::named(name)) {
            bindings.retain(|(_, a)| *a != action);
        }
        for (name, spec) in changes {
            let Some(action) = Action::named(name) else {
                warnings.push(format!("[keys] `{name}` is not something a key can be given to (see `astrum keys`)"));
                continue;
            };
            let specs: Vec<&str> = match spec {
                KeySpec::One(one) => vec![one.as_str()],
                KeySpec::Many(many) => many.iter().map(String::as_str).collect(),
            };
            let mut wanted = Vec::new();
            for spec in specs.into_iter().filter(|s| !s.trim().is_empty() && !s.trim().eq_ignore_ascii_case("none")) {
                match Binding::parse(spec) {
                    Ok(binding) => wanted.push(binding),
                    Err(why) => warnings.push(format!("[keys] {name}: {why}")),
                }
            }
            bindings.retain(|(_, a)| *a != action);
            for binding in wanted {
                // A key does one thing: it is taken from whatever had it.
                if let Some((_, was)) = bindings.iter().find(|(b, a)| *b == binding && *a != action) {
                    if changes.keys().any(|other| Action::named(other) == Some(*was)) {
                        warnings.push(format!("[keys] `{}` is given to both {} and {name}", spec_of(&binding), was.name()));
                    }
                }
                bindings.retain(|(b, _)| *b != binding);
                bindings.push((binding, action));
            }
        }
        (Keymap { bindings }, warnings)
    }

    /// What a key pressed after the prefix does.
    pub fn after_prefix(&self, key: &KeyEvent) -> Option<Action> {
        let find = |key: &KeyEvent| self.bindings.iter().find(|(b, _)| b.after_prefix && b.key.matches(key)).map(|(_, a)| *a);
        // The prefix is pressed with Ctrl, and Ctrl is often still down
        // for the key after it: Ctrl+B Ctrl+C is Ctrl+B c, unless that
        // key with Ctrl has been given something of its own.
        find(key).or_else(|| {
            let mut plain = *key;
            plain.modifiers.remove(KeyModifiers::CONTROL);
            key.modifiers.contains(KeyModifiers::CONTROL).then(|| find(&plain)).flatten()
        })
    }

    /// What a key pressed by itself does, if it is one of Astrum's.
    pub fn direct(&self, key: &KeyEvent) -> Option<Action> {
        self.bindings.iter().find(|(b, _)| !b.after_prefix && b.key.matches(key)).map(|(_, a)| *a)
    }

    /// The first key for an action.
    pub fn binding(&self, action: Action) -> Option<Binding> {
        self.bindings.iter().find(|(_, a)| *a == action).map(|(b, _)| *b)
    }

    /// The first key for an action that is pressed after the prefix.
    pub fn prefix_key(&self, action: Action) -> Option<Key> {
        self.bindings.iter().find(|(b, a)| *a == action && b.after_prefix).map(|(b, _)| b.key)
    }

    /// The key for an action as shown on screen, given how the prefix key
    /// is shown: `^B c`, or `^T` for a key that needs no prefix. Empty
    /// when the action has no key.
    pub fn label(&self, action: Action, prefix: &str) -> String {
        match self.binding(action) {
            Some(b) if b.after_prefix => format!("{prefix} {}", b.key.label()),
            Some(b) => b.key.label(),
            None => String::new(),
        }
    }

    /// Every key of an action, as written in the config file.
    pub fn specs(&self, action: Action) -> Vec<String> {
        self.bindings.iter().filter(|(_, a)| *a == action).map(|(b, _)| spec_of(b)).collect()
    }
}

/// A binding as it would be written in the config file.
fn spec_of(binding: &Binding) -> String {
    let key = &binding.key;
    let mut s = String::new();
    if key.ctrl {
        s.push_str("ctrl+");
    }
    if key.alt {
        s.push_str("alt+");
    }
    s.push_str(&match key.code {
        KeyCode::Char(' ') => "space".to_string(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::F(n) => format!("f{n}"),
        KeyCode::Enter => "enter".into(),
        KeyCode::Tab => "tab".into(),
        KeyCode::BackTab => "shift+tab".into(),
        KeyCode::Esc => "esc".into(),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Delete => "delete".into(),
        KeyCode::Insert => "insert".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        KeyCode::PageUp => "pageup".into(),
        KeyCode::PageDown => "pagedown".into(),
        _ => "?".into(),
    });
    // Only said where it would otherwise be read as a key by itself.
    if binding.after_prefix && (key.ctrl || key.alt) {
        s = format!("prefix {s}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn keys_are_read_and_told_apart() {
        let d = Key::parse("d").unwrap();
        let big_d = Key::parse("D").unwrap();
        assert!(d.matches(&press(KeyCode::Char('d'), KeyModifiers::NONE)));
        assert!(!d.matches(&press(KeyCode::Char('D'), KeyModifiers::SHIFT)));
        assert!(big_d.matches(&press(KeyCode::Char('D'), KeyModifiers::SHIFT)));
        assert_eq!(Key::parse("shift+d"), Some(big_d));
        // Chords, names, and the awkward ones.
        let ctrl_t = Key::parse("ctrl+t").unwrap();
        assert!(ctrl_t.matches(&press(KeyCode::Char('t'), KeyModifiers::CONTROL)));
        assert!(!ctrl_t.matches(&press(KeyCode::Char('t'), KeyModifiers::NONE)));
        assert_eq!(ctrl_t.label(), "^T");
        assert_eq!(Key::parse("Alt+1").unwrap().label(), "Alt+1");
        assert_eq!(Key::parse("shift+tab").unwrap().code, KeyCode::BackTab);
        assert_eq!(Key::parse("pageup").unwrap().label(), "PgUp");
        assert_eq!(Key::parse("f5").unwrap().code, KeyCode::F(5));
        assert_eq!(Key::parse("+").unwrap().code, KeyCode::Char('+'));
        assert_eq!(Key::parse("ctrl++").unwrap(), Key { code: KeyCode::Char('+'), ctrl: true, alt: false });
        assert_eq!(Key::parse("-").unwrap().code, KeyCode::Char('-'));
        for bad in ["", "ctrl+", "hyper+x", "two keys", "f99"] {
            assert_eq!(Key::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_plain_key_is_for_after_the_prefix_and_a_chord_is_by_itself() {
        assert!(Binding::parse("c").unwrap().after_prefix);
        assert!(Binding::parse("pageup").unwrap().after_prefix);
        assert!(!Binding::parse("ctrl+t").unwrap().after_prefix);
        assert!(!Binding::parse("alt+left").unwrap().after_prefix);
        assert!(Binding::parse("prefix ctrl+t").unwrap().after_prefix);
        assert!(Binding::parse("nonsense key").is_err());
    }

    #[test]
    fn keys_as_people_write_and_press_them() {
        // Two actions swap keys: nothing is wrong with that.
        let mut changes = BTreeMap::new();
        changes.insert("next_tab".to_string(), KeySpec::One("p".into()));
        changes.insert("prev_tab".to_string(), KeySpec::One("n".into()));
        let (keys, warnings) = Keymap::new(&changes);
        assert!(warnings.is_empty(), "{warnings:?}");
        let plain = |c| press(KeyCode::Char(c), KeyModifiers::NONE);
        assert_eq!((keys.after_prefix(&plain('p')), keys.after_prefix(&plain('n'))), (Some(Action::NextTab), Some(Action::PrevTab)));
        // Two given the same key is still said.
        let mut changes = BTreeMap::new();
        changes.insert("next_tab".to_string(), KeySpec::One("y".into()));
        changes.insert("prev_tab".to_string(), KeySpec::One("y".into()));
        let (_, warnings) = Keymap::new(&changes);
        assert!(warnings.iter().any(|w| w.contains("given to both")), "{warnings:?}");

        // Shift where a terminal would not say it was held is not a key:
        // better refused than taking the plain key from what has it.
        for spec in ["shift+left", "shift+f5", "ctrl+shift+t", "shift+1"] {
            assert!(Key::parse(spec).is_none(), "{spec}");
        }
        assert!(Key::parse("shift+d").is_some() && Key::parse("shift+tab").is_some() && Key::parse("alt+shift+d").is_some());

        // Ctrl+\ is reported as Ctrl+4 by terminals that do not spell keys out.
        let key = Key::parse("ctrl+backslash").unwrap();
        assert!(key.matches(&press(KeyCode::Char('\\'), KeyModifiers::CONTROL)) && key.matches(&press(KeyCode::Char('4'), KeyModifiers::CONTROL)));
        assert!(!key.matches(&press(KeyCode::Char('4'), KeyModifiers::NONE)));

        // After the prefix, Ctrl may still be down: the key is the same
        // key, unless that one with Ctrl has something of its own.
        let keys = Keymap::default();
        assert_eq!(keys.after_prefix(&press(KeyCode::Char('c'), KeyModifiers::CONTROL)), Some(Action::NewTab));
        assert_eq!(keys.after_prefix(&press(KeyCode::Char('d'), KeyModifiers::CONTROL)), Some(Action::Detach));
        let mut changes = BTreeMap::new();
        changes.insert("zoom".to_string(), KeySpec::One("prefix ctrl+c".into()));
        let (keys, _) = Keymap::new(&changes);
        assert_eq!(keys.after_prefix(&press(KeyCode::Char('c'), KeyModifiers::CONTROL)), Some(Action::Zoom));
        assert_eq!(keys.after_prefix(&plain('c')), Some(Action::NewTab));
    }

    #[test]
    fn the_defaults_are_what_they_were_and_the_config_changes_them() {
        let keys = Keymap::default();
        let plain = |c| press(KeyCode::Char(c), KeyModifiers::NONE);
        assert_eq!(keys.after_prefix(&plain('c')), Some(Action::NewTab));
        assert_eq!(keys.after_prefix(&plain('|')), Some(Action::SplitRight));
        assert_eq!(keys.after_prefix(&plain('3')), Some(Action::Tab(2)));
        assert_eq!(keys.after_prefix(&press(KeyCode::Char('D'), KeyModifiers::SHIFT)), Some(Action::DetachAll));
        assert_eq!(keys.after_prefix(&plain('d')), Some(Action::Detach));
        assert_eq!(keys.after_prefix(&press(KeyCode::Left, KeyModifiers::NONE)), Some(Action::FocusLeft));
        assert_eq!(keys.direct(&plain('c')), None);
        assert_eq!(keys.label(Action::NewTab, "^B"), "^B c");
        // Every default is a key that parses, and no key does two things.
        assert_eq!(keys.bindings.len(), Keymap::defaults().len());
        for (i, (binding, action)) in keys.bindings.iter().enumerate() {
            assert!(!keys.bindings[..i].iter().any(|(b, _)| b == binding), "{:?} twice ({action:?})", binding.key);
        }

        let mut changes = BTreeMap::new();
        changes.insert("new_tab".to_string(), KeySpec::Many(vec!["ctrl+t".into(), "y".into()]));
        changes.insert("split-right".to_string(), KeySpec::One("v".into()));
        changes.insert("detach_all".to_string(), KeySpec::One("none".into()));
        changes.insert("fly".to_string(), KeySpec::One("x".into()));
        changes.insert("zoom".to_string(), KeySpec::One("not a key".into()));
        let (keys, warnings) = Keymap::new(&changes);
        // The new keys work, the old ones for those actions are free again.
        assert_eq!(keys.direct(&press(KeyCode::Char('t'), KeyModifiers::CONTROL)), Some(Action::NewTab));
        assert_eq!(keys.after_prefix(&plain('y')), Some(Action::NewTab));
        assert_eq!(keys.after_prefix(&plain('c')), None);
        assert_eq!(keys.after_prefix(&plain('v')), Some(Action::SplitRight));
        assert_eq!(keys.after_prefix(&plain('|')), None);
        assert_eq!(keys.binding(Action::DetachAll), None);
        assert_eq!(keys.label(Action::NewTab, "^B"), "^T");
        assert_eq!(keys.specs(Action::NewTab), vec!["ctrl+t".to_string(), "y".to_string()]);
        // Untouched actions keep theirs.
        assert_eq!(keys.after_prefix(&plain('n')), Some(Action::NextTab));
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("fly")) && warnings.iter().any(|w| w.contains("not a key")));

        // A key given to something else is taken from what had it.
        let mut changes = BTreeMap::new();
        changes.insert("zoom".to_string(), KeySpec::One("n".into()));
        let (keys, warnings) = Keymap::new(&changes);
        assert_eq!(keys.after_prefix(&plain('n')), Some(Action::Zoom));
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(keys.specs(Action::NextTab), vec!["tab".to_string()]);
    }

    #[test]
    fn every_action_has_a_name() {
        for (name, action, what) in ACTIONS {
            assert_eq!(Action::named(name), Some(*action));
            assert_eq!(action.name(), *name);
            assert!(!what.is_empty());
        }
        assert_eq!(Action::named("New-Tab"), Some(Action::NewTab));
    }
}
