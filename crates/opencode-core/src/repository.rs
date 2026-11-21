use regex::Regex;
use serde::{Deserialize, Serialize};

/// リモート／ファイル参照に共通するリポジトリ情報。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BaseReference {
    /// ホスト名（小文字化済み）。
    pub host: String,
    /// リポジトリのパス（`owner/repo` 形式）。
    pub path: String,
    /// パスを `/` で分割した各要素。
    pub segments: Vec<String>,
    /// セグメントが 2 つの場合の先頭要素（オーナー）。
    pub owner: Option<String>,
    /// リポジトリ名（パスの最後の要素）。
    pub repo: String,
    /// リモート URL。元入力に含まれる場合はその値、なければ生成した HTTPS URL。
    pub remote: String,
    /// 表示用のラベル。
    pub label: String,
}

/// リモートリポジトリへの参照。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteReference {
    /// リポジトリの共通情報（フラット化してシリアライズ）。
    #[serde(flatten)]
    pub base: BaseReference,
    /// 明示的に指定されたプロトコル。
    pub protocol: Option<String>,
}

/// ローカルファイルパスによるリポジトリ参照。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileReference {
    /// リポジトリの共通情報（フラット化してシリアライズ）。
    #[serde(flatten)]
    pub base: BaseReference,
    /// ファイル参照のプロトコル。
    pub protocol: String,
}

/// リポジトリ参照の種別。`serde(untagged)` により内容から判別される。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Reference {
    /// ローカルファイルによる参照。
    ///
    /// `FileReference.protocol` は必須の `String` であるため、先にマッチさせる。
    /// これにより File をシリアライズした JSON が Remote に化けるのを防ぐ。
    /// `protocol` が無い／`null` の JSON は File にマッチせず Remote へフォールバックする。
    File(FileReference),
    /// リモートリポジトリへの参照。
    Remote(RemoteReference),
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

/// `github:owner/repo`・SCP 形式の SSH・`owner/repo` などの文字列をリポジトリ参照に解析する。
///
/// ホスト名やパス要素に不正な文字が含まれるなど、安全でない入力は `None` を返す。
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

    // serde_json を dev-dependency に追加せずに JSON 相当のマップを組み立て、
    // untagged enum のバリアント選択を検証するための最小限の値表現。
    enum TestValue {
        Str(String),
        Null,
        Seq(Vec<TestValue>),
    }

    struct TestValueDeserializer(TestValue);

    impl<'de> serde::de::IntoDeserializer<'de, serde::de::value::Error> for TestValue {
        type Deserializer = TestValueDeserializer;

        fn into_deserializer(self) -> Self::Deserializer {
            TestValueDeserializer(self)
        }
    }

    impl<'de> serde::Deserializer<'de> for TestValueDeserializer {
        type Error = serde::de::value::Error;

        fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: serde::de::Visitor<'de>,
        {
            match self.0 {
                TestValue::Str(value) => visitor.visit_string(value),
                TestValue::Null => visitor.visit_unit(),
                TestValue::Seq(items) => {
                    visitor.visit_seq(serde::de::value::SeqDeserializer::new(items.into_iter()))
                }
            }
        }

        serde::forward_to_deserialize_any! {
            bool i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 char str string bytes
            byte_buf option unit unit_struct newtype_struct seq tuple tuple_struct
            map struct enum identifier ignored_any
        }
    }

    fn deserialize_reference(fields: Vec<(&'static str, TestValue)>) -> Reference {
        let map = fields
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect::<Vec<_>>();
        let de =
            serde::de::value::MapDeserializer::<_, serde::de::value::Error>::new(map.into_iter());
        Reference::deserialize(de).expect("reference JSON must deserialize")
    }

    fn base_fields() -> Vec<(&'static str, TestValue)> {
        vec![
            ("host", TestValue::Str("github.com".to_string())),
            ("path", TestValue::Str("anomalyco/opencode".to_string())),
            (
                "segments",
                TestValue::Seq(vec![
                    TestValue::Str("anomalyco".to_string()),
                    TestValue::Str("opencode".to_string()),
                ]),
            ),
            ("owner", TestValue::Str("anomalyco".to_string())),
            ("repo", TestValue::Str("opencode".to_string())),
            (
                "remote",
                TestValue::Str("https://github.com/anomalyco/opencode".to_string()),
            ),
            ("label", TestValue::Str("anomalyco/opencode".to_string())),
        ]
    }

    #[test]
    fn file_shaped_json_round_trips_as_file() {
        let mut fields = base_fields();
        fields.push(("protocol", TestValue::Str("file".to_string())));

        match deserialize_reference(fields) {
            Reference::File(file) => {
                assert_eq!(file.protocol, "file");
                assert_eq!(file.base.host, "github.com");
                assert_eq!(file.base.path, "anomalyco/opencode");
                assert_eq!(file.base.owner.as_deref(), Some("anomalyco"));
            }
            Reference::Remote(_) => {
                panic!("File-shaped JSON must stay File after a serialization round-trip")
            }
        }
    }

    #[test]
    fn remote_shaped_json_still_deserializes_as_remote() {
        let mut fields = base_fields();
        fields.push(("protocol", TestValue::Null));

        match deserialize_reference(fields) {
            Reference::Remote(remote) => {
                assert_eq!(remote.protocol, None);
                assert_eq!(remote.base.host, "github.com");
                assert_eq!(remote.base.path, "anomalyco/opencode");
                assert_eq!(remote.base.repo, "opencode");
            }
            Reference::File(_) => {
                panic!("Remote-shaped JSON must still deserialize as Remote")
            }
        }
    }
}
