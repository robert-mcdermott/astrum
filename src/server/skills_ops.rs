//! Skills in a session: `/name` to use one on purpose, `/skills`, the
//! settings screen's list, and the names offered in the panel's slash menu.
//! (The agent itself finds them at the start of each turn: see
//! `agent::skills`.)

use super::{Overlay, PickItem, Picker, PickerKind, Server};
use crate::agent::skills::{self, Shell, Skill};
use crate::paths;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// What the settings screen last found: where it looked, when, and the names
/// of the skills. The screen is drawn many times a second, and looking reads
/// folders and files: what was found is used for a little while.
struct Seen {
    at: Instant,
    place: Place,
    names: Vec<String>,
}

/// Where skills are looked for: the user's own folder, those the config
/// file names, and the project's folder.
type Place = (PathBuf, Vec<PathBuf>, Option<String>);

static SEEN: Mutex<Option<Seen>> = Mutex::new(None);
const SEEN_FOR: Duration = Duration::from_secs(2);

/// The names of the skills in `place`: as `look` found them lately, or as
/// it finds them now.
fn seen(last: &Mutex<Option<Seen>>, place: Place, look: impl FnOnce() -> Vec<String>) -> Vec<String> {
    let mut last = last.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = last.as_ref().filter(|s| s.place == place && s.at.elapsed() < SEEN_FOR) {
        return s.names.clone();
    }
    let names = look();
    *last = Some(Seen { at: Instant::now(), place, names: names.clone() });
    names
}

impl Server {
    /// Where tab `idx`'s skills are looked for: the project's folder, when
    /// its shell is on this machine; and where that shell is.
    pub(super) fn skill_place(&self, idx: usize) -> (Option<String>, Shell) {
        let Some(tab) = self.tabs.get(idx) else { return (None, Shell::Here) };
        let pane = tab.panes.agent();
        match (pane.there().is_some(), pane.inner().is_some()) {
            (_, true) => (None, Shell::Far),
            (true, false) => (None, Shell::FarWithAstrum),
            (false, false) => (pane.cwd.clone(), Shell::Here),
        }
    }

    /// The skills for tab `idx`, as found now (those turned off too); the
    /// tab's slash menu is brought up to date with them.
    pub(super) fn skills_for(&mut self, idx: usize) -> Vec<Skill> {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        let named: Vec<std::path::PathBuf> = self.cfg.agent.skill_dirs.iter().map(|d| paths::resolve(d, &home)).collect();
        let (project, _) = self.skill_place(idx);
        let found = skills::find(&skills::yours(), &named, project.as_deref().map(std::path::Path::new), std::path::Path::new(&home));
        let off = self.cfg.state.skills_off();
        if let Some(tab) = self.tabs.get_mut(idx) {
            // With where each is from: a project's takes the place of one of
            // yours of the same name, and the menu says whose it is.
            tab.agent.skills = found.iter().filter(|s| !off.contains(&s.name)).map(|s| (format!("/{}", s.name), format!("{} · {}", s.source.label(), s.description))).collect();
        }
        found
    }

    /// `/name`: use the skill of that name, with what follows as the request.
    /// False if there is no such skill.
    pub(super) fn use_skill_command(&mut self, idx: usize, name: &str, arg: &str) -> bool {
        let found = self.skills_for(idx);
        let Some(skill) = found.iter().find(|s| s.name == name) else { return false };
        if self.cfg.state.skills_off().iter().any(|n| n == name) {
            self.notice(idx, format!("The skill `{name}` is turned off. `/skills on {name}` turns it on again."));
            return true;
        }
        let (_, shell) = self.skill_place(idx);
        match skills::loaded(skill, shell) {
            Ok(text) => {
                let request = if arg.trim().is_empty() { "Do what this skill is for, here.".to_string() } else { arg.trim().to_string() };
                let sent = format!("{text}\nThe user chose this skill for what follows: use it.\n\n{request}");
                let shown = format!("/{name}{}", if arg.trim().is_empty() { String::new() } else { format!(" {}", arg.trim()) });
                self.start_turn_as(idx, shown, sent);
            }
            Err(why) => self.notice(idx, format!("The skill `{name}` could not be loaded: {why}.")),
        }
        true
    }

