//! Layouts: a project opened the same way every time.
//!
//! A layout is a file of your own, `~/.config/astrum/layouts/<name>.toml`:
//! tabs, each with a name and a folder, split into panes, each pane with a
//! folder and, if wanted, a command.
//!
//! ```toml
//! [[tab]]
//! name = "web"
//! cwd = "~/src/shop"
//! split = "right"
//! panes = [{ command = "nvim ." }, { size = 35, command = "npm run dev" }]
//! ```
//!
//! Opening one adds its tabs to the session. Every pane is an ordinary
//! shell, and a pane's command is typed at that shell's first prompt, so
//! that aliases, `PATH` and the shell's own setup apply; when the command
//! ends, the shell is still there.

use super::panes::{Node, Panes};
use super::session::AgentSession;
use super::{Focus, Overlay, PickItem, Picker, PickerKind, Server, Tab};
use crate::pane::Pane;
use crate::paths;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// The most tabs, and panes in all, a layout may open: a slip of the pen
/// in a file is not to start a hundred shells.
const MAX_TABS: usize = 20;
const MAX_PANES: usize = 40;

/// How long a pane's command waits for a shell that never says it is at
/// its prompt; and how long after one that does.
const WAIT: std::time::Duration = std::time::Duration::from_millis(2500);
/// How long a shell that will say so (one with shell integration) is given
/// to reach its first prompt: its startup files may be slow, or ask something.
const WAIT_FOR_PROMPT: std::time::Duration = std::time::Duration::from_secs(20);
const AFTER_PROMPT: std::time::Duration = std::time::Duration::from_millis(120);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    #[serde(default, rename = "tab")]
    pub tabs: Vec<Part>,
}

/// A tab, or a pane, or a part of either that is itself split.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Part {
    /// A tab's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The folder its shell starts in (a tab's is that of all its panes,
    /// unless one says otherwise).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// What is typed at its shell's first prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Its share of the split it is in, in percent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u16>,
    /// Which way it is split into `panes`: `right` (side by side) or `down`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split: Option<Way>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub panes: Vec<Part>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Way {
    Right,
    Down,
}

pub fn dir() -> PathBuf {
    paths::config_dir().join("layouts")
}

/// Is this a name a layout can have? (It becomes a file's name.)
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 40 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The layouts there are, by name.
pub fn names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".toml").map(str::to_string))
        .filter(|n| valid_name(n))
        .collect();
    names.sort();
    names
}

pub fn load(name: &str) -> Result<Layout, String> {
    if !valid_name(name) {
        return Err(format!("`{name}` is not a layout's name (letters, digits, `-` and `_`)"));
    }
    let file = dir().join(format!("{name}.toml"));
    let text = std::fs::read_to_string(&file).map_err(|_| format!("There is no layout `{name}` ({})", paths::tildify(&file.to_string_lossy())))?;
    parse(&text).map_err(|why| format!("Layout `{name}`: {why}"))
}

pub fn parse(text: &str) -> Result<Layout, String> {
    let layout: Layout = toml::from_str(text).map_err(|e| e.message().to_string())?;
    if layout.tabs.is_empty() {
        return Err("it has no `[[tab]]`".into());
    }
    if layout.tabs.len() > MAX_TABS {
        return Err(format!("it has more than {MAX_TABS} tabs"));
    }
    let panes: usize = layout.tabs.iter().map(leaves).sum();
    if panes > MAX_PANES {
        return Err(format!("it has more than {MAX_PANES} panes"));
    }
    Ok(layout)
}

fn leaves(part: &Part) -> usize {
    if part.panes.is_empty() { 1 } else { part.panes.iter().map(leaves).sum() }
}

/// A pane to open: its folder and its command.
struct Leaf {
    cwd: Option<String>,
    command: Option<String>,
}

