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
//! repository or the home folder, whichever comes first, the closest last
//! (outside the home folder, with no repository, the working directory's
//! alone).
//!
//! What a project holds is whatever a repository someone else wrote says,
//! and skills are looked for while the server waits: what is read is a
//! plain file, in the skill's own folder, and only so much of it.

use std::path::{Component, Path, PathBuf};

/// How long a description may be, as the format has it.
const MAX_DESCRIPTION: usize = 1024;
/// How much of a `SKILL.md`, or of one of its files, is read.
const MAX_BODY: usize = 64 * 1024;
const MAX_FILE: usize = 256 * 1024;
/// How many files of a skill are listed, and how deep.
const MAX_FILES: usize = 200;
const MAX_DEPTH: usize = 4;
/// How many skills are offered at most from each of the places they are
/// found in (the user's own, `skill_dirs`, a project's): one with a great
/// many does not crowd out the others.
const MAX_SKILLS: usize = 100;

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

/// At most `limit` bytes of the file at `path`, and whether it holds more.
/// Only a plain file: a link in a repository may lead to a device
/// (`/dev/zero` never ends) or to a pipe (which waits for a writer).
fn read_capped(path: &Path, limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let plain = |m: std::fs::Metadata| if m.is_file() { Ok(()) } else { Err(std::io::Error::other("it is not a plain file")) };
    plain(std::fs::metadata(path)?)?;
    // Not held up by a pipe put in its place since.
    let f = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path)?;
    plain(f.metadata()?)?;
    let mut bytes = Vec::new();
    f.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    let more = bytes.len() > limit;
    bytes.truncate(limit);
    Ok((bytes, more))
}

/// The `SKILL.md` in `dir`: at most `MAX_BODY` of it, and whether it holds
/// more. It must be in the skill's own folder: one that is a link to a file
/// elsewhere (`~/.netrc`, say) is not read. (The whole folder may be a link.)
fn skill_md(dir: &Path) -> Result<(Vec<u8>, bool), String> {
    let root = dir.canonicalize().map_err(|e| e.to_string())?;
    let real = dir.join("SKILL.md").canonicalize().map_err(|e| e.to_string())?;
    if !real.starts_with(&root) {
        return Err("it is a link to a file outside the skill's folder".into());
    }
    read_capped(&real, MAX_BODY).map_err(|e| e.to_string())
}

/// The skill in `dir`, if it is one.
pub fn read(dir: &Path, source: Source) -> Option<Skill> {
    let (text, _) = skill_md(dir).ok()?;
    let text = String::from_utf8_lossy(&text).into_owned();
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

/// The skills in a folder of skills (so many, and no more are read).
fn in_folder(folder: &Path, source: Source) -> Vec<Skill> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(folder).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    dirs.iter().filter_map(|d| read(d, source.clone())).take(MAX_SKILLS).collect()
}

/// The folders of the project `cwd` is in, closest first: from `cwd` up to
/// the nearest that holds `.git`, or to the home folder, whichever comes
/// first. Outside the home folder, with no repository above, `cwd` alone: a
/// folder such as `/tmp` is anyone's, and what someone else put above it is
/// not this project's. (`rules::project_files` goes up the same way.)
fn project_levels(cwd: &Path, home: &Path) -> Vec<PathBuf> {
    let mut levels = Vec::new();
    for d in cwd.ancestors() {
        levels.push(d.to_path_buf());
        if d.join(".git").exists() || d == home {
            return levels;
        }
    }
    levels.truncate(1);
    levels
}

/// The folders a project's skills may be in, outermost first.
fn project_folders(cwd: &Path, home: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for d in project_levels(cwd, home) {
        for name in [".agents/skills", ".astrum/skills"] {
            let candidate = d.join(name);
            if candidate.is_dir() {
                found.push(candidate);
            }
        }
    }
    found.reverse();
    found
}