    /// `/skills`, `/skills off name`, `/skills on name`.
    pub(super) fn skills_command(&mut self, idx: usize, arg: &str) {
        let found = self.skills_for(idx);
        let mut words = arg.split_whitespace();
        let text = match (words.next(), words.next()) {
            (Some(verb @ ("off" | "on")), Some(name)) => match self.set_skill_on(name, verb == "on") {
                Ok(text) | Err(text) => text,
            },
            (Some("off" | "on"), None) => "`/skills off` and `/skills on` take the name of a skill.".into(),
            (Some(other), _) => format!("`/skills {other}` is not something `/skills` does: `/skills` lists them, `/skills off name` and `/skills on name` turn one off and on."),
            (None, _) if found.is_empty() => format!(
                "There are no skills. A skill is a folder with a `SKILL.md` in it: a short front matter with its `name` and a `description` of when it is of use, then the instructions. Yours go in `{}`, a project's in its `.agents/skills/`.",
                paths::tildify(&skills::yours().to_string_lossy())
            ),
            (None, _) => {
                let off = self.cfg.state.skills_off();
                let mut lines = vec!["Skills (the agent loads one when it fits; `/name` uses one on purpose):".to_string()];
                for s in &found {
                    let state = if off.contains(&s.name) { " · off" } else { "" };
                    let first = s.description.lines().next().unwrap_or("");
                    lines.push(format!("- `{}` · {}{state} · {} · `{}`", s.name, s.source.label(), crate::ui::text::truncate(first, 120), paths::tildify(&s.dir.to_string_lossy())));
                }
                let (_, shell) = self.skill_place(idx);
                if shell != Shell::Here {
                    lines.push("This tab's shell is on another machine: only your own skills are offered here, not those of a project there.".into());
                }
                lines.push("`/skills off name` and `/skills on name` turn one off and on.".into());
                lines.join("\n")
            }
        };
        self.notice(idx, text);
    }

    /// Turn a skill off or on, and remember it.
    fn set_skill_on(&mut self, name: &str, on: bool) -> Result<String, String> {
        let off = self.cfg.state.skills_off.get_or_insert_with(Vec::new);
        let was_off = off.iter().any(|n| n == name);
        match (on, was_off) {
            (true, false) => return Err(format!("`{name}` is not turned off.")),
            (false, true) => return Err(format!("`{name}` is already off.")),
            (true, true) => off.retain(|n| n != name),
            (false, false) => off.push(name.to_string()),
        }
        off.sort();
        self.cfg.save_state();
        let idx = self.active;
        self.skills_for(idx);
        Ok(if on { format!("The skill `{name}` is on again.") } else { format!("The skill `{name}` is off: the agent is no longer shown it.") })
    }

    /// The skills, to turn off and on (in the settings).
    pub(super) fn open_skills_picker(&mut self) {
        let idx = self.active;
        let found = self.skills_for(idx);
        let off = self.cfg.state.skills_off().to_vec();
        let items = found
            .iter()
            .map(|s| {
                let state = if off.contains(&s.name) { "off" } else { "on" };
                PickItem { label: s.name.clone(), detail: format!("{state} · {} · {}", s.source.label(), crate::ui::text::truncate(s.description.lines().next().unwrap_or(""), 60)), value: s.name.clone() }
            })
            .collect();
        let note = if found.is_empty() {
            format!("None yet: a skill is a folder with a SKILL.md, in {} or a project's .agents/skills", paths::tildify(&skills::yours().to_string_lossy()))
        } else {
            "Enter turns it off, or on again · /skills for more · Esc closes".to_string()
        };
        self.ui.overlay = Some(Overlay::Picker(Picker {
            kind: PickerKind::Skills,
            title: "Skills".into(),
            items,
            filter: String::new(),
            selected: 0,
            loading: false,
            note,
            original_theme: None,
            pending_delete: None,
        }));
        self.dirty = true;
    }

    pub(super) fn skill_picked(&mut self, name: &str) {
        let on = self.cfg.state.skills_off().iter().any(|n| n == name);
        let (Ok(text) | Err(text)) = self.set_skill_on(name, on);
        self.toast(text);
    }

    /// The settings screen's line for the skills: those found lately (see
    /// `seen`), and how many of them are off now.
    pub fn skills_summary(&self) -> String {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        let named: Vec<PathBuf> = self.cfg.agent.skill_dirs.iter().map(|d| paths::resolve(d, &home)).collect();
        let (project, _) = self.skill_place(self.active);
        let yours = skills::yours();
        let found = seen(&SEEN, (yours.clone(), named.clone(), project.clone()), || {
            skills::find(&yours, &named, project.as_deref().map(std::path::Path::new), std::path::Path::new(&home)).into_iter().map(|s| s.name).collect()
        });
        let off = found.iter().filter(|n| self.cfg.state.skills_off().contains(n)).count();
        match (found.len(), off) {
            (0, _) => "none yet".into(),
            (n, 0) => format!("{n}"),
            (n, off) => format!("{n} · {off} off"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_settings_screen_does_not_look_for_skills_each_time_it_is_drawn() {
        let last = Mutex::new(None);
        let looked = std::cell::Cell::new(0);
        let look = || {
            looked.set(looked.get() + 1);
            vec!["release".to_string()]
        };
        let here: Place = ("/cfg/skills".into(), Vec::new(), Some("/home/proj".into()));
        for _ in 0..100 {
            assert_eq!(seen(&last, here.clone(), look), ["release"]);
        }
        assert_eq!(looked.get(), 1);
        // Somewhere else, it looks again; and there again, after a while.
        seen(&last, ("/cfg/skills".into(), Vec::new(), Some("/home/other".into())), look);
        assert_eq!(looked.get(), 2);
        last.lock().unwrap().as_mut().unwrap().at -= SEEN_FOR;
        seen(&last, ("/cfg/skills".into(), Vec::new(), Some("/home/other".into())), look);
        assert_eq!(looked.get(), 3);
    }
}