/// The tree of a part: its leaves are numbered in order into `out`. The
/// parts of a split share it by their sizes; those that give none share
/// what is left equally.
fn tree(part: &Part, cwd: Option<&str>, out: &mut Vec<Leaf>) -> Node {
    // (A folder not given in full is inside the one of what it is part of.)
    let joined = match (part.cwd.as_deref(), cwd) {
        (Some(own), Some(outer)) if !own.starts_with('/') && !own.starts_with('~') => Some(format!("{}/{own}", outer.trim_end_matches('/'))),
        (Some(own), _) => Some(own.to_string()),
        (None, outer) => outer.map(str::to_string),
    };
    let cwd = joined.as_deref();
    if part.panes.is_empty() {
        out.push(Leaf { cwd: cwd.map(str::to_string), command: part.command.clone().filter(|c| !c.trim().is_empty()) });
        return Node::Leaf(out.len() as u64 - 1);
    }
    let side_by_side = part.split != Some(Way::Down);
    let given: u32 = part.panes.iter().filter_map(|p| p.size).map(|s| s.clamp(1, 99) as u32).sum();
    let unsized_parts = part.panes.iter().filter(|p| p.size.is_none()).count() as u32;
    let rest = 100u32.saturating_sub(given).max(unsized_parts);
    let sizes: Vec<u32> = part.panes.iter().map(|p| p.size.map_or(rest / unsized_parts.max(1), |s| s.clamp(1, 99) as u32).max(1)).collect();
    // One beside the rest, and so on along the row.
    fn row(parts: &[Part], sizes: &[u32], side_by_side: bool, cwd: Option<&str>, out: &mut Vec<Leaf>) -> Node {
        let first = tree(&parts[0], cwd, out);
        if parts.len() == 1 {
            return first;
        }
        let total: u32 = sizes.iter().sum();
        let share = (sizes[0] * 1000 / total.max(1)).clamp(50, 950) as u16;
        Node::Split { side_by_side, share, a: Box::new(first), b: Box::new(row(&parts[1..], &sizes[1..], side_by_side, cwd, out)) }
    }
    row(&part.panes, &sizes, side_by_side, cwd, out)
}

/// A tab's panes as a part of a layout, for saving one.
fn part_of(node: &Node, panes: &Panes, home: &str) -> Part {
    match node {
        Node::Leaf(id) => {
            let pane = panes.get(*id);
            let cwd = pane.and_then(|p| p.cwd.clone()).map(|c| tilde(&c, home));
            Part { cwd, command: pane.and_then(Pane::running_line), ..Part::default() }
        }
        Node::Split { side_by_side, share, a, b } => {
            let percent = ((*share as u32 + 5) / 10).clamp(1, 99) as u16;
            let mut first = part_of(a, panes, home);
            first.size = Some(percent);
            let mut second = part_of(b, panes, home);
            second.size = Some(100 - percent);
            Part { split: Some(if *side_by_side { Way::Right } else { Way::Down }), panes: vec![first, second], ..Part::default() }
        }
    }
}

fn tilde(path: &str, home: &str) -> String {
    match path.strip_prefix(home) {
        Some("") => "~".to_string(),
        Some(rest) if rest.starts_with('/') && home.len() > 1 => format!("~{rest}"),
        _ => path.to_string(),
    }
}

impl Server {
    /// Open a layout: its tabs are added to the session, and the first of
    /// them comes to the front. Says why not, if not.
    pub fn open_layout(&mut self, name: &str, from: Option<&str>) -> Result<usize, String> {
        let layout = load(name)?;
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        // (Folders that are not given in full are from the folder of the
        // pane in front, or of the terminal that asked.)
        let here = from.map(str::to_string).or_else(|| self.tab().filter(|t| t.panes.focused().there().is_none()).and_then(|t| t.panes.focused().cwd.clone())).unwrap_or_else(|| home.clone());
        let mut missing = Vec::new();
        let (cols, rows) = self.pane_size(false);
        let first = self.tabs.len();
        for part in &layout.tabs {
            let mut leaves = Vec::new();
            let node = tree(part, None, &mut leaves);
            let mut ids = HashMap::new();
            let mut list = Vec::new();
            for (n, leaf) in leaves.into_iter().enumerate() {
                let cwd = leaf.cwd.as_deref().map(|c| paths::resolve(c, &here).to_string_lossy().into_owned());
                // A folder that is not there: the pane opens (in the home
                // folder), but its command is not run somewhere it was
                // not meant for.
                let gone = cwd.as_deref().is_some_and(|c| !std::path::Path::new(c).is_dir());
                if gone {
                    missing.push(leaf.cwd.clone().unwrap_or_default());
                }
                let Some(mut pane) = self.spawn_pane(Some(cwd.as_deref().unwrap_or(&here)), cols, rows) else { continue };
                pane.to_type = leaf.command.filter(|_| !gone).map(|c| (c, std::time::Instant::now()));
                ids.insert(n as u64, pane.id);
                list.push(pane);
            }
            let Some(id) = list.first().map(|p| p.id) else { continue };
            let panes = Panes::restored(list, &node, &ids, 0, false);
            self.tabs.push(Tab { id, panes, name: part.name.clone().filter(|n| !n.trim().is_empty()), agent: AgentSession::new(), focus: Focus::Shell, activity: false, owner: None, sized: (0, 0) });
        }
        let opened = self.tabs.len() - first;
        if opened == 0 {
            return Err(format!("Layout `{name}` opened nothing: no shell could be started"));
        }
        self.select_tab(first);
        self.dirty = true;
        if let Some(dir) = missing.first() {
            self.toast(format!("Layout `{name}`: there is no folder {dir}, so that pane's command was not run"));
        }
        Ok(opened)
    }

