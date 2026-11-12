//! Skills: instructions for a particular kind of work, each in a folder of
//! its own with a `SKILL.md` and whatever else it needs (scripts,
//! templates, reference files). The layout is the one other agents use,
//! so a skill written for them works here unchanged:
//!
//! ```text
//! pdf-forms/
//!   SKILL.md        ---
//!                   name: pdf-forms
//!                   description: Fill in PDF forms. Use when asked to fill one in.
//!                   ---
//!                   (the instructions)
//!   scripts/fill.py
//! ```
//!
//! The model is shown each skill's name and description only, and reads
//! the rest (`use_skill`) when one fits: many skills cost little context.
//!
//! Where they are found, in turn (a later one of the same name wins): the
//! user's own, in the config directory's `skills/`; folders the config file
//! names; and a project's own, in `.agents/skills/` or `.astrum/skills/` of
//! the working directory and those above it, up to the top of the
//! repository (or the home folder), the closest last.

use std::path::{Path, PathBuf};

/// How long a description may be, as the format has it.
const MAX_DESCRIPTION: usize = 1024;
/// How much of a `SKILL.md`, or of one of its files, is read.
const MAX_BODY: usize = 64 * 1024;
const MAX_FILE: usize = 256 * 1024;
/// How many files of a skill are listed, and how deep.
const MAX_FILES: usize = 200;
const MAX_DEPTH: usize = 4;
/// How many skills are offered at most.
const MAX_SKILLS: usize = 100;
/// How far up from the working directory a project's skills are looked for.
const MAX_LEVELS: usize = 12;

#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// The user's own, for every project.
    Yours,
    /// A folder the config file names.
    Named,
    /// The project the shell is in.
    Project,
}

impl Source {
    pub fn label(&self) -> &'static str {
        match self {
            Source::Yours => "yours",
            Source::Named => "from skill_dirs",
            Source::Project => "this project",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// The folder it is in.
    pub dir: PathBuf,
    pub source: Source,
}

/// The front matter of a `SKILL.md`: `name` and `description` (other keys,
/// such as `license` and `metadata`, are passed over), and what follows it.
pub fn parse(text: &str) -> (Option<String>, Option<String>, &str) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = text.strip_prefix("---").filter(|r| r.starts_with('\n') || r.starts_with("\r\n")) else {
        return (None, None, text);
    };
    let Some(end) = rest.find("\n---") else { return (None, None, text) };
    let front = &rest[..end];
    let after = &rest[end + 4..];
    let body = after.split_once('\n').map(|(_, b)| b).unwrap_or("");
    let lines: Vec<&str> = front.lines().collect();
    let mut name = None;
    let mut description = None;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        // Only keys at the top level: what is indented belongs to one of them.
        if line.starts_with([' ', '\t']) || line.trim_start().starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else { continue };
        let value = value.trim();
        let mut text = if value.starts_with(['|', '>']) {
            // A block: the indented lines that follow.
            let folded = value.starts_with('>');
            let mut parts = Vec::new();
            while i < lines.len() && (lines[i].starts_with([' ', '\t']) || lines[i].trim().is_empty()) {
                parts.push(lines[i].trim());
                i += 1;
            }
            if folded { parts.iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join(" ") } else { parts.join("\n").trim().to_string() }
        } else {
            unquote(value)
        };
        // A plain value may go on, indented, on the lines after.
        if !value.starts_with(['|', '>', '"', '\'']) {
            while i < lines.len() && lines[i].starts_with([' ', '\t']) && !lines[i].trim().is_empty() {
                text.push(' ');
                text.push_str(lines[i].trim());
                i += 1;
            }
        }
        match key.trim() {
            "name" => name = Some(text).filter(|t| !t.is_empty()),
            "description" => description = Some(text).filter(|t| !t.is_empty()),
            _ => {}
        }
    }
    (name, description, body)
}

