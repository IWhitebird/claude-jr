// Skills: folders with a SKILL.md (the same format Claude Code uses).
//
//   ---
//   name: release
//   description: Cut a release: bump the version, tag, write notes.
//   ---
//   Steps...
//
// Looked for in ./.claude/skills, ./.claude-jr/skills, ~/.claude/skills and
// ~/.config/claude-jr/skills; the first skill with a name wins. Only names and
// descriptions are sent up front (in the skill tool's description); the body
// is loaded when the model calls the tool or the user types /name.

use super::{Output, def};
use serde_json::value::RawValue;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const MAX_DESCRIPTION: usize = 300;

pub struct Skill {
    pub name: String,
    pub description: String,
    file: PathBuf,
}

pub fn discover() -> Vec<Skill> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let roots = [
        cwd.join(".claude/skills"),
        cwd.join(".claude-jr/skills"),
        home.join(".claude/skills"),
        crate::config::config_dir().join("skills"),
    ];
    let mut skills: Vec<Skill> = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else { continue };
        let mut dirs: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        dirs.sort();
        for dir in dirs {
            let file = dir.join("SKILL.md");
            let Some(skill) = parse(&file) else { continue };
            if !skills.iter().any(|s| s.name == skill.name) {
                skills.push(skill);
            }
        }
    }
    skills
}

fn parse(file: &Path) -> Option<Skill> {
    let text = std::fs::read_to_string(file).ok()?;
    let (front, _) = split_front_matter(&text)?;
    let field = |key: &str| {
        front.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == key).then(|| v.trim().trim_matches(['"', '\'']).to_string())
        })
    };
    let fallback = file.parent()?.file_name()?.to_string_lossy().into_owned();
    let name = field("name").filter(|n| !n.is_empty()).unwrap_or(fallback);
    let mut description = field("description").unwrap_or_default();
    if description.len() > MAX_DESCRIPTION {
        let cut = (0..=MAX_DESCRIPTION).rev().find(|&i| description.is_char_boundary(i)).unwrap_or(0);
        description.truncate(cut);
        description.push('…');
    }
    Some(Skill { name, description, file: file.to_path_buf() })
}

/// (front matter, body). Files without front matter are all body.
fn split_front_matter(text: &str) -> Option<(&str, &str)> {
    let Some(rest) = text.strip_prefix("---") else { return Some(("", text)) };
    let end = rest.find("\n---")?;
    let body = rest[end + 4..].trim_start_matches(['-', '\r', '\n']);
    Some((&rest[..end], body))
}

pub fn definition(skills: &[Skill]) -> Box<RawValue> {
    let list: String = skills.iter().map(|s| format!("\n- {}: {}", s.name, s.description)).collect();
    def(json!({
        "name": "skill",
        "description": format!(
            "Load a skill: instructions for a specific kind of task. When a task matches one of these, \
             load it first and follow it.{list}"
        ),
        "input_schema": {
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"]
        }
    }))
}

/// The skill's instructions, with its folder so relative paths in it work.
pub fn body(skill: &Skill) -> Result<String, String> {
    let text = std::fs::read_to_string(&skill.file).map_err(|e| format!("{}: {e}", skill.file.display()))?;
    let body = split_front_matter(&text).map_or(text.as_str(), |(_, b)| b);
    let dir = skill.file.parent().map(|d| d.display().to_string()).unwrap_or_default();
    Ok(format!("Skill \"{}\" (files in {dir}):\n\n{}", skill.name, body.trim()))
}