    /// The layouts there are, to pick one to open.
    pub fn open_layout_picker(&mut self) {
        let items: Vec<PickItem> = names()
            .into_iter()
            .map(|name| {
                let detail = match load(&name) {
                    Ok(layout) => {
                        let tabs: Vec<String> = layout.tabs.iter().enumerate().map(|(i, t)| t.name.clone().unwrap_or_else(|| format!("tab {}", i + 1))).collect();
                        tabs.join(", ")
                    }
                    Err(why) => why,
                };
                PickItem { label: name.clone(), detail, value: name }
            })
            .collect();
        let note = if items.is_empty() {
            format!("None yet: right-click a tab → Save this tab as a layout…, or write one in {}", paths::tildify(&dir().to_string_lossy()))
        } else {
            "Enter opens it as new tabs · Esc closes".to_string()
        };
        self.ui.overlay = Some(Overlay::Picker(Picker { kind: PickerKind::Layout, title: "Open a layout".into(), items, filter: String::new(), selected: 0, loading: false, note, original_theme: None, pending_delete: None }));
        self.dirty = true;
    }

    pub(super) fn layout_picked(&mut self, name: &str) {
        match self.open_layout(name, None) {
            Ok(n) if self.ui.toast.is_none() => self.toast(format!("Opened layout `{name}`: {n} tab{}", if n == 1 { "" } else { "s" })),
            Ok(_) => {}
            Err(why) => self.toast(why),
        }
    }

    /// Save a tab as a layout of that name: its splits, its folders, and
    /// the commands running in it now. One of the same name is added to:
    /// the tab joins the tabs it has, or takes the place of the one with
    /// its name.
    pub(super) fn save_layout(&mut self, tab: u64, name: &str) {
        let name = name.trim();
        if !valid_name(name) {
            self.toast("A layout's name may only have letters, digits, '-' and '_'");
            return;
        }
        let Some(idx) = self.tab_index(tab) else { return };
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        let t = &self.tabs[idx];
        let mut part = part_of(t.panes.layout(), &t.panes, &home);
        part.name = Some(crate::ui::tab_title(t));
        // One of that name is added to; but one that cannot be read is
        // left as it is, with all that is in it.
        let file = dir().join(format!("{name}.toml"));
        let mut layout = match load(name) {
            Ok(layout) => layout,
            Err(_) if !file.exists() => Layout::default(),
            Err(why) => {
                self.toast(format!("Not saved, and `{name}` left as it is, because it cannot be read: {why}"));
                return;
            }
        };
        match layout.tabs.iter().position(|p| p.name == part.name) {
            Some(at) => layout.tabs[at] = part,
            None if layout.tabs.len() >= MAX_TABS => {
                self.toast(format!("Layout `{name}` has {MAX_TABS} tabs already, which is as many as one may have"));
                return;
            }
            None => layout.tabs.push(part),
        }
        let written = std::fs::create_dir_all(dir()).map_err(|e| e.to_string()).and_then(|()| toml::to_string(&layout).map_err(|e| e.to_string())).and_then(|text| std::fs::write(&file, text).map_err(|e| e.to_string()));
        match written {
            Ok(()) => self.toast(format!("Saved to layout `{name}` ({} tab{}): {}", layout.tabs.len(), if layout.tabs.len() == 1 { "" } else { "s" }, paths::tildify(&file.to_string_lossy()))),
            Err(why) => self.toast(format!("Could not save the layout: {why}")),
        }
    }