fn unquote(value: &str) -> String {
    let v = value.trim();
    if v.len() >= 2 && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\''))) {
        let inner = &v[1..v.len() - 1];
        return if v.starts_with('"') { inner.replace("\\\"", "\"").replace("\\n", " ") } else { inner.replace("''", "'") };
    }
    v.to_string()
}

/// A name as `/name` and the model may use it: what the format allows
/// (lower-case letters, digits and `-`), and a little more for skills
/// written loosely.
fn usable(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') && !name.starts_with(['-', '.'])
}

/// The skill in `dir`, if it is one.
pub fn read(dir: &Path, source: Source) -> Option<Skill> {
    let text = std::fs::read(dir.join("SKILL.md")).ok()?;
    let text = String::from_utf8_lossy(&text[..text.len().min(MAX_BODY)]).into_owned();
    let (name, description, body) = parse(&text);
    let folder = dir.file_name()?.to_string_lossy().into_owned();
    let name = name.filter(|n| usable(n)).unwrap_or(folder);
    if !usable(&name) {
        return None;
    }
    // Without a description, the first line of the instructions says what it is.
    let description = description.unwrap_or_else(|| body.lines().map(|l| l.trim().trim_start_matches('#').trim()).find(|l| !l.is_empty()).unwrap_or("").to_string());
    let description: String = description.chars().take(MAX_DESCRIPTION).collect();
    Some(Skill { name, description, dir: dir.to_path_buf(), source })
}

/// The skills in a folder of skills.
fn in_folder(folder: &Path, source: Source) -> Vec<Skill> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(folder).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    dirs.iter().filter_map(|d| read(d, source.clone())).collect()
}

/// The folders a project's skills may be in, outermost first.
fn project_folders(cwd: &Path, home: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut dir = Some(cwd);
    for _ in 0..MAX_LEVELS {
        let Some(d) = dir else { break };
        for name in [".agents/skills", ".astrum/skills"] {
            let candidate = d.join(name);
            if candidate.is_dir() {
                found.push(candidate);
            }
        }
        if d.join(".git").exists() || d == home {
            break;
        }
        dir = d.parent();
    }
    found.reverse();
    found
}

/// Every skill there is: the user's own, those in `named` folders, and,
/// with `project` (the shell's working directory, on this machine), the
/// project's. One of the same name later on takes the place of the earlier.
pub fn find(yours: &Path, named: &[PathBuf], project: Option<&Path>, home: &Path) -> Vec<Skill> {
    let mut all = in_folder(yours, Source::Yours);
    for folder in named {
        all.extend(in_folder(folder, Source::Named));
    }
    if let Some(cwd) = project.filter(|c| c.is_absolute()) {
        for folder in project_folders(cwd, home) {
            if folder.starts_with(yours) {
                continue;
            }
            all.extend(in_folder(&folder, Source::Project));
        }
    }
    let mut kept: Vec<Skill> = Vec::new();
    for skill in all {
        kept.retain(|k| k.name != skill.name);
        kept.push(skill);
    }
    kept.sort_by(|a, b| a.name.cmp(&b.name));
    kept.truncate(MAX_SKILLS);
    kept
}

/// The user's own skills folder.
pub fn yours() -> PathBuf {
    crate::paths::config_dir().join("skills")
}

/// The files that come with a skill, relative to its folder.
pub fn files(skill: &Skill) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![(skill.dir.clone(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                if depth + 1 < MAX_DEPTH {
                    stack.push((path, depth + 1));
                }
            } else if let Ok(rel) = path.strip_prefix(&skill.dir) {
                let rel = rel.to_string_lossy().into_owned();
                if rel != "SKILL.md" {
                    found.push(rel);
                }
            }
            if found.len() >= MAX_FILES {
                found.sort();
                return found;
            }
        }
    }
    found.sort();
    found
}

