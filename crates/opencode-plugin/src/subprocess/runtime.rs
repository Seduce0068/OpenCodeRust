//! JS runtime detection — finds bun, deno, or node on `$PATH`.

use std::path::PathBuf;

/// Supported JavaScript runtimes, in order of preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsRuntime {
    Bun,
    Deno,
    Node,
}

impl JsRuntime {
    /// The executable name.
    pub fn command(&self) -> &'static str {
        match self {
            Self::Bun => "bun",
            Self::Deno => "deno",
            Self::Node => "node",
        }
    }

    /// Arguments to run a TS file.
    pub fn run_args(&self, script: &str) -> Vec<String> {
        match self {
            Self::Bun => vec!["run".into(), script.into()],
            Self::Deno => vec!["run".into(), "--allow-all".into(), script.into()],
            Self::Node => {
                // Node >=22 has native TS support via --experimental-strip-types
                vec!["--experimental-strip-types".into(), script.into()]
            }
        }
    }

    /// The package manager command to use for `npm install`.
    /// For Bun/Deno we use their built-in install; for Node we use npm.
    pub fn install_command(&self) -> &'static str {
        match self {
            Self::Bun => "bun",
            Self::Deno => "deno",
            Self::Node => "npm",
        }
    }

    /// Arguments for the install command.
    ///
    /// `--ignore-scripts` suppresses dependency lifecycle scripts during install.
    pub fn install_args(&self) -> Vec<String> {
        vec!["install".into(), "--ignore-scripts".into()]
    }
}

/// Detect the best available JS runtime, preferring bun > deno > node.
pub fn detect_runtime() -> Option<JsRuntime> {
    [JsRuntime::Bun, JsRuntime::Deno, JsRuntime::Node]
        .into_iter()
        .find(|&rt| which::which(rt.command()).is_ok())
}

/// Return the full path to the runtime binary, if found.
pub fn runtime_path(rt: JsRuntime) -> Option<PathBuf> {
    which::which(rt.command()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト専用のダミー値。実在するファイルや外部プロセスは使わない。
    const DUMMY_SCRIPT: &str = "/dummy/plugin-host.ts";

    const ALL_RUNTIMES: [JsRuntime; 3] = [JsRuntime::Bun, JsRuntime::Deno, JsRuntime::Node];

    #[test]
    fn command_returns_exact_executable_names() {
        assert_eq!(JsRuntime::Bun.command(), "bun");
        assert_eq!(JsRuntime::Deno.command(), "deno");
        assert_eq!(JsRuntime::Node.command(), "node");
    }

    #[test]
    fn run_args_are_exact_per_runtime_and_keep_script_last() {
        assert_eq!(
            JsRuntime::Bun.run_args(DUMMY_SCRIPT),
            vec!["run".to_string(), DUMMY_SCRIPT.to_string()]
        );
        assert_eq!(
            JsRuntime::Deno.run_args(DUMMY_SCRIPT),
            vec![
                "run".to_string(),
                "--allow-all".to_string(),
                DUMMY_SCRIPT.to_string(),
            ]
        );
        assert_eq!(
            JsRuntime::Node.run_args(DUMMY_SCRIPT),
            vec![
                "--experimental-strip-types".to_string(),
                DUMMY_SCRIPT.to_string(),
            ]
        );

        // どのランタイムでもスクリプト値は改変されず末尾に渡される。
        for script in [DUMMY_SCRIPT, "host with spaces.ts", ""] {
            for rt in ALL_RUNTIMES {
                let args = rt.run_args(script);
                assert_eq!(
                    args.last().map(String::as_str),
                    Some(script),
                    "{rt:?} がスクリプト値を末尾に渡していない: {script:?}"
                );
            }
        }
    }

    #[test]
    fn install_command_uses_native_tool_except_for_node() {
        assert_eq!(JsRuntime::Bun.install_command(), "bun");
        assert_eq!(JsRuntime::Deno.install_command(), "deno");
        assert_eq!(JsRuntime::Node.install_command(), "npm");

        // Node だけは実行コマンドと install コマンドが異なる。
        for rt in ALL_RUNTIMES {
            assert_eq!(
                rt.install_command() == rt.command(),
                rt != JsRuntime::Node,
                "{rt:?} の install コマンド選択が不正"
            );
        }
    }

    #[test]
    fn install_args_disables_lifecycle_scripts_for_all_runtimes() {
        for rt in ALL_RUNTIMES {
            assert_eq!(
                rt.install_args(),
                vec!["install".to_string(), "--ignore-scripts".to_string()]
            );
        }
    }

    #[test]
    fn detect_runtime_respects_preference_order_without_spawning_processes() {
        // which::which は PATH を検索するだけで実行ファイルを起動しない。
        // 特定のランタイムの有無を仮定せず、検出結果が優先順位と整合することだけを検証する。
        let available = |rt: JsRuntime| which::which(rt.command()).is_ok();

        match detect_runtime() {
            Some(JsRuntime::Bun) => {
                // 最優先なので追加条件はない。
            }
            Some(JsRuntime::Deno) => {
                assert!(
                    !available(JsRuntime::Bun),
                    "bun が PATH にあるなら bun を選ぶべき"
                );
            }
            Some(JsRuntime::Node) => {
                assert!(
                    !available(JsRuntime::Bun),
                    "bun が PATH にあるなら bun を選ぶべき"
                );
                assert!(
                    !available(JsRuntime::Deno),
                    "deno が PATH にあるなら deno を選ぶべき"
                );
            }
            None => {
                for rt in ALL_RUNTIMES {
                    assert!(!available(rt), "{rt:?} が PATH にあるなら検出されるべき");
                }
            }
        }
    }

    #[test]
    fn runtime_path_matches_which_and_names_the_expected_executable() {
        for rt in ALL_RUNTIMES {
            let expected = which::which(rt.command()).ok();
            assert_eq!(
                runtime_path(rt),
                expected,
                "{rt:?} の PATH 検索が一致しない"
            );

            if let Some(path) = runtime_path(rt) {
                assert_eq!(
                    path.file_stem().and_then(|name| name.to_str()),
                    Some(rt.command()),
                    "{rt:?} の実行ファイル名が command() と一致しない: {path:?}"
                );
            }
        }

        if let Some(rt) = detect_runtime() {
            let path = runtime_path(rt).expect("検出されたランタイムは PATH 上に存在する");
            assert_eq!(
                path.file_stem().and_then(|name| name.to_str()),
                Some(rt.command())
            );
        }
    }
}