    /// A pane's shell is about to draw its first prompt: its command is
    /// typed a moment from now, once the prompt is there.
    pub(super) fn type_command(&mut self, idx: usize, pane: u64) {
        let Some(p) = self.tabs.get_mut(idx).and_then(|t| t.panes.get_mut(pane)) else { return };
        let wait = if matches!(p.shell_name.as_str(), "bash" | "zsh" | "fish") { WAIT_FOR_PROMPT + WAIT } else { WAIT };
        if let Some((_, since)) = &mut p.to_type {
            let soon = std::time::Instant::now().checked_sub(wait - AFTER_PROMPT);
            *since = soon.map_or(*since, |soon| soon.min(*since));
        }
    }

    /// Shells that never said they were at a prompt (no shell integration)
    /// are typed to after a moment all the same.
    pub(super) fn type_commands_due(&mut self) {
        let due: Vec<(usize, u64)> = self
            .tabs
            .iter()
            .enumerate()
            .flat_map(|(idx, t)| {
                // (A shell that says when it is at its prompt is waited for longer.)
                let limit = |p: &Pane| if matches!(p.shell_name.as_str(), "bash" | "zsh" | "fish") { WAIT_FOR_PROMPT + WAIT } else { WAIT };
                t.panes.iter().filter(move |p| p.to_type.as_ref().is_some_and(|(_, at)| at.elapsed() >= limit(p))).map(move |p| (idx, p.id))
            })
            .collect();
        for (idx, pane) in due {
            let Some(p) = self.tabs.get_mut(idx).and_then(|t| t.panes.get_mut(pane)) else { continue };
            if let Some((command, _)) = p.to_type.take() {
                p.write(format!("{command}\r").into_bytes());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_layout_is_read_and_its_panes_share_the_space_as_it_says() {
        let layout = parse(
            r#"
[[tab]]
name = "web"
cwd = "~/src/shop"
split = "right"
panes = [{ command = "nvim ." }, { size = 35, command = "npm run dev" }]

[[tab]]
name = "logs"
split = "down"
panes = [{}, { split = "right", cwd = "/var/log", panes = [{ command = "tail -f a" }, { command = "tail -f b" }] }]

[[tab]]
command = "htop"
"#,
        )
        .unwrap();
        let mut leaves = Vec::new();
        let web = tree(&layout.tabs[0], None, &mut leaves);
        assert!(matches!(web, Node::Split { side_by_side: true, share: 650, .. }), "{web:?}");
        assert_eq!(leaves.iter().map(|l| (l.cwd.as_deref(), l.command.as_deref())).collect::<Vec<_>>(), [(Some("~/src/shop"), Some("nvim .")), (Some("~/src/shop"), Some("npm run dev"))]);
        let mut leaves = Vec::new();
        let logs = tree(&layout.tabs[1], None, &mut leaves);
        assert!(matches!(&logs, Node::Split { side_by_side: false, share: 500, b, .. } if matches!(**b, Node::Split { side_by_side: true, .. })), "{logs:?}");
        assert_eq!(leaves.iter().map(|l| l.cwd.as_deref()).collect::<Vec<_>>(), [None, Some("/var/log"), Some("/var/log")]);
        // A folder not given in full is inside its tab's.
        let inside = parse("[[tab]]\ncwd = \"~/shop\"\npanes = [{ cwd = \"api\" }, { cwd = \"/tmp\" }]\n").unwrap();
        let mut leaves = Vec::new();
        tree(&inside.tabs[0], None, &mut leaves);
        assert_eq!(leaves.iter().map(|l| l.cwd.as_deref()).collect::<Vec<_>>(), [Some("~/shop/api"), Some("/tmp")]);
        let mut leaves = Vec::new();
        assert_eq!(tree(&layout.tabs[2], None, &mut leaves), Node::Leaf(0));
        // What is not a layout says why.
        assert!(parse("").unwrap_err().contains("no `[[tab]]`"));
        assert!(parse("[[tab]]\ncolour = 1\n").is_err());
        let many = "[[tab]]\n".repeat(MAX_TABS + 1);
        assert!(parse(&many).unwrap_err().contains("more than"));
        // And one written out reads back the same.
        let again = parse(&toml::to_string(&layout).unwrap()).unwrap();
        assert_eq!(again, layout);
    }

    #[test]
    fn names_that_would_leave_the_folder_are_no_names() {
        assert!(valid_name("web-2") && !valid_name("../x") && !valid_name("a/b") && !valid_name("") && !valid_name("a.toml"));
        assert_eq!(tilde("/home/me/src", "/home/me"), "~/src");
        assert_eq!(tilde("/home/meow", "/home/me"), "/home/meow");
    }
}
