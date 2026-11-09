//! Skill discovery: folders containing a `SKILL.md` with YAML frontmatter.
//!
//! Sources, in order (first one wins when the same directory is reachable twice):
//! 1. Boba-managed skills: `<boba_skills_dir>/<name>/SKILL.md` (editable from the UI)
//! 2. Project skills: `<repo>/.claude/skills/<name>/SKILL.md`
//! 3. User skills: `$CLAUDE_CONFIG_DIR/skills` or `~/.claude/skills`
//! 4. Plugin skills: `<installPath>/skills/<name>/SKILL.md` for every plugin listed in
//!    `~/.claude/plugins/installed_plugins.json`, named `<plugin>:<name>` like Claude Code does.
//!
//! Ids are UUIDv5 of the canonical absolute path, so they are stable across restarts and
//! machines that share a layout, and never collide between sources.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use boba_core::{Id, Skill, SkillSource};
use uuid::Uuid;

/// Namespace for skill ids (UUIDv5 over the skill directory's absolute path).
pub const SKILL_NAMESPACE: Uuid = Uuid::from_u128(0x6b6f_6261_2d73_6b69_6c6c_2d6e_735f_7631);

pub fn skill_id(abs_path: &Path) -> Id {
    Uuid::new_v5(&SKILL_NAMESPACE, abs_path.to_string_lossy().as_bytes())
}

/// The Claude Code config dir of the user running the server.
pub fn claude_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude"))
}

/// Blocking filesystem scan; call from `spawn_blocking`.
pub fn discover(
    repo_paths: &[String],
    boba_skills_dir: &str,
    claude_home: Option<&Path>,
) -> Vec<Skill> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();

    scan_dir(
        Path::new(boba_skills_dir),
        SkillSource::Boba,
        None,
        &mut seen,
        &mut found,
    );
    for repo in repo_paths {
        let dir = Path::new(repo).join(".claude").join("skills");
        scan_dir(&dir, SkillSource::Project, None, &mut seen, &mut found);
    }
    if let Some(home) = claude_home {
        scan_dir(
            &home.join("skills"),
            SkillSource::User,
            None,
            &mut seen,
            &mut found,
        );
        for (plugin, install) in installed_plugins(home) {
            scan_dir(
                &install.join("skills"),
                SkillSource::Plugin,
                Some(&plugin),
                &mut seen,
                &mut found,
            );
        }
    }
    found
}

fn scan_dir(
    dir: &Path,
    source: SkillSource,
    namespace: Option<&str>,
    seen: &mut HashSet<PathBuf>,
    out: &mut Vec<Skill>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.join("SKILL.md").is_file())
        .collect();
    dirs.sort();
    for path in dirs {
        let abs = std::fs::canonicalize(&path).unwrap_or(path);
        if !seen.insert(abs.clone()) {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(abs.join("SKILL.md")) else {
            continue;
        };
        let fm = parse_frontmatter(&raw);
        let dir_name = abs
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = fm.name.filter(|n| !n.is_empty()).unwrap_or(dir_name);
        let name = match namespace {
            Some(ns) => format!("{ns}:{name}"),
            None => name,
        };
        out.push(Skill {
            id: skill_id(&abs),
            name,
            description: fm.description.unwrap_or_default(),
            source,
            path: abs.to_string_lossy().into_owned(),
            editable: source == SkillSource::Boba,
        });
    }
}

/// `(plugin name, install path)` from `installed_plugins.json` (v1 and v2 layouts).
fn installed_plugins(home: &Path) -> Vec<(String, PathBuf)> {
    let Ok(raw) = std::fs::read_to_string(home.join("plugins").join("installed_plugins.json"))
    else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    let Some(plugins) = json.get("plugins").and_then(|p| p.as_object()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, val) in plugins {
        let plugin = key.split('@').next().unwrap_or(key).to_string();
        // v2: array of installs; v1: a single object.
        let installs: Vec<&serde_json::Value> = match val {
            serde_json::Value::Array(a) => a.iter().collect(),
            v => vec![v],
        };
        for inst in installs {
            if let Some(p) = inst.get("installPath").and_then(|p| p.as_str()) {
                out.push((plugin.clone(), PathBuf::from(p)));
            }
        }
    }
    out.sort();
    out
}

#[derive(Debug, Default, PartialEq)]
pub struct Frontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
}

/// Minimal YAML frontmatter reader for the two keys we need. Handles plain, single- and
/// double-quoted scalars, `|` / `>` block scalars and plain multi-line continuation.
pub fn parse_frontmatter(raw: &str) -> Frontmatter {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let mut lines = raw.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return Frontmatter::default();
    }
    let body: Vec<&str> = lines.take_while(|l| l.trim_end() != "---").collect();
    let mut fm = Frontmatter::default();
    let mut i = 0;
    while i < body.len() {
        let line = body[i];
        i += 1;
        if line.starts_with([' ', '\t']) || line.trim_start().starts_with('#') {
            continue;
        }
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        if key != "name" && key != "description" {
            continue;
        }
        let rest = rest.trim();
        // Collect indented continuation lines.
        let mut cont = Vec::new();
        while i < body.len() && (body[i].starts_with([' ', '\t']) || body[i].trim().is_empty()) {
            cont.push(body[i]);
            i += 1;
        }
        while cont.last().is_some_and(|l| l.trim().is_empty()) {
            cont.pop();
        }
        let value = scalar(rest, &cont);
        match key {
            "name" => fm.name = Some(value),
            _ => fm.description = Some(value),
        }
    }
    fm
}