pub fn load(skills: &[Skill], input: &Value) -> Output {
    let name = input["name"].as_str().ok_or("missing name")?;
    let skill = skills.iter().find(|s| s.name == name).ok_or_else(|| format!("no skill named {name}"))?;
    body(skill)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_front_matter() {
        let dir = super::super::test_dir("skill").join("release");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("SKILL.md");
        std::fs::write(&file, "---\nname: ship\ndescription: \"Cut a release\"\n---\n\nBump the version.\n").unwrap();
        let skill = parse(&file).unwrap();
        assert_eq!((skill.name.as_str(), skill.description.as_str()), ("ship", "Cut a release"));
        assert!(body(&skill).unwrap().ends_with("Bump the version."));

        std::fs::write(&file, "Just steps.\n").unwrap();
        let skill = parse(&file).unwrap();
        assert_eq!((skill.name.as_str(), skill.description.as_str()), ("release", ""));
    }

    fn skill_in(dir: &Path, folder: &str, text: &str) -> PathBuf {
        let d = dir.join(folder);
        std::fs::create_dir_all(&d).unwrap();
        let file = d.join("SKILL.md");
        std::fs::write(&file, text).unwrap();
        file
    }

    #[test]
    fn loading_a_skill_by_name() {
        let dir = super::super::test_dir("skill-load");
        let a = parse(&skill_in(&dir, "alpha", "---\nname: alpha\ndescription: first\n---\nDo A.\n")).unwrap();
        let b = parse(&skill_in(&dir, "beta", "Just B.\n")).unwrap();
        let skills = [a, b];

        let out = load(&skills, &json!({"name": "alpha"})).unwrap();
        assert_eq!(out, format!("Skill \"alpha\" (files in {}):\n\nDo A.", dir.join("alpha").display()));
        assert!(
            load(&skills, &json!({"name": "beta"})).unwrap().ends_with("Just B."),
            "the folder name is the default name"
        );
        assert_eq!(load(&skills, &json!({"name": "gamma"})).unwrap_err(), "no skill named gamma");
        assert_eq!(load(&skills, &json!({})).unwrap_err(), "missing name");

        let def: Value = serde_json::from_str(definition(&skills).get()).unwrap();
        let text = def["description"].as_str().unwrap();
        assert!(text.contains("\n- alpha: first\n- beta: "), "{text}");
        assert_eq!(def["name"], "skill");

        // A file that vanished is an error naming it.
        std::fs::remove_file(dir.join("alpha/SKILL.md")).unwrap();
        let err = load(&skills, &json!({"name": "alpha"})).unwrap_err();
        assert!(err.contains("alpha/SKILL.md"), "{err}");
    }

    #[test]
    fn front_matter_edge_cases() {
        let dir = super::super::test_dir("skill-front");
        // Unterminated front matter: not a skill.
        assert!(parse(&skill_in(&dir, "broken", "---\nname: x\nno end here\n")).is_none());
        // A blank name falls back to the folder name; single quotes are stripped.
        let s = parse(&skill_in(&dir, "folder", "---\nname:\ndescription: 'quoted: yes'\n---\nbody")).unwrap();
        assert_eq!((s.name.as_str(), s.description.as_str()), ("folder", "quoted: yes"));
        // Long descriptions are cut on a character boundary and marked.
        let long = "\u{e9}".repeat(400);
        let s = parse(&skill_in(&dir, "long", &format!("---\ndescription: {long}\n---\nbody"))).unwrap();
        assert_eq!(s.description, format!("{}\u{2026}", "\u{e9}".repeat(150)));
        assert!(parse(&dir.join("nothing/SKILL.md")).is_none());
        assert_eq!(split_front_matter("---\na: b\n---\n\n\nBody"), Some(("\na: b", "Body")));
        assert_eq!(split_front_matter("plain"), Some(("", "plain")));
    }

    #[test]
    fn skills_are_found_in_the_config_folder_and_the_first_name_wins() {
        super::super::test_home();
        let root = crate::config::config_dir().join("skills");
        skill_in(&root, "unit-first", "---\nname: unit-dup\ndescription: from config\n---\nA");
        skill_in(&root, "unit-second", "---\nname: unit-dup\ndescription: shadowed\n---\nB");
        skill_in(&root, "unit-not-a-skill", "---\nname: unit-bad\n");
        std::fs::write(root.join("stray-file.md"), "x").unwrap();
        let found: Vec<(String, String)> = discover()
            .into_iter()
            .filter(|s| s.name == "unit-dup" || s.name == "unit-bad")
            .map(|s| (s.name, s.description))
            .collect();
        assert_eq!(found, [("unit-dup".to_string(), "from config".to_string())]);
    }
}
