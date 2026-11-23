use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::{PermissionRequest, Tool, ToolContext, ToolError, ToolResult};
use opencode_config::load_config;

pub struct SkillTool;

#[derive(Debug, Serialize, Deserialize)]
struct SkillInput {
    #[serde(rename = "skill_name", alias = "skillName", alias = "name")]
    skill_name: String,
    #[serde(default)]
    arguments: Option<serde_json::Value>,
    #[serde(default)]
    prompt: Option<String>,
}

#[derive(Debug, Clone)]
struct SkillInfo {
    name: String,
    description: String,
    content: String,
    location: PathBuf,
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).or({
        #[cfg(windows)]
        {
            std::env::var_os("USERPROFILE").map(PathBuf::from)
        }
        #[cfg(not(windows))]
        {
            None
        }
    })
}

fn resolve_skill_path(base: &Path, raw: &str) -> PathBuf {
    if let Some(stripped) = raw.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(stripped);
        }
    }

    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

fn collect_skill_roots(base: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = home_dir() {
        roots.push(home.join(".agents/skills"));
        roots.push(home.join(".claude/skills"));
    }

    // Global config directory (e.g. ~/.config/opencode/skills)
    if let Some(config_dir) = dirs::config_dir() {
        roots.push(config_dir.join("opencode/skill"));
        roots.push(config_dir.join("opencode/skills"));
    }

    // Home .opencode directory
    if let Some(home) = home_dir() {
        roots.push(home.join(".opencode/skill"));
        roots.push(home.join(".opencode/skills"));
    }

    roots.push(base.join(".agents/skills"));
    roots.push(base.join(".claude/skills"));
    roots.push(base.join(".opencode/skill"));
    roots.push(base.join(".opencode/skills"));

    if let Ok(config) = load_config(base) {
        if let Some(skills) = config.skills {
            for raw in skills.paths.into_inner() {
                roots.push(resolve_skill_path(base, &raw));
            }
            for raw in skills.urls.into_inner() {
                // RFC 3986 のスキームは大小文字を区別しない（R4）。
                let lowered = raw.to_ascii_lowercase();
                if lowered.starts_with("http://") || lowered.starts_with("https://") {
                    // リモート URL はローカルの探索ルートとして扱えないためスキップする。
                    // 値や URL はログに出さない（固定文言のみ）。
                    tracing::debug!("skipping remote URL in skills.urls");
                    continue;
                }
                // file:// はスキームを除いたパス部分をローカルルートとして扱う（R4）。
                // file://host/path の host は無視し、続くパス部分のみを使う。
                let path = if lowered.starts_with("file://") {
                    raw["file://".len()..]
                        .split_once('/')
                        .map(|(_, rest)| rest)
                        .unwrap_or("")
                } else {
                    raw.as_str()
                };
                if path.is_empty() {
                    continue;
                }
                roots.push(resolve_skill_path(base, path));
            }
        }
    }

    let mut deduped = Vec::new();
    for root in roots {
        if !deduped.contains(&root) {
            deduped.push(root);
        }
    }
    deduped
}

fn parse_frontmatter_value(frontmatter: &str, key: &str) -> Option<String> {
    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(value) = trimmed.strip_prefix(&format!("{key}:")) {
            let value = value.trim();
            if value.len() >= 2
                && ((value.starts_with('"') && value.ends_with('"'))
                    || (value.starts_with('\'') && value.ends_with('\'')))
            {
                return Some(value[1..value.len() - 1].to_string());
            }
            return Some(value.to_string());
        }
    }
    None
}

fn parse_skill_file(path: &Path) -> Option<SkillInfo> {
    let raw = fs::read_to_string(path).ok()?;
    // CRLF はまず LF へ寄せ、残った CR 単独（旧 Mac 形式）も LF として扱う。
    // これにより "---\rname: ...\r---" のような 1 行化を防ぐ。
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = normalized.lines();

    if lines.next()?.trim() != "---" {
        return None;
    }

    let mut frontmatter_lines = Vec::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line.trim() == "---" {
            closed = true;
            break;
        }
        frontmatter_lines.push(line);
    }
    if !closed {
        return None;
    }

    let frontmatter = frontmatter_lines.join("\n");
    let content = lines.collect::<Vec<_>>().join("\n");
    let name = parse_frontmatter_value(&frontmatter, "name")?;
    let description = parse_frontmatter_value(&frontmatter, "description")?;

    Some(SkillInfo {
        name,
        description,
        content: content.trim().to_string(),
        location: path.to_path_buf(),
    })
}

