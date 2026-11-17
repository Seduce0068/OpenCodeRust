use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaseReference {
    pub host: String,
    pub path: String,
    pub segments: Vec<String>,
    pub owner: Option<String>,
    pub repo: String,
    pub remote: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteReference {
    #[serde(flatten)]
    pub base: BaseReference,
    pub protocol: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileReference {
    #[serde(flatten)]
    pub base: BaseReference,
    pub protocol: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Reference {
    Remote(RemoteReference),
    File(FileReference),
}

fn normalize_input(input: &str) -> String {
    let re_git = Regex::new(r"^git\+").unwrap();
    let re_hash = Regex::new(r"#.*$").unwrap();
    let re_slash = Regex::new(r"/+$").unwrap();

    let cleaned = re_git.replace(input, "");
    let cleaned = re_hash.replace(&cleaned, "");
    re_slash.replace(&cleaned, "").trim().to_string()
}

fn trim_git_suffix(input: &str) -> String {
    input.trim_end_matches(".git").to_string()
}

fn get_parts(input: &str) -> Vec<String> {
    input
        .split('/')
        .map(|s| trim_git_suffix(s.trim()))
        .filter(|s| !s.is_empty())
        .collect()
}

fn safe_host(input: &str) -> bool {
    !input.is_empty() && !input.starts_with('-') && !Regex::new(r"[\s/\\]").unwrap().is_match(input)
}

fn safe_segment(input: &str) -> bool {
    input != "."
        && input != ".."
        && !input.contains(':')
        && !Regex::new(r"[\s/\\]").unwrap().is_match(input)
}

fn host_like(input: &str) -> bool {
    input.contains('.') || input.contains(':') || input == "localhost"
}

fn build_remote(
    host: &str,
    segments: Vec<String>,
    remote: Option<String>,
    protocol: Option<String>,
) -> Option<RemoteReference> {
    let segments: Vec<String> = segments
        .into_iter()
        .map(|s| trim_git_suffix(&s))
        .filter(|s| !s.is_empty())
        .collect();

    if !safe_host(host) || segments.is_empty() || segments.iter().any(|s| !safe_segment(s)) {
        return None;
    }

    let repository_path = segments.join("/");
    let host_lower = host.to_lowercase();
    let repo = segments.last().unwrap().clone();

    Some(RemoteReference {
        base: BaseReference {
            host: host_lower.clone(),
            path: repository_path.clone(),
            segments: segments.clone(),
            owner: if segments.len() == 2 {
                Some(segments[0].clone())
            } else {
                None
            },
            repo,
            remote: remote.unwrap_or_else(|| format!("https://{}/{}", host_lower, repository_path)),
            label: if host_lower == "github.com" && segments.len() == 2 {
                repository_path
            } else {
                format!("{}/{}", host_lower, repository_path)
            },
        },
        protocol,
    })
}

pub fn parse(input: &str) -> Option<Reference> {
    let cleaned = normalize_input(input);

    let re_github = Regex::new(r"^github:([^/\s]+)/([^/\s]+)$").unwrap();
    if let Some(caps) = re_github.captures(&cleaned) {
        return build_remote(
            "github.com",
            vec![caps[1].to_string(), caps[2].to_string()],
            None,
            None,
        )
        .map(Reference::Remote);
    }

    if !cleaned.contains("://") {
        let re_scp = Regex::new(r"^(?:[^@/\s]+@)?([^:/\s]+):(.+)$").unwrap();
        if let Some(caps) = re_scp.captures(&cleaned) {
            return build_remote(&caps[1], get_parts(&caps[2]), Some(cleaned.clone()), None)
                .map(Reference::Remote);
        }

        let direct = get_parts(&cleaned);
        if direct.len() >= 2 && host_like(&direct[0]) {
            let host = direct[0].clone();
            return build_remote(&host, direct[1..].to_vec(), None, None).map(Reference::Remote);
        }
        if direct.len() == 2 {
            return build_remote("github.com", direct, None, None).map(Reference::Remote);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_github_shorthand() {
        let reference = parse("github:anomalyco/opencode").unwrap();
        if let Reference::Remote(r) = reference {
            assert_eq!(r.base.host, "github.com");
            assert_eq!(r.base.path, "anomalyco/opencode");
            assert_eq!(r.base.owner, Some("anomalyco".to_string()));
            assert_eq!(r.base.repo, "opencode");
        } else {
            panic!("Expected remote reference");
        }
    }

    #[test]
    fn test_parse_ssh_scp() {
        let reference = parse("git@github.com:anomalyco/opencode.git").unwrap();
        if let Reference::Remote(r) = reference {
            assert_eq!(r.base.host, "github.com");
            assert_eq!(r.base.path, "anomalyco/opencode");
        } else {
            panic!("Expected remote reference");
        }
    }

    #[test]
    fn test_parse_direct() {
        let reference = parse("anomalyco/opencode").unwrap();
        if let Reference::Remote(r) = reference {
            assert_eq!(r.base.host, "github.com");
            assert_eq!(r.base.path, "anomalyco/opencode");
        } else {
            panic!("Expected remote reference");
        }
    }

    #[test]
    fn normalize_input_strips_git_hash_and_slashes() {
        assert_eq!(
            normalize_input("git+https://host/a///#frag"),
            "https://host/a"
        );
        assert_eq!(normalize_input("  github:a/b  "), "github:a/b");
    }

    #[test]
    fn trim_git_suffix_and_get_parts() {
        assert_eq!(trim_git_suffix("repo.git"), "repo");
        assert_eq!(trim_git_suffix("repo"), "repo");
        assert_eq!(get_parts(" owner / repo.git / "), vec!["owner", "repo"]);
    }

    #[test]
    fn safe_host_and_segment_reject_unsafe() {
        assert!(safe_host("github.com"));
        assert!(!safe_host(""));
        assert!(!safe_host("-evil"));
        assert!(!safe_host("bad host"));

        assert!(safe_segment("repo"));
        assert!(!safe_segment("."));
        assert!(!safe_segment(".."));
        assert!(!safe_segment("a:b"));
    }

    #[test]
    fn host_like_detects_hosts() {
        assert!(host_like("github.com"));
        assert!(host_like("host:22"));
        assert!(host_like("localhost"));
        assert!(!host_like("repo"));
    }

    #[test]
    fn parse_rejects_empty_and_unsafe() {
        assert!(parse("").is_none());
        assert!(parse("-bad").is_none());
    }
}