/// Skills, one of each name (of two, the later), in order of name.
fn by_name(all: impl IntoIterator<Item = Skill>) -> Vec<Skill> {
    let mut kept: Vec<Skill> = Vec::new();
    for skill in all {
        kept.retain(|k| k.name != skill.name);
        kept.push(skill);
    }
    kept.sort_by(|a, b| a.name.cmp(&b.name));
    kept
}

/// Every skill there is: the user's own, those in `named` folders, and,
/// with `project` (the shell's working directory, on this machine), the
/// project's. One of the same name later on takes the place of the earlier.
pub fn find(yours: &Path, named: &[PathBuf], project: Option<&Path>, home: &Path) -> Vec<Skill> {
    let mut places = vec![in_folder(yours, Source::Yours), named.iter().flat_map(|f| in_folder(f, Source::Named)).collect()];
    if let Some(cwd) = project.filter(|c| c.is_absolute()) {
        places.push(project_folders(cwd, home).iter().filter(|f| !f.starts_with(yours)).flat_map(|f| in_folder(f, Source::Project)).collect());
    }
    // So many from each place, so that a project with a great many does not
    // leave out the user's own.
    by_name(places.into_iter().flat_map(|place| by_name(place).into_iter().take(MAX_SKILLS)))
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
    let (text, cut) = skill_md(&skill.dir).map_err(|e| format!("its SKILL.md could not be read: {e}"))?;
    let text = String::from_utf8_lossy(&text).into_owned();
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

/// Where one of a skill's files is (`file` relative to its folder, as
/// listed), if it is one: its real path, and that path from the folder.
fn locate(skill: &Skill, file: &str) -> Result<(PathBuf, PathBuf), String> {
    let not_one = || format!("`{file}` is not one of the files of the skill `{}`", skill.name);
    // As listed, a file's path goes down from the folder and nowhere else:
    // not from the top, and not up (`..`, which a link inside the skill can
    // carry back into the folder here, but not on another machine).
    if !Path::new(file).components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir)) {
        return Err(not_one());
    }
    let real = skill.dir.join(file).canonicalize().map_err(|_| format!("the skill `{}` has no file `{file}`", skill.name))?;
    let root = skill.dir.canonicalize().map_err(|e| e.to_string())?;
    let from_root = real.strip_prefix(&root).map_err(|_| not_one())?.to_path_buf();
    if from_root.as_os_str().is_empty() || !std::fs::metadata(&real).is_ok_and(|m| m.is_file()) {
        return Err(not_one());
    }
    Ok((real, from_root))
}

/// One of a skill's files, to read: its real path, and what it holds (up
/// to 256 KB of it, with a note when there is more).
pub fn file(skill: &Skill, file: &str) -> Result<(PathBuf, String), String> {
    let (real, _) = locate(skill, file)?;
    let (bytes, cut) = read_capped(&real, MAX_FILE).map_err(|e| e.to_string())?;
    if bytes.iter().take(8192).any(|b| *b == 0) {
        return Err(format!("`{file}` is not text"));
    }
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if cut {
        text.push_str("\n[the rest was left out: the file is longer than 256 KB]");
    }
    Ok((real, text))
}

