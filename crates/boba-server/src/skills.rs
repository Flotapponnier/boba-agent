//! Skills: Boba-managed ones live in `<data>/skills/<slug>/SKILL.md` and are editable;
//! the runner discovers the rest (user / project / plugin), which are read-only.

use std::path::{Path, PathBuf};

use anyhow::Context;
use boba_core::{Skill, SkillSource};

use crate::db::new_id;
use crate::state::SharedState;

/// Filesystem-safe folder name for a skill.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "skill".into()
    } else {
        out
    }
}

fn yaml_escape(s: &str) -> String {
    let one_line = s.replace(['\n', '\r'], " ");
    format!(
        "\"{}\"",
        one_line.replace('\\', "\\\\").replace('"', "\\\"")
    )
}

pub fn render_skill_md(name: &str, description: &str, body: &str) -> String {
    format!(
        "---\nname: {}\ndescription: {}\n---\n\n{}\n",
        yaml_escape(name),
        yaml_escape(description),
        body.trim()
    )
}

/// Split SKILL.md into (name, description, body). Tolerant of quoting styles.
pub fn parse_skill_md(text: &str) -> (Option<String>, Option<String>, String) {
    let text = text.trim_start_matches('\u{feff}');
    let Some(rest) = text.strip_prefix("---") else {
        return (None, None, text.to_string());
    };
    let Some(end) = rest.find("\n---") else {
        return (None, None, text.to_string());
    };
    let front = &rest[..end];
    let body = rest[end + 4..].trim_start_matches(['-']).trim().to_string();
    let mut name = None;
    let mut description = None;
    for line in front.lines() {
        if let Some((k, v)) = line.split_once(':') {
            let v = unquote(v.trim());
            match k.trim() {
                "name" => name = Some(v),
                "description" => description = Some(v),
                _ => {}
            }
        }
    }
    (name, description, body)
}

fn unquote(v: &str) -> String {
    if v.len() >= 2
        && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')))
    {
        v[1..v.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
    } else {
        v.to_string()
    }
}

/// Write (create or overwrite) a Boba skill folder; returns its absolute path.
pub fn write_boba_skill(
    dir: &Path,
    name: &str,
    description: &str,
    body: &str,
) -> anyhow::Result<PathBuf> {
    let folder = dir.join(slug(name));
    std::fs::create_dir_all(&folder).with_context(|| format!("creating {}", folder.display()))?;
    std::fs::write(
        folder.join("SKILL.md"),
        render_skill_md(name, description, body),
    )?;
    Ok(absolute(&folder))
}

fn absolute(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Boba skills found on disk.
pub fn scan_boba_dir(dir: &Path) -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        let md = path.join("SKILL.md");
        let Ok(text) = std::fs::read_to_string(&md) else {
            continue;
        };
        let (name, description, _) = parse_skill_md(&text);
        let fallback = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        out.push(Skill {
            id: new_id(),
            name: name.unwrap_or(fallback),
            description: description.unwrap_or_default(),
            source: SkillSource::Boba,
            path: absolute(&path).to_string_lossy().to_string(),
            editable: true,
        });
    }
    out
}

/// Re-discover every skill, upsert them (ids are stable per path), prune vanished ones.
pub async fn rescan(state: &SharedState) -> anyhow::Result<Vec<Skill>> {
    let dir = state.cfg.skills_dir();
    std::fs::create_dir_all(&dir).ok();
    let mut found = scan_boba_dir(&dir);
    let repo_paths: Vec<String> = state
        .db
        .repos()
        .await?
        .into_iter()
        .map(|r| r.path)
        .collect();
    match state
        .runner
        .discover_skills(&repo_paths, &dir.to_string_lossy())
        .await
    {
        Ok(more) => {
            for sk in more {
                if !found.iter().any(|f| f.path == sk.path) {
                    found.push(sk);
                }
            }
        }
        Err(e) => tracing::warn!(error = ?e, "skill discovery failed"),
    }
    let paths: Vec<String> = found.iter().map(|s| s.path.clone()).collect();
    for sk in &found {
        state.db.upsert_skill(sk).await?;
    }
    state.db.prune_skills(&paths).await?;
    state.db.skills().await
}

/// Files of a skill folder (relative paths), SKILL.md excluded.
pub fn list_files(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>, depth: usize) {
        if depth > 4 || out.len() > 200 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(base, &p, out, depth + 1);
            } else if let Ok(rel) = p.strip_prefix(base) {
                let rel = rel.to_string_lossy().to_string();
                if rel != "SKILL.md" {
                    out.push(rel);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out, 0);
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_frontmatter() {
        let md = render_skill_md("Code \"review\"", "Reviews PRs: carefully", "# Hi\nbody");
        let (n, d, b) = parse_skill_md(&md);
        assert_eq!(n.as_deref(), Some("Code \"review\""));
        assert_eq!(d.as_deref(), Some("Reviews PRs: carefully"));
        assert_eq!(b, "# Hi\nbody");
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("  SEO Audit! "), "seo-audit");
        assert_eq!(slug("!!!"), "skill");
    }
}