fn scalar(first: &str, cont: &[&str]) -> String {
    let first = strip_comment(first);
    if first.starts_with('|') || first.starts_with('>') {
        let indent = cont
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.len() - l.trim_start().len())
            .min()
            .unwrap_or(0);
        let lines: Vec<&str> = cont
            .iter()
            .map(|l| l.get(indent..).unwrap_or("").trim_end())
            .collect();
        return if first.starts_with('|') {
            lines.join("\n").trim().to_string()
        } else {
            fold(&lines)
        };
    }
    let mut parts = vec![first.to_string()];
    parts.extend(cont.iter().map(|l| l.trim().to_string()));
    let joined = fold(&parts.iter().map(String::as_str).collect::<Vec<_>>());
    unquote(&joined)
}

/// YAML folding: single newlines become spaces, blank lines become newlines.
fn fold(lines: &[&str]) -> String {
    let mut out = String::new();
    for l in lines {
        let l = l.trim();
        if l.is_empty() {
            out.push('\n');
        } else {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push(' ');
            }
            out.push_str(l);
        }
    }
    out.trim().to_string()
}

fn strip_comment(s: &str) -> &str {
    if s.starts_with(['"', '\'']) {
        return s;
    }
    match s.find(" #") {
        Some(i) => s[..i].trim_end(),
        None => s,
    }
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        let inner = &s[1..s.len() - 1];
        let mut out = String::new();
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some(o) => out.push(o),
                    None => {}
                }
            } else {
                out.push(c);
            }
        }
        out
    } else if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        s[1..s.len() - 1].replace("''", "'")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_variants() {
        let fm = parse_frontmatter(
            "---\nname: deploy\ndescription: Ship it. Use when: deploying\n---\n# body",
        );
        assert_eq!(fm.name.as_deref(), Some("deploy"));
        assert_eq!(
            fm.description.as_deref(),
            Some("Ship it. Use when: deploying")
        );

        let fm = parse_frontmatter("---\nname: \"q\\\"x\"\ndescription: 'it''s'\n---\n");
        assert_eq!(fm.name.as_deref(), Some("q\"x"));
        assert_eq!(fm.description.as_deref(), Some("it's"));

        let fm = parse_frontmatter(
            "---\nname: a\ndescription: >\n  line one\n  line two\n\n  para\nlicense: MIT\n---\n",
        );
        assert_eq!(fm.description.as_deref(), Some("line one line two\npara"));

        let fm = parse_frontmatter("---\ndescription: |\n  keep\n  lines\nname: b\n---\n");
        assert_eq!(fm.description.as_deref(), Some("keep\nlines"));
        assert_eq!(fm.name.as_deref(), Some("b"));

        let fm =
            parse_frontmatter("---\nname: c # comment\ndescription: plain\n  continued\n---\n");
        assert_eq!(fm.name.as_deref(), Some("c"));
        assert_eq!(fm.description.as_deref(), Some("plain continued"));

        assert_eq!(
            parse_frontmatter("# no frontmatter"),
            Frontmatter::default()
        );
    }

    #[test]
    fn discovers_all_sources_with_stable_ids() {
        let tmp = std::env::temp_dir().join(format!("boba-skills-{}", Uuid::now_v7()));
        let mk = |dir: PathBuf, name: &str| {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: d {name}\n---\n"),
            )
            .unwrap();
        };
        let boba = tmp.join("data/skills");
        let repo = tmp.join("repo");
        let home = tmp.join("home/.claude");
        let plugin = tmp.join("plugin-install");
        mk(boba.join("mine"), "mine");
        mk(repo.join(".claude/skills/proj"), "proj");
        mk(home.join("skills/user"), "user");
        mk(plugin.join("skills/deploy"), "deploy");
        std::fs::create_dir_all(home.join("skills/not-a-skill")).unwrap();
        std::fs::create_dir_all(home.join("plugins")).unwrap();
        std::fs::write(
            home.join("plugins/installed_plugins.json"),
            serde_json::json!({"version": 2, "plugins": {"vercel@official": [{"installPath": plugin}]}}).to_string(),
        )
        .unwrap();

        let repos = vec![repo.to_string_lossy().into_owned()];
        let skills = discover(&repos, &boba.to_string_lossy(), Some(&home));
        let names: Vec<(&str, SkillSource, bool)> = skills
            .iter()
            .map(|s| (s.name.as_str(), s.source, s.editable))
            .collect();
        assert_eq!(
            names,
            vec![
                ("mine", SkillSource::Boba, true),
                ("proj", SkillSource::Project, false),
                ("user", SkillSource::User, false),
                ("vercel:deploy", SkillSource::Plugin, false),
            ]
        );
        let again = discover(&repos, &boba.to_string_lossy(), Some(&home));
        assert_eq!(skills[0].id, again[0].id);
        assert_ne!(skills[0].id, skills[1].id);
        // Same dir reachable twice (repo passed twice) is listed once.
        let twice = discover(&[repos[0].clone(), repos[0].clone()], "/nonexistent", None);
        assert_eq!(twice.len(), 1);
        std::fs::remove_dir_all(&tmp).ok();
    }
}
