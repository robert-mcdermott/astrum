//! From what a pane shows to the right place: a file it names opened in
//! your editor at the line, what a command printed copied whole, the
//! prompts gone between, and the whole history opened in your editor.

use super::{Focus, Server};
use crate::pane::{FileAt, Rows};
use std::path::PathBuf;

/// Editors that are told the line as part of the file's name with this
/// option before it (`-g file:12:3`).
const WITH_GOTO: &[&str] = &["code", "code-insiders", "codium", "cursor", "windsurf"];
/// Editors that are told the line as part of the file's name (`file:12:3`).
const WITH_NAME: &[&str] = &["subl", "zed", "hx", "helix"];

/// What to run to open a file at its line, with `editor` (as `$EDITOR`
/// says it: a program and perhaps some words for it).
pub fn editor_command(editor: &str, file: &FileAt) -> Vec<String> {
    let mut command: Vec<String> = editor.split_whitespace().map(str::to_string).collect();
    if command.is_empty() {
        command.push("vi".into());
    }
    let name = std::path::Path::new(&command[0]).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let path = file.path.display().to_string();
    let at = match (file.line, file.col) {
        (Some(line), Some(col)) => format!("{path}:{line}:{col}"),
        (Some(line), None) => format!("{path}:{line}"),
        _ => path.clone(),
    };
    match file.line {
        Some(_) if WITH_GOTO.contains(&name.as_str()) => command.extend(["-g".to_string(), at]),
        Some(_) if WITH_NAME.contains(&name.as_str()) => command.push(at),
        Some(line) => command.extend([format!("+{line}"), path]),
        None => command.push(path),
    }
    command
}

impl Server {
    /// The editor to open files with: the one the shell of the terminal in
    /// the lead names, else the one this session was started with, else vi.
    fn editor(&self) -> String {
        let lead = self.clients.iter().find(|c| Some(c.id) == self.lead && c.attached);
        lead.and_then(|c| c.editor.clone())
            .or_else(|| ["VISUAL", "EDITOR"].iter().find_map(|v| std::env::var(v).ok()))
            .filter(|e| !e.trim().is_empty())
            .unwrap_or_else(|| "vi".into())
    }

    /// Run a program in a new pane beside the one with the keyboard (or
    /// over it, zoomed), in that pane's folder. The pane closes when the
    /// program ends. Returns the new pane.
    fn run_beside(&mut self, run: Vec<String>, zoomed: bool) -> Option<u64> {
        let area = self.panes_area();
        let tab = self.tab()?;
        if tab.panes.zoomed {
            self.toggle_zoom();
        }
        let tab = self.tab()?;
        if !zoomed && !tab.panes.room_to_split(area, true) {
            self.toast("Not enough room beside this pane");
            return None;
        }
        // (The folder of a shell on another machine is not one of this one's.)
        let cwd = tab.panes.focused().there().is_none().then(|| tab.panes.focused().cwd.clone()).flatten();
        let from = self.focused_rect();
        let (cols, rows) = if zoomed { (area.width.max(2), area.height.max(1)) } else { ((from.width / 2).max(2), from.height.max(1)) };
        let pane = self.spawn_running(cwd.as_deref(), cols, rows, Some(&run))?;
        let id = pane.id;
        let tab = self.tab_mut()?;
        tab.panes.add(pane, true);
        tab.focus = Focus::Shell;
        tab.panes.zoomed = zoomed;
        self.focus_pane(id);
        Some(id)
    }

    /// Open a file at its line in the editor, beside the pane it was named in.
    pub(super) fn open_in_editor(&mut self, file: &FileAt) {
        let command = editor_command(&self.editor(), file);
        if self.run_beside(command, false).is_some() {
            self.toast(format!("Opening {}", crate::ui::text::truncate(&file.label(), 60)));
        }
    }

    /// Copy what a command printed, with its command line if `with`.
    pub(super) fn copy_output(&mut self, rows: &Rows, with: bool) {
        let Some(tab) = self.tab_mut() else { return };
        let text = tab.panes.focused_mut().copy_command(rows, with);
        self.dirty = true;
        match text.filter(|t| !t.trim().is_empty()) {
            Some(text) => {
                let lines = text.trim_end().lines().count();
                self.copy_to_clipboard(text);
                let what = if rows.cmd.is_empty() { "the command".to_string() } else { format!("`{}`", crate::ui::text::truncate(&rows.cmd, 40)) };
                self.toast(format!("Copied what {what} printed ({lines} line{})", if lines == 1 { "" } else { "s" }));
            }
            None => self.toast("That command printed nothing"),
        }
    }

    /// Go to the prompt before (or after) the one in view, in the pane
    /// with the keyboard.
    pub(super) fn to_prompt(&mut self, older: bool) {
        let Some(tab) = self.tab_mut() else { return };
        if !tab.panes.focused_mut().to_prompt(older) {
            self.toast(if older { "There is no prompt before this one" } else { "There is no prompt after this one" });
        }
        self.dirty = true;
    }

    /// Open the pane's history, as plain text, in the editor, at the line
    /// in view, zoomed over the pane. The file is readable only by you, and
    /// is removed when the editor's pane closes.
    pub(super) fn history_in_editor(&mut self) {
        let Some(tab) = self.tab() else { return };
        let (text, line) = tab.panes.focused().history_text();
        let file = match write_private(&text) {
            Ok(file) => file,
            Err(e) => {
                self.toast(format!("Could not write the history out: {e}"));
                return;
            }
        };
        let command = editor_command(&self.editor(), &FileAt { path: file.clone(), line: Some(line as u32), col: None });
        match self.run_beside(command, true) {
            Some(id) => {
                if let Some(pane) = self.tab_mut().and_then(|t| t.panes.get_mut(id)) {
                    pane.remove_after = Some(file);
                }
            }
            None => {
                let _ = std::fs::remove_file(&file);
            }
        }
    }
}

/// Write text to a new file only this user can read, in Astrum's own
/// runtime folder.
fn write_private(text: &str) -> std::io::Result<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let dir = crate::paths::runtime_dir().map_err(std::io::Error::other)?;
    std::fs::create_dir_all(&dir)?;
    let name = format!("history-{}-{}.txt", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    let path = dir.join(name);
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
    file.write_all(text.as_bytes())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_editor_is_told_the_line_its_own_way() {
        let file = FileAt { path: "/p/a.rs".into(), line: Some(12), col: Some(3) };
        assert_eq!(editor_command("vim", &file), ["vim", "+12", "/p/a.rs"]);
        assert_eq!(editor_command("/usr/bin/code --wait", &file), ["/usr/bin/code", "--wait", "-g", "/p/a.rs:12:3"]);
        assert_eq!(editor_command("hx", &file), ["hx", "/p/a.rs:12:3"]);
        assert_eq!(editor_command("  ", &FileAt { line: None, ..file }), ["vi", "/p/a.rs"]);
    }
}