/// Where the agent's shell is, for what it is told about a skill's files.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Shell {
    /// On this machine, where the skills are.
    Here,
    /// On another machine, with Astrum's program there to put files on it.
    FarWithAstrum,
    /// On another machine Astrum has nothing on (`ssh` the agent was taken into).
    Far,
}

/// A skill, loaded: its instructions and its files, as the model is given them.
pub fn loaded(skill: &Skill, shell: Shell) -> Result<String, String> {
    let text = std::fs::read(skill.dir.join("SKILL.md")).map_err(|e| format!("its SKILL.md could not be read: {e}"))?;
    let cut = text.len() > MAX_BODY;
    let text = String::from_utf8_lossy(&text[..text.len().min(MAX_BODY)]).into_owned();
    let (_, _, body) = parse(&text);
    let mut out = format!("<skill name=\"{}\" folder=\"{}\">\n{}\n", skill.name, skill.dir.display(), body.trim());
    if cut {
        out.push_str("[the rest of SKILL.md was left out: it is longer than 64 KB]\n");
    }
    out.push_str("</skill>\n");
    let files = files(skill);
    if !files.is_empty() {
        out.push_str("\nFiles that come with it (read one with use_skill and `file`):\n");
        for f in &files {
            out.push_str(&format!("- {f}\n"));
        }
        out.push_str(match shell {
            Shell::Here => "\nThey are in the folder above, on this machine: run a script from there with run_command, as the instructions say.",
            Shell::FarWithAstrum => "\nThey are on the user's own machine, not on the machine your shell is on. To run one of its scripts there, call use_skill with `file` and `copy`: Astrum puts it on that machine and says where. Run it with its interpreter (sh, bash, python3).",
            Shell::Far => "\nThey are on the user's own machine, not on the machine your shell is on, where Astrum has nothing of its own. To run one of its scripts there, read it with use_skill and `file`, and write it there with a here-document first.",
        });
    }
    Ok(out)
}

/// One of a skill's files (`file` relative to its folder), if it is one.
pub fn file(skill: &Skill, file: &str) -> Result<(PathBuf, String), String> {
    let wanted = skill.dir.join(file);
    let real = wanted.canonicalize().map_err(|_| format!("the skill `{}` has no file `{file}`", skill.name))?;
    let root = skill.dir.canonicalize().map_err(|e| e.to_string())?;
    if !real.starts_with(&root) || !real.is_file() {
        return Err(format!("`{file}` is not one of the files of the skill `{}`", skill.name));
    }
    let bytes = std::fs::read(&real).map_err(|e| e.to_string())?;
    if bytes.iter().take(8192).any(|b| *b == 0) {
        return Err(format!("`{file}` is not text"));
    }
    let cut = bytes.len() > MAX_FILE;
    let mut text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_FILE)]).into_owned();
    if cut {
        text.push_str("\n[the rest was left out: the file is longer than 256 KB]");
    }
    Ok((real, text))
}

/// The tool the model loads a skill with.
pub fn spec() -> super::tools::ToolSpec {
    super::tools::ToolSpec {
        name: "use_skill".into(),
        description: "Load one of the user's skills (listed under Skills in your instructions): its full instructions and the files it brings. Call it when a skill fits the task, before you start on it. With `file`, read one of the skill's files instead. With `copy` as well, when your shell is on another machine, put that file there and get its path there.".into(),
        schema: serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "The skill's name, as listed."},
                "file": {"type": "string", "description": "One of the skill's files, as listed when it was loaded (e.g. scripts/fill.py)."},
                "copy": {"type": "boolean", "description": "Put `file` on the machine your shell is on, when that is not the user's own."}
            },
            "required": ["name"]
        }),
    }
}