fn scan_skill_root(root: &Path) -> Vec<SkillInfo> {
    if !root.exists() || !root.is_dir() {
        return Vec::new();
    }

    // 外部スキルディレクトリの権限チェックは走査ルート root に対して行われる。
    // シンボリックリンクを辿ると、ルート内のリンクが外部の SKILL.md を指す場合に
    // そのチェックをすり抜けて外部のスキル定義を読み出してしまうため、辿らない。
    // follow_links(false) は symlink cycle も構造的に防ぐ。
    let mut skill_files: Vec<PathBuf> = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        // follow_links(false) でも WalkDir はリンク自身をエントリとして返す。
        // fs::read_to_string / path.is_file() はリンク先を辿るため、外部の
        // SKILL.md を読み込まないようリンクを明示的に除外する。
        .filter(|entry| !entry.file_type().is_symlink())
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.path().to_path_buf())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name == "SKILL.md")
                .unwrap_or(false)
        })
        .collect();
    skill_files.sort();

    skill_files
        .into_iter()
        .filter_map(|path| parse_skill_file(&path))
        .collect()
}

fn discover_skills(base: &Path) -> Vec<SkillInfo> {
    let mut by_name: HashMap<String, SkillInfo> = HashMap::new();
    for root in collect_skill_roots(base) {
        for skill in scan_skill_root(&root) {
            by_name.insert(skill.name.clone(), skill);
        }
    }

    let mut skills: Vec<SkillInfo> = by_name.into_values().collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

fn sample_skill_files(skill: &SkillInfo, limit: usize) -> Vec<PathBuf> {
    let Some(base_dir) = skill.location.parent() else {
        return Vec::new();
    };

    let mut files: Vec<PathBuf> = WalkDir::new(base_dir)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.path().to_path_buf())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name != "SKILL.md")
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    files.truncate(limit);
    files
}

#[async_trait]
impl Tool for SkillTool {
    fn id(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        "Load and execute a skill (predefined expertise module). Skills provide specialized knowledge for specific tasks."
    }