/// One of a skill's files, to put on another machine: its path from the
/// skill's folder, and what it holds, byte for byte. Never cut short or
/// changed, as what is read for the model may be: a script cut off part way
/// would still run there.
fn whole_file(skill: &Skill, file: &str) -> Result<(String, String), String> {
    let (real, from_root) = locate(skill, file)?;
    let from_root = from_root.to_str().ok_or_else(|| format!("`{file}` has a name that cannot be put on another machine"))?.to_string();
    let (bytes, cut) = read_capped(&real, MAX_FILE).map_err(|e| e.to_string())?;
    if cut {
        return Err(format!("`{file}` is longer than 256 KB: Astrum puts no file that large on another machine"));
    }
    // (What goes there is sent as text.)
    let text = String::from_utf8(bytes).map_err(|_| format!("`{file}` is not text: Astrum cannot put it on another machine"))?;
    Ok((from_root, text))
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
    if !call.bool_arg("copy") {
        return match file(skill, wanted) {
            Ok((path, text)) => (format!("[{}]\n{text}", path.display()), false),
            Err(why) => (format!("{why}."), true),
        };
    }
    let path = match locate(skill, wanted) {
        Ok((path, _)) => path,
        Err(why) => return (format!("{why}."), true),
    };
    match (shell, away) {
        (Shell::Here, _) => (format!("Your shell is on this machine: run it where it is, {}.", path.display()), false),
        (Shell::Far, _) | (_, None) => ("Astrum cannot put files on the machine your shell is on (it has nothing of its own there). Write it there with a here-document, from what use_skill with `file` gives.".into(), true),
        (Shell::FarWithAstrum, Some((way, home))) => {
            let (from_root, text) = match whole_file(skill, wanted) {
                Ok(found) => found,
                Err(why) => return (format!("{why}."), true),
            };
            // Where the file really is in the skill's folder, not the way the
            // model put it: that is what is the same on the other machine.
            let there = format!("{}/.cache/astrum/skills/{}/{from_root}", home.trim_end_matches('/'), skill.name);
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

    fn fifo(path: &Path) {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    }

    #[test]
    fn what_is_read_is_a_plain_file_in_the_skill_and_only_so_much() {
        use std::os::unix::fs::symlink;
        let root = scratch("plain");
        let folder = root.join("skills");
        std::fs::write(root.join("secret.md"), "---\nname: leak\ndescription: the first line of a secret\n---\n").unwrap();
        // A SKILL.md that never ends, one that waits for a writer, and one
        // that is another of the user's files: none of them is a skill, and
        // looking does not wait on them.
        for (name, target) in [("zero", Path::new("/dev/zero")), ("leak", &root.join("secret.md"))] {
            std::fs::create_dir_all(folder.join(name)).unwrap();
            symlink(target, folder.join(name).join("SKILL.md")).unwrap();
        }
        std::fs::create_dir_all(folder.join("pipe")).unwrap();
        fifo(&folder.join("pipe/SKILL.md"));
        // A link to a file in its own folder is its own.
        let inner = folder.join("inner");
        std::fs::create_dir_all(inner.join("docs")).unwrap();
        std::fs::write(inner.join("docs/skill.md"), "---\nname: inner\ndescription: Linked within.\n---\n").unwrap();
        symlink("docs/skill.md", inner.join("SKILL.md")).unwrap();
        let big = skill(&folder, "big", &format!("---\nname: big\ndescription: Long.\n---\n{}", "x".repeat(MAX_BODY * 4)));

        let found = find(&folder, &[], None, &root);
        assert_eq!(found.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["big", "inner"]);
        for name in ["zero", "leak", "pipe"] {
            let s = Skill { name: name.into(), description: String::new(), dir: folder.join(name), source: Source::Project };
            assert!(loaded(&s, Shell::Here).is_err(), "{name}");
        }
        let text = loaded(&found[0], Shell::Here).unwrap();
        assert!(text.contains("[the rest of SKILL.md was left out") && text.len() < MAX_BODY + 1000);

        // Its files: a device or a pipe is not one of them; a long one is read so far.
        symlink("/dev/zero", big.join("zero")).unwrap();
        fifo(&big.join("pipe"));
        std::fs::write(big.join("long.txt"), "y".repeat(MAX_FILE * 4)).unwrap();
        assert!(file(&found[0], "zero").is_err() && file(&found[0], "pipe").is_err());
        let (_, text) = file(&found[0], "long.txt").unwrap();
        assert!(text.ends_with("[the rest was left out: the file is longer than 256 KB]") && text.len() < MAX_FILE + 100);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn outside_the_home_folder_only_a_repository_is_a_project() {
        let root = scratch("outside");
        // (A scratch folder inside a repository cannot show this.)
        if root.ancestors().skip(1).any(|d| d.join(".git").exists()) {
            return;
        }
        let (yours, home) = (root.join("cfg/skills"), root.join("home"));
        skill(&yours, "release", "---\nname: release\ndescription: Mine.\n---\n");
        // Someone else's, in a folder anyone may write to.
        let shared = root.join("tmp");
        skill(&shared.join(".agents/skills"), "release", "---\nname: release\ndescription: Planted.\n---\n");
        let work = shared.join("work");
        skill(&work.join(".agents/skills"), "here", "---\nname: here\ndescription: In the folder itself.\n---\n");
        let found = find(&yours, &[], Some(&work), &home);
        let shown: Vec<(&str, &str)> = found.iter().map(|s| (s.name.as_str(), s.description.as_str())).collect();
        assert_eq!(shown, [("here", "In the folder itself."), ("release", "Mine.")]);
        assert_eq!(find(&yours, &[], Some(&work.join("deeper")), &home).len(), 1, "a folder below has none of its own");
        // A repository there is a project, up to its top.
        let repo = shared.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        skill(&repo.join(".agents/skills"), "lint", "---\nname: lint\ndescription: The repository's.\n---\n");
        let found = find(&yours, &[], Some(&repo.join("src")), &home);
        let shown: Vec<(&str, &str)> = found.iter().map(|s| (s.name.as_str(), s.description.as_str())).collect();
        assert_eq!(shown, [("lint", "The repository's."), ("release", "Mine.")]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_file_put_on_another_machine_is_where_it_really_is_and_whole() {
        use std::os::unix::fs::symlink;
        let root = scratch("whole");
        let dir = skill(&root, "deploy", "---\nname: deploy\ndescription: d\n---\n");
        std::fs::create_dir_all(dir.join("a/b/c/d/e")).unwrap();
        std::fs::write(dir.join("a/b/c/d/e/run.sh"), "echo héllo ✓\n").unwrap();
        symlink("a/b/c/d/e", dir.join("lib")).unwrap();
        let s = read(&dir, Source::Yours).unwrap();
        // Through the link, the first is inside the skill here; there, its
        // `..` would go up from the folder it is put in, to the home folder.
        for up in ["lib/../../../../../SKILL.md", "lib/../../../../../../.bashrc", "/etc/hosts"] {
            assert!(whole_file(&s, up).unwrap_err().contains("is not one of the files"), "{up}");
            assert!(file(&s, up).is_err(), "{up}");
        }
        assert_eq!(whole_file(&s, "lib/run.sh").unwrap(), ("a/b/c/d/e/run.sh".to_string(), "echo héllo ✓\n".to_string()));
        assert_eq!(whole_file(&s, "./lib/run.sh").unwrap().0, "a/b/c/d/e/run.sh");
        // Too long to put there whole: not put there cut short.
        std::fs::write(dir.join("big.sh"), "#".repeat(MAX_FILE + 1)).unwrap();
        assert!(whole_file(&s, "big.sh").unwrap_err().contains("longer than 256 KB"));
        assert!(file(&s, "big.sh").is_ok(), "it can still be read, so far");
        std::fs::write(dir.join("bytes.bin"), [0xffu8, 0xfe, b'\n']).unwrap();
        assert!(whole_file(&s, "bytes.bin").unwrap_err().contains("is not text"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_project_with_a_great_many_skills_does_not_crowd_out_the_users_own() {
        let root = scratch("many");
        let (yours, home) = (root.join("cfg/skills"), root.join("home"));
        skill(&yours, "zz-mine", "---\nname: zz-mine\ndescription: Mine.\n---\n");
        skill(&yours, "0050", "---\nname: \"0050\"\ndescription: Mine too.\n---\n");
        let project = home.join("proj");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        for i in 1..=MAX_SKILLS + 20 {
            let name = format!("{i:04}");
            skill(&project.join(".agents/skills"), &name, &format!("---\nname: \"{name}\"\ndescription: Theirs.\n---\n"));
        }
        let found = find(&yours, &[], Some(&project), &home);
        assert_eq!(found.len(), MAX_SKILLS + 1);
        let by = |n: &str| found.iter().find(|s| s.name == n).map(|s| s.source.label());
        assert_eq!((by("zz-mine"), by("0050"), by("0100"), by("0101")), (Some("yours"), Some("this project"), Some("this project"), None));
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