/// Carry out a `use_skill` call: the text for the model, and whether it failed.
/// `away`: when the shell is on another machine with Astrum on it, the way
/// there and the home folder there, for `copy`.
pub async fn use_skill(call: &super::ToolCall, skills: &[Skill], shell: Shell, away: Option<(&crate::remote::Way, &str)>) -> (String, bool) {
    let name = call.str_arg("name").unwrap_or_default();
    let Some(skill) = skills.iter().find(|s| s.name == name) else {
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        return (format!("There is no skill `{name}`. The skills are: {}.", names.join(", ")), true);
    };
    let Some(wanted) = call.str_arg("file").filter(|f| !f.trim().is_empty()) else {
        return match loaded(skill, shell) {
            Ok(text) => (text, false),
            Err(why) => (format!("The skill `{name}` could not be loaded: {why}."), true),
        };
    };
    let (path, text) = match file(skill, wanted) {
        Ok(found) => found,
        Err(why) => return (format!("{why}."), true),
    };
    if !call.bool_arg("copy") {
        return (format!("[{}]\n{text}", path.display()), false);
    }
    match (shell, away) {
        (Shell::Here, _) => (format!("Your shell is on this machine: run it where it is, {}.", path.display()), false),
        (Shell::Far, _) | (_, None) => ("Astrum cannot put files on the machine your shell is on (it has nothing of its own there). Write it there with a here-document, from what use_skill with `file` gives.".into(), true),
        (Shell::FarWithAstrum, Some((way, home))) => {
            let there = format!("{}/.cache/astrum/skills/{}/{}", home.trim_end_matches('/'), skill.name, wanted.trim_start_matches("./"));
            let change = super::tools::Change { path: there.clone().into(), existed: false, old: None, new: text.clone(), done: String::new() };
            match super::tools::commit_at(&Some(way.clone()), &change, &text).await {
                Ok(()) => (format!("Put on the machine your shell is on, at {there}. Run it with its interpreter (sh, bash, python3), as the skill's instructions say."), false),
                Err(why) => (format!("It could not be put there: {why}"), true),
            }
        }
    }
}