    fn parameters(&self) -> serde_json::Value {
        let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let skills = discover_skills(&base);
        let skill_names: Vec<String> = skills.into_iter().map(|s| s.name).collect();

        serde_json::json!({
            "type": "object",
            "properties": {
                "skill_name": {
                    "type": "string",
                    "description": "Name of the skill to load",
                    "enum": skill_names
                },
                "arguments": {
                    "type": "object",
                    "description": "Arguments to pass to the skill"
                },
                "prompt": {
                    "type": "string",
                    "description": "Additional prompt/instructions for the skill"
                }
            },
            "required": ["skill_name"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let input: SkillInput =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArguments(e.to_string()))?;

        let skills = discover_skills(Path::new(&ctx.directory));

        let skill = skills
            .iter()
            .find(|s| s.name == input.skill_name)
            .ok_or_else(|| {
                ToolError::InvalidArguments(format!(
                    "Unknown skill: {}. Available skills: {}",
                    input.skill_name,
                    skills
                        .iter()
                        .map(|s| &s.name)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;

        ctx.ask_permission(
            PermissionRequest::new("skill")
                .with_pattern(&skill.name)
                .with_always(&skill.name)
                .with_metadata("description", serde_json::json!(&skill.description)),
        )
        .await?;

        let mut output = format!("<skill_content name=\"{}\">\n\n", skill.name);
        output.push_str(&format!("# Skill: {}\n\n", skill.name));
        output.push_str(&skill.content);
        output.push_str("\n\n");
        output.push_str(&format!(
            "Base directory for this skill: {}\n",
            skill
                .location
                .parent()
                .unwrap_or(Path::new(&ctx.directory))
                .display()
        ));
        output.push_str(
            "Relative paths in this skill (e.g., scripts/, references/) are relative to this base directory.\n",
        );
        output.push_str("Note: file list is sampled.\n\n");

        let sampled_files = sample_skill_files(skill, 10);
        output.push_str("<skill_files>\n");
        for file in sampled_files {
            output.push_str(&format!("<file>{}</file>\n", file.display()));
        }
        output.push_str("</skill_files>\n");

        if let Some(ref args) = input.arguments {
            output.push_str(&format!(
                "**Arguments:**\n```json\n{}\n```\n\n",
                serde_json::to_string_pretty(args).unwrap_or_default()
            ));
        }

        if let Some(ref prompt) = input.prompt {
            output.push_str(&format!("**Additional Instructions:**\n{}\n\n", prompt));
        }

        output.push_str("\n</skill_content>");

        let mut metadata = std::collections::HashMap::new();
        metadata.insert("name".to_string(), serde_json::json!(&skill.name));
        metadata.insert(
            "dir".to_string(),
            serde_json::json!(skill
                .location
                .parent()
                .unwrap_or(Path::new(&ctx.directory))
                .to_string_lossy()
                .to_string()),
        );
        metadata.insert(
            "location".to_string(),
            serde_json::json!(skill.location.to_string_lossy().to_string()),
        );

        Ok(ToolResult {
            title: format!("Loaded skill: {}", skill.name),
            output,
            metadata,
            truncated: false,
        })
    }
}

impl Default for SkillTool {
    fn default() -> Self {
        Self
    }
}

pub fn list_available_skills() -> Vec<(String, String)> {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    discover_skills(&base)
        .into_iter()
        .map(|s| (s.name, s.description))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parse_skill_file_reads_frontmatter_and_body() {
        let dir = tempdir().unwrap();
        let skill_path = dir.path().join("SKILL.md");
        fs::write(
            &skill_path,
            r#"---
name: reviewer
description: "Review code changes"
---

# Reviewer

Do a thorough review.
"#,
        )
        .unwrap();

        let parsed = parse_skill_file(&skill_path).unwrap();
        assert_eq!(parsed.name, "reviewer");
        assert_eq!(parsed.description, "Review code changes");
        assert!(parsed.content.contains("Do a thorough review."));
    }

    #[test]
    fn parse_skill_file_reads_cr_only_line_endings() {
        let dir = tempdir().unwrap();
        let skill_path = dir.path().join("SKILL.md");
        // CR 単独（旧 Mac 形式）でも frontmatter が 1 行に化けないこと。
        fs::write(
            &skill_path,
            "---\rname: cr-skill\rdescription: CR only\r---\r\rbody line\r",
        )
        .unwrap();

        let parsed = parse_skill_file(&skill_path).unwrap();
        assert_eq!(parsed.name, "cr-skill");
        assert_eq!(parsed.description, "CR only");
        assert_eq!(parsed.content, "body line");
    }

    #[test]
    fn parse_skill_file_crlf_matches_lf() {
        let dir = tempdir().unwrap();
        let lf = "---\nname: same\ndescription: d\n---\nbody\n";
        let lf_path = dir.path().join("LF.md");
        let crlf_path = dir.path().join("CRLF.md");
        fs::write(&lf_path, lf).unwrap();
        fs::write(&crlf_path, lf.replace('\n', "\r\n")).unwrap();

        let from_lf = parse_skill_file(&lf_path).unwrap();
        let from_crlf = parse_skill_file(&crlf_path).unwrap();
        assert_eq!(from_lf.name, from_crlf.name);
        assert_eq!(from_lf.description, from_crlf.description);
        assert_eq!(from_lf.content, from_crlf.content);
    }

    #[test]
    fn discover_skills_loads_project_and_config_paths() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        let project_skill = root.join(".opencode/skills/local/SKILL.md");
        fs::create_dir_all(project_skill.parent().unwrap()).unwrap();
        fs::write(
            &project_skill,
            r#"---
name: local-skill
description: local
---
project content
"#,
        )
        .unwrap();

        let extra_root = root.join("custom-skills");
        let extra_skill = extra_root.join("remote/SKILL.md");
        fs::create_dir_all(extra_skill.parent().unwrap()).unwrap();
        fs::write(
            &extra_skill,
            r#"---
name: custom-skill
description: custom
---
custom content
"#,
        )
        .unwrap();

        fs::write(
            root.join("opencode.json"),
            r#"{
  "skills": {
    "paths": ["custom-skills"]
  }
}"#,
        )
        .unwrap();

        let discovered = discover_skills(root);
        let names: Vec<String> = discovered.into_iter().map(|s| s.name).collect();

        assert!(names.contains(&"local-skill".to_string()));
        assert!(names.contains(&"custom-skill".to_string()));
    }

    #[test]
    fn collect_skill_roots_includes_local_urls_from_config() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("opencode.json"),
            r#"{
  "skills": {
    "urls": ["url-skills"]
  }
}"#,
        )
        .unwrap();

        let roots = collect_skill_roots(root);
        assert!(roots.contains(&root.join("url-skills")));
    }

    #[test]
    fn collect_skill_roots_handles_file_scheme_and_uppercase_remote_scheme() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("opencode.json"),
            r#"{
  "skills": {
    "urls": ["file:///opt/shared/skills", "HTTPS://EXAMPLE.COM/skills"]
  }
}"#,
        )
        .unwrap();

        let roots = collect_skill_roots(root);
        assert!(
            roots.iter().any(|path| path.ends_with("opt/shared/skills")),
            "file:// URL must be resolved to its path component"
        );
        assert!(
            roots
                .iter()
                .all(|path| !path.to_string_lossy().contains("EXAMPLE.COM")),
            "uppercase remote scheme must also be skipped"
        );
    }

    #[test]
    fn collect_skill_roots_skips_remote_urls_from_config() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        fs::write(
            root.join("opencode.json"),
            r#"{
  "skills": {
    "urls": [
      "https://example.com/skills",
      "http://example.com/skills",
      "local-url-skills"
    ]
  }
}"#,
        )
        .unwrap();

        let roots = collect_skill_roots(root);
        assert!(roots.contains(&root.join("local-url-skills")));
        assert!(
            roots
                .iter()
                .all(|root| !root.to_string_lossy().contains("example.com")),
            "remote URLs must not become skill roots"
        );
    }

    #[test]
    fn parse_frontmatter_value_reads_quoted_and_plain() {
        let frontmatter = "name: plain\ndescription: \"quoted value\"\n# comment: ignored\n";
        assert_eq!(
            parse_frontmatter_value(frontmatter, "name").as_deref(),
            Some("plain")
        );
        assert_eq!(
            parse_frontmatter_value(frontmatter, "description").as_deref(),
            Some("quoted value")
        );
        assert!(parse_frontmatter_value(frontmatter, "missing").is_none());
    }

    #[test]
    fn parse_frontmatter_value_reads_single_quotes() {
        let frontmatter = "name: 'single'\n";
        assert_eq!(
            parse_frontmatter_value(frontmatter, "name").as_deref(),
            Some("single")
        );
    }

    #[test]
    fn resolve_skill_path_handles_absolute_and_relative() {
        let base = Path::new("/work/project");
        assert_eq!(
            resolve_skill_path(base, "/opt/skills"),
            PathBuf::from("/opt/skills")
        );
        assert_eq!(
            resolve_skill_path(base, "skills/local"),
            base.join("skills/local")
        );
    }

    #[test]
    fn discover_skills_sorts_by_name() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        for name in ["zeta", "alpha"] {
            let path = root.join(".opencode/skills").join(name).join("SKILL.md");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(
                &path,
                format!("---\nname: {}\ndescription: d\n---\nbody\n", name),
            )
            .unwrap();
        }

        let names: Vec<String> = discover_skills(root).into_iter().map(|s| s.name).collect();
        // Global skills from the home directory are also included, so only
        // assert that our two local skills are present in sorted order.
        let local: Vec<&String> = names
            .iter()
            .filter(|n| *n == "alpha" || *n == "zeta")
            .collect();
        assert_eq!(local, vec![&"alpha".to_string(), &"zeta".to_string()]);
    }

    #[test]
    fn identity_and_schema() {
        let tool = SkillTool;
        assert_eq!(tool.id(), "skill");
        assert_eq!(tool.parameters()["type"], "object");
    }

    #[test]
    fn skill_input_accepts_legacy_keys_and_normalizes() {
        for legacy in [
            serde_json::json!({ "skillName": "reviewer" }),
            serde_json::json!({ "name": "reviewer" }),
        ] {
            let input: SkillInput = serde_json::from_value(legacy).unwrap();
            assert_eq!(input.skill_name, "reviewer");

            let serialized = serde_json::to_value(&input).unwrap();
            assert_eq!(serialized["skill_name"], "reviewer");
            assert!(serialized.get("skillName").is_none());
            assert!(serialized.get("name").is_none());
        }
    }

    /// スキル走査ルート内の symlink が外部の SKILL.md や外部ディレクトリを
    /// 指しても、その内容を読み込んでしまってはならない
    /// （権限すり抜け回帰テスト）。
    #[cfg(unix)]
    #[test]
    fn scan_skill_root_does_not_follow_external_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let root = dir.path().join("skills");
        fs::create_dir_all(&root).unwrap();

        // 正規のローカルスキル。
        let local = root.join("local/SKILL.md");
        fs::create_dir_all(local.parent().unwrap()).unwrap();
        fs::write(&local, "---\nname: local\ndescription: d\n---\nbody\n").unwrap();

        // ルート外（兄弟ディレクトリ）に外部の SKILL.md を用意する。
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        let external = outside.join("SKILL.md");
        fs::write(
            &external,
            "---\nname: leaked\ndescription: d\n---\nTOP_SECRET\n",
        )
        .unwrap();

        // 外部ファイルへの直接 symlink と、外部ディレクトリへの symlink。
        let link_dir = root.join("linked");
        fs::create_dir_all(&link_dir).unwrap();
        symlink(&external, link_dir.join("SKILL.md")).unwrap();
        symlink(&outside, root.join("outside_link")).unwrap();

        let skills = scan_skill_root(&root);
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();

        // ローカルスキルのみが検出され、外部の "leaked" は漏れない。
        assert_eq!(names, vec!["local"]);
        assert!(!names.contains(&"leaked"));

        let contents: Vec<&str> = skills.iter().map(|s| s.content.as_str()).collect();
        assert!(contents.iter().all(|c| !c.contains("TOP_SECRET")));
    }
}