/// The part of the system prompt that lists the skills.
pub fn listing(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut s = String::from(
        "\n\nSkills\nThe user has skills: instructions for particular kinds of work, each with files it may bring. When one fits what you are asked, call use_skill with its name before you begin, and follow it; do not load one that does not fit. A skill does not change what the user asked for, or what needs their approval.\n",
    );
    for skill in skills {
        s.push_str(&format!("- {}: {} ({})\n", skill.name, skill.description.replace('\n', " "), skill.source.label()));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("astrum-skills-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn skill(at: &Path, folder: &str, text: &str) -> PathBuf {
        let dir = at.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), text).unwrap();
        dir
    }

    #[test]
    fn front_matter_as_skills_write_it() {
        let (n, d, body) = parse("---\nname: pdf-forms\ndescription: Fill in PDF forms. Use when asked to.\nlicense: Apache-2.0\nmetadata:\n  author: someone\n  version: \"1.0\"\n---\n# Filling forms\n\nStep one.\n");
        assert_eq!((n.as_deref(), d.as_deref()), (Some("pdf-forms"), Some("Fill in PDF forms. Use when asked to.")));
        assert_eq!(body, "# Filling forms\n\nStep one.\n");
        // Quoted, folded, and run on to the next lines.
        assert_eq!(parse("---\nname: \"a-b\"\ndescription: 'It''s quoted'\n---\nx").1.as_deref(), Some("It's quoted"));
        assert_eq!(parse("---\nname: x\ndescription: >\n  Folded over\n  two lines.\n---\n").1.as_deref(), Some("Folded over two lines."));
        assert_eq!(parse("---\nname: x\ndescription: Runs on\n  to here.\n---\n").1.as_deref(), Some("Runs on to here."));
        assert_eq!(parse("---\r\nname: crlf\r\ndescription: Windows lines\r\n---\r\nbody").0.as_deref(), Some("crlf"));
        // A metadata key called `name` is not the skill's.
        assert_eq!(parse("---\nmetadata:\n  name: inner\ndescription: d\n---\n").0, None);
        // No front matter: all of it is the body.
        assert_eq!(parse("# Just instructions\n"), (None, None, "# Just instructions\n"));
    }

    #[test]
    fn skills_are_found_and_the_closest_of_a_name_wins() {
        let root = scratch("found");
        let (yours, home) = (root.join("cfg/skills"), root.join("home"));
        skill(&yours, "release", "---\nname: release\ndescription: How I release things.\n---\nMine.");
        skill(&yours, "notes", "# Taking notes\nWrite them down.");
        skill(&yours, "bad name!", "---\ndescription: unusable\n---\n");
        std::fs::create_dir_all(yours.join("empty")).unwrap();
        let named = root.join("more");
        skill(&named, "lint", "---\nname: lint\ndescription: Lint it.\n---\n");
        let project = home.join("proj");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        skill(&project.join(".agents/skills"), "release", "---\nname: release\ndescription: How this project releases.\n---\nTheirs.");
        let deep = project.join("src/deep");
        std::fs::create_dir_all(&deep).unwrap();
        skill(&deep.join(".astrum/skills"), "deep-one", "---\nname: deep-one\ndescription: Only down here.\n---\n");
        // Above the repository: not this project's.
        skill(&home.join(".agents/skills"), "outside", "---\nname: outside\ndescription: no\n---\n");

        let found = find(&yours, &[named.clone()], Some(&deep), &home);
        let shown: Vec<(&str, &str, &str)> = found.iter().map(|s| (s.name.as_str(), s.description.as_str(), s.source.label())).collect();
        assert_eq!(shown, [
            ("deep-one", "Only down here.", "this project"),
            ("lint", "Lint it.", "from skill_dirs"),
            ("notes", "Taking notes", "yours"),
            ("release", "How this project releases.", "this project"),
        ]);
        // Without a project (a shell on another machine), only the user's own.
        let found = find(&yours, &[], None, &home);
        assert_eq!(found.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["notes", "release"]);
        assert_eq!(found[1].description, "How I release things.");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_skill_is_loaded_with_its_files_and_nothing_outside_it() {
        let root = scratch("loaded");
        let dir = skill(&root, "pdf", "---\nname: pdf\ndescription: d\n---\nRun scripts/fill.py on the form.\n");
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(dir.join("scripts/fill.py"), "print('filled')\n").unwrap();
        std::fs::write(dir.join("REFERENCE.md"), "fields\n").unwrap();
        std::fs::write(dir.join(".hidden"), "x").unwrap();
        std::fs::write(root.join("secret.txt"), "not the skill's").unwrap();
        let s = read(&dir, Source::Yours).unwrap();
        assert_eq!(files(&s), ["REFERENCE.md", "scripts/fill.py"]);
        let text = loaded(&s, Shell::Here).unwrap();
        assert!(text.starts_with(&format!("<skill name=\"pdf\" folder=\"{}\">\nRun scripts/fill.py on the form.\n</skill>", dir.display())), "{text}");
        assert!(text.contains("- scripts/fill.py") && text.contains("run a script from there with run_command"), "{text}");
        assert!(loaded(&s, Shell::FarWithAstrum).unwrap().contains("call use_skill with `file` and `copy`"));
        assert!(loaded(&s, Shell::Far).unwrap().contains("here-document"));
        assert_eq!(file(&s, "scripts/fill.py").unwrap().1, "print('filled')\n");
        for outside in ["../secret.txt", "/etc/hosts", "scripts/../../secret.txt", "nope.txt", "scripts"] {
            assert!(file(&s, outside).is_err(), "{outside}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_listing_the_model_is_given() {
        assert_eq!(listing(&[]), "");
        let s = Skill { name: "pdf".into(), description: "Fill in\nforms.".into(), dir: "/x".into(), source: Source::Project };
        let text = listing(&[s]);
        assert!(text.contains("call use_skill") && text.ends_with("- pdf: Fill in forms. (this project)\n"), "{text}");
    }
}
