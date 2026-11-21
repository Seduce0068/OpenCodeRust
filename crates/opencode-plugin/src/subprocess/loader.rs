//! Plugin discovery, npm installation, and subprocess lifecycle management.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use url::Url;

use super::auth::PluginAuthBridge;
use super::client::{PluginContext, PluginSubprocess, PluginSubprocessError};
use super::runtime::{detect_runtime, JsRuntime};
use crate::{Hook, HookContext, HookError, HookEvent, HookOutput, PluginSystem};

// ---------------------------------------------------------------------------
// Embedded host script
// ---------------------------------------------------------------------------

const HOST_SCRIPT: &str = include_str!("../../host/plugin-host.ts");
const BUILTIN_CODEX_AUTH: &str = include_str!("../../builtin/codex-auth.ts");
const BUILTIN_COPILOT_AUTH: &str = include_str!("../../builtin/copilot-auth.ts");

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum PluginLoaderError {
    #[error("no JS runtime found (install bun, deno, or node)")]
    NoRuntime,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("subprocess error: {0}")]
    Subprocess(#[from] PluginSubprocessError),

    /// A plugin subprocess failed to spawn (or failed its initialize handshake).
    ///
    /// Deliberately a unit variant: the spec, the `file://` cache path and the
    /// raw subprocess error can all embed credential-bearing locations, so none
    /// of them is retained here or emitted to the logs. Callers need only the
    /// failure signal in order to fail closed.
    #[error("failed to spawn plugin subprocess")]
    PluginSpawnFailed,

    #[error("npm install failed: {0}")]
    NpmInstall(String),

    #[error("invalid npm plugin spec `{0}`: expected a lowercase npm name with an explicit fixed version (e.g. @scope/pkg@1.2.3)")]
    InvalidNpmSpec(String),

    #[error("plugin path is not absolute and cannot be converted to a file URL: {0:?}")]
    InvalidPluginPath(PathBuf),

    /// A cache path we are about to write to or launch is a symlink. We never
    /// follow it, and we never print the symlink target (which could embed a
    /// credential-bearing path); only the rejected cache path is reported.
    #[error("refusing to use symlinked plugin script path: {0:?}")]
    ScriptIsSymlink(PathBuf),

    #[error("plugin script path is not a regular file: {0:?}")]
    ScriptNotRegularFile(PathBuf),

    /// A cache directory we are about to use is itself a symlink. We never
    /// follow it, and we never print the symlink target (which could embed a
    /// credential-bearing path); only the rejected cache path is reported.
    #[error("refusing to use symlinked cache directory: {0:?}")]
    CacheDirIsSymlink(PathBuf),

    #[error("plugin script failed content verification: {0:?}")]
    ScriptVerificationFailed(PathBuf),
}

// ---------------------------------------------------------------------------
// PluginLoader
// ---------------------------------------------------------------------------

pub struct PluginLoader {
    clients: RwLock<Vec<Arc<PluginSubprocess>>>,
    /// Auth bridges for plugins that declare auth, keyed by provider ID.
    auth_bridges: RwLock<HashMap<String, Arc<PluginAuthBridge>>>,
    hook_system: Arc<PluginSystem>,
    runtime: JsRuntime,
    host_script_path: PathBuf,
    /// Scripts this loader wrote into the shared cache, with the hash of the
    /// immutable embedded content they must still match at launch time.
    trusted_scripts: StdRwLock<Vec<TrustedScript>>,
}

impl PluginLoader {
    /// Create a new loader. Detects the JS runtime and writes the host script
    /// to `~/.cache/opencode/plugin-host.ts`.
    pub fn new() -> Result<Self, PluginLoaderError> {
        let runtime = detect_runtime().ok_or(PluginLoaderError::NoRuntime)?;

        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("opencode");
        ensure_secure_cache_dir(&cache_dir)?;

        let host_script_path = cache_dir.join("plugin-host.ts");
        write_script_atomic(&host_script_path, HOST_SCRIPT.as_bytes())?;

        Ok(Self {
            clients: RwLock::new(Vec::new()),
            auth_bridges: RwLock::new(HashMap::new()),
            hook_system: Arc::new(PluginSystem::new()),
            runtime,
            trusted_scripts: StdRwLock::new(vec![TrustedScript {
                path: host_script_path.clone(),
                expected_hash: script_hash(HOST_SCRIPT.as_bytes()),
            }]),
            host_script_path,
        })
    }

    /// Load all plugins from the given spec list.
    ///
    /// Each spec is either:
    /// - `file:///path/to/plugin.ts` — loaded directly
    /// - An npm package name (e.g. `opencode-anthropic-auth@0.0.13`)
    pub async fn load_all(
        &self,
        specs: &[String],
        context: &PluginContext,
    ) -> Result<(), PluginLoaderError> {
        self.load_all_with_spawn(
            specs,
            context,
            |runtime, host, plugin, ctx, cwd| async move {
                PluginSubprocess::spawn(runtime, &host, &plugin, ctx, cwd.as_deref()).await
            },
        )
        .await
    }

    /// [`Self::load_all`] with the subprocess spawner injected.
    ///
    /// Keeping the spawn behind a parameter lets the failure path be tested
    /// without launching a real JS runtime. Production callers always go
    /// through [`Self::load_all`], which supplies [`PluginSubprocess::spawn`].
    /// Owned arguments are passed to `spawn` so the returned future never
    /// borrows from this closure.
    async fn load_all_with_spawn<F, Fut>(
        &self,
        specs: &[String],
        context: &PluginContext,
        spawn: F,
    ) -> Result<(), PluginLoaderError>
    where
        F: Fn(JsRuntime, String, String, PluginContext, Option<PathBuf>) -> Fut,
        Fut: std::future::Future<Output = Result<PluginSubprocess, PluginSubprocessError>>,
    {
        // Collect npm packages that need installing
        let npm_specs: Vec<&str> = specs
            .iter()
            .filter(|s| !s.starts_with("file://"))
            .map(|s| s.as_str())
            .collect();

        if !npm_specs.is_empty() {
            self.install_npm_packages(&npm_specs).await?;
        }

        // Verify every script we placed in the shared cache immediately before
        // any subprocess is launched. The cache may be writable by other users,
        // so a pre-existing symlink or a post-write swap must abort startup
        // rather than hand arbitrary TypeScript to a `--allow-all` runtime.
        {
            let trusted = self
                .trusted_scripts
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for script in trusted.iter() {
                verify_script(&script.path, script.expected_hash)?;
            }
        }

        // Spawn each plugin. A single failure aborts the whole load so callers
        // can fail closed (notably for the built-in auth plugins).
        for spec in specs {
            let plugin_path = self.resolve_plugin(spec)?;
            // For npm packages (non file:// specs), set cwd to the npm dir
            // so bare-specifier imports resolve against node_modules/.
            let cwd = if !spec.starts_with("file://") {
                Some(self.npm_dir())
            } else {
                None
            };
            match spawn(
                self.runtime,
                self.host_script_path
                    .to_str()
                    .unwrap_or("plugin-host.ts")
                    .to_string(),
                plugin_path,
                context.clone(),
                cwd,
            )
            .await
            {
                Ok(client) => {
                    tracing::info!(
                        plugin = client.name(),
                        hooks = ?client.hooks(),
                        has_auth = client.auth_meta().is_some(),
                        "loaded TS plugin"
                    );
                    let client = Arc::new(client);

                    // If the plugin provides auth, create an auth bridge
                    if let Some(auth_meta) = client.auth_meta().cloned() {
                        let provider = auth_meta.provider.clone();
                        let bridge =
                            Arc::new(PluginAuthBridge::new(Arc::clone(&client), auth_meta));
                        tracing::info!(
                            plugin = client.name(),
                            provider = provider.as_str(),
                            methods = ?bridge.methods(),
                            "registered plugin auth bridge"
                        );
                        let mut bridges = self.auth_bridges.write().await;
                        bridges.insert(provider, bridge);
                    }
                    self.register_client_hooks(Arc::clone(&client)).await;

                    let mut clients = self.clients.write().await;
                    clients.push(client);
                }
                Err(_) => {
                    // Fail closed: the spec, the file:// cache path and the raw
                    // subprocess error are deliberately never logged — any of
                    // them can embed a credential-bearing location. Emit only
                    // a fixed event name plus a [REDACTED] placeholder.
                    tracing::error!(
                        event = "plugin_subprocess_spawn_failed",
                        error = "[REDACTED]",
                        "Failed to spawn plugin subprocess"
                    );
                    return Err(PluginLoaderError::PluginSpawnFailed);
                }
            }
        }

        Ok(())
    }

    /// Load bundled auth plugins shipped with the Rust runtime.
    pub async fn load_builtins(&self, context: &PluginContext) -> Result<(), PluginLoaderError> {
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("opencode")
            .join("plugins")
            .join("builtin");
        ensure_secure_cache_dir(&cache_dir)?;

        let codex_path = cache_dir.join("builtin-codex-auth.ts");
        write_script_atomic(&codex_path, BUILTIN_CODEX_AUTH.as_bytes())?;

        let copilot_path = cache_dir.join("builtin-copilot-auth.ts");
        write_script_atomic(&copilot_path, BUILTIN_COPILOT_AUTH.as_bytes())?;

        self.trust_script(TrustedScript {
            path: codex_path.clone(),
            expected_hash: script_hash(BUILTIN_CODEX_AUTH.as_bytes()),
        });
        self.trust_script(TrustedScript {
            path: copilot_path.clone(),
            expected_hash: script_hash(BUILTIN_COPILOT_AUTH.as_bytes()),
        });

        let specs = vec![file_url(&codex_path)?, file_url(&copilot_path)?];
        self.load_all(&specs, context).await
    }

    /// Get all loaded plugin clients.
    pub async fn clients(&self) -> Vec<Arc<PluginSubprocess>> {
        self.clients.read().await.clone()
    }

    /// Get the hook system this loader registers bridge hooks into.
    pub fn hook_system(&self) -> Arc<PluginSystem> {
        Arc::clone(&self.hook_system)
    }

    /// Shut down all plugin subprocesses and drop their registries.
    ///
    /// RES-07: `shutdown_all()` used to leave `clients` / `auth_bridges` populated,
    /// so every reload pushed another client onto the old ones. Both registries
    /// are now taken (i.e. drained) before the best-effort shutdown so a
    /// concurrent reload can neither observe nor append to stale entries.
    pub async fn shutdown_all(&self) {
        let clients = std::mem::take(&mut *self.clients.write().await);
        let _auth_bridges = std::mem::take(&mut *self.auth_bridges.write().await);
        for client in clients.iter() {
            if let Err(e) = client.shutdown().await {
                log_shutdown_failure(&e);
            }
        }
    }

    /// Get the auth bridge for a given provider ID, if any plugin provides it.
    pub async fn auth_bridge(&self, provider: &str) -> Option<Arc<PluginAuthBridge>> {
        self.auth_bridges.read().await.get(provider).cloned()
    }

    /// Get all registered auth bridges, keyed by provider ID.
    pub async fn auth_bridges(&self) -> HashMap<String, Arc<PluginAuthBridge>> {
        self.auth_bridges.read().await.clone()
    }

    // -- Private helpers ----------------------------------------------------

    /// Register a cache script for launch-time verification.
    ///
    /// RES-07: `load_builtins()` can run more than once during reloads. Without
    /// this check every run appended another `TrustedScript` for the same path,
    /// growing the verification list on each reload. Identity is the pair
    /// `(path, expected_hash)`: a different hash at the same path (or the same
    /// hash at a different path) is a genuinely distinct entry.
    fn trust_script(&self, script: TrustedScript) {
        let mut trusted = self
            .trusted_scripts
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let duplicate = trusted.iter().any(|existing| {
            existing.path == script.path && existing.expected_hash == script.expected_hash
        });
        if !duplicate {
            trusted.push(script);
        }
    }

    async fn register_client_hooks(&self, client: Arc<PluginSubprocess>) {
        for hook_name in client.hooks() {
            let Some(event) = super::hook_name_to_event(hook_name) else {
                tracing::debug!(
                    plugin = client.name(),
                    hook = hook_name.as_str(),
                    "skipping unsupported TS hook"
                );
                continue;
            };

            let hook_id = format!("ts:{}:{}", client.name(), hook_name);
            let plugin_name = client.name().to_string();
            let hook_name_owned = hook_name.clone();
            let hook_client = Arc::clone(&client);

            // Avoid duplicate registrations when load_all() is called more than once.
            let _ = self.hook_system.remove(&event, &hook_id).await;
            self.hook_system
                .register(Hook::new(&hook_id, event, move |context: HookContext| {
                    let hook_client = Arc::clone(&hook_client);
                    let hook_name_owned = hook_name_owned.clone();
                    let plugin_name = plugin_name.clone();
                    async move {
                        let (input, output) = hook_io_from_context(&context);
                        let value = hook_client
                            .invoke_hook(&hook_name_owned, input, output)
                            .await
                            .map_err(|err| {
                                HookError::ExecutionError(format!(
                                    "TS plugin `{}` hook `{}` failed: {}",
                                    plugin_name, hook_name_owned, err
                                ))
                            })?;
                        Ok(HookOutput::with_payload(value))
                    }
                }))
                .await;
        }
    }

    /// Resolve a plugin spec to a path that the host can `import()`.
    ///
    /// For npm packages, we return the bare package name (not a `file://` URL)
    /// because `import("file:///path/to/dir")` doesn't resolve `package.json`
    /// exports — only bare specifiers trigger full module resolution.
    /// The subprocess working directory is set to `npm_dir()` so the runtime
    /// finds the package in `node_modules/`.
    fn resolve_plugin(&self, spec: &str) -> Result<String, PluginLoaderError> {
        if spec.starts_with("file://") {
            return Ok(spec.to_string());
        }

        // npm package — return bare package name for proper module resolution.
        // Reuse parse_npm_spec so scoped names (e.g. @scope/pkg@1.0) keep their
        // leading `@scope/` instead of being truncated at the first `@`.
        let (name, _version) = parse_npm_spec(spec)?;
        Ok(name.to_string())
    }

    /// Run an install command with stdout and stderr discarded (`Stdio::null()`).
    ///
    /// The child never sees a pipe, so it can write an unlimited amount of
    /// output without blocking on a full OS pipe buffer: `status()` cannot
    /// deadlock the way piped stdio + `status()` would, and nothing is buffered
    /// in memory the way `output()` would buffer it. Install output may contain
    /// local paths or registry URLs, so it is neither retained nor logged; only
    /// the exit status decides success, and only a fixed message is used on
    /// failure (see `NpmInstall`).
    async fn run_install_command(
        command: &str,
        args: &[String],
        cwd: &Path,
    ) -> Result<std::process::ExitStatus, std::io::Error> {
        let status = tokio::process::Command::new(command)
            .args(args)
            .current_dir(cwd)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await?;
        Ok(status)
    }

    /// Install npm packages into the shared cache directory.
    async fn install_npm_packages(&self, specs: &[&str]) -> Result<(), PluginLoaderError> {
        let npm_dir = self.npm_dir();
        std::fs::create_dir_all(&npm_dir)?;

        // Write/update package.json
        let pkg_json = npm_dir.join("package.json");

        // Reuse cached deps only after re-validating every entry. The cache is
        // writable between runs, so a poisoned package.json must not smuggle
        // dist-tags, ranges, `file:` specs or malformed names into this install.
        let mut deps = read_validated_deps(&pkg_json);

        // Add new packages
        for spec in specs {
            let (name, version) = parse_npm_spec(spec)?;
            deps.insert(
                name.to_string(),
                serde_json::Value::String(version.to_string()),
            );
        }

        let pkg = serde_json::json!({
            "name": "opencode-plugins",
            "private": true,
            "dependencies": deps,
        });
        // package.json も共有キャッシュ上の書き込み対象なので、スクリプトと同じ
        // symlink 拒否・create_new・0600・atomic rename の経路に統一する。
        let pkg_bytes = serde_json::to_string_pretty(&pkg).unwrap();
        write_script_atomic(&pkg_json, pkg_bytes.as_bytes())?;

        // Run install
        let install_cmd = self.runtime.install_command();
        let install_args = self.runtime.install_args();

        let status = Self::run_install_command(install_cmd, &install_args, &npm_dir).await?;

        if !status.success() {
            return Err(PluginLoaderError::NpmInstall(format!(
                "{} install exited with {}",
                install_cmd, status
            )));
        }

        Ok(())
    }

    fn npm_dir(&self) -> PathBuf {
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("opencode")
            .join("plugins")
    }
}

/// Emit a fixed, value-independent warning when a plugin subprocess fails to
/// shut down.
///
/// The error originates from plugin-controlled RPC responses and may embed
/// tokens, URLs or credential-bearing paths. The value is deliberately never
/// formatted into the event: only a fixed event name and a `[REDACTED]`
/// placeholder reach the logs, matching the fail-closed spawn-failure path.
fn log_shutdown_failure<E: std::fmt::Display>(_error: &E) {
    tracing::warn!(
        event = "plugin_shutdown_failed",
        error = "[REDACTED]",
        "error shutting down plugin"
    );
}

// ---------------------------------------------------------------------------
// Secure cache writing / verification helpers
// ---------------------------------------------------------------------------

/// A script this loader wrote to the shared cache and can verify at launch.
struct TrustedScript {
    path: PathBuf,
    /// SHA-256 digest of the immutable embedded content this file must match.
    expected_hash: [u8; 32],
}

/// Create `dir` and, on Unix, force it to `0700` — also correcting the mode of
/// a pre-existing directory. `~/.cache/opencode` may be group/world writable,
/// which would let another user replace cached scripts between our write and
/// the subprocess launch. A directory path that is itself a symlink is
/// rejected: `create_dir_all` would happily follow one, so it must be checked
/// through `symlink_metadata` (which never follows the final component).
/// Windows keeps the previous `create_dir_all` behavior.
fn ensure_secure_cache_dir(dir: &Path) -> Result<(), PluginLoaderError> {
    std::fs::create_dir_all(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() {
        return Err(PluginLoaderError::CacheDirIsSymlink(dir.to_path_buf()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// SHA-256 of the script content. The expected value is derived from an
/// immutable `include_str!` constant in this binary, so a match proves the
/// on-disk script is the one we generated; there is no attacker-writable hash
/// table to tamper with. A cryptographic digest is used instead of a 64-bit
/// FNV-1a so a targeted collision cannot be crafted cheaply.
fn script_hash(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

/// Monotonic, process-unique suffix for temp file names.
fn unique_temp_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!(
        "{}-{nanos}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Replace `dest` with `temp` atomically where the platform allows it.
#[cfg(unix)]
fn replace_cached_file(temp: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::rename(temp, dest)
}

/// `std::fs::rename` cannot replace an existing destination on Windows, so the
/// target entry is removed first. `remove_file` removes the symlink itself and
/// never follows it.
#[cfg(not(unix))]
fn replace_cached_file(temp: &Path, dest: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(dest).is_ok() {
        std::fs::remove_file(dest)?;
    }
    std::fs::rename(temp, dest)
}

/// Write `content` to `path` without ever following a symlink at `path`:
/// reject a pre-existing symlink, write to a `create_new` temp file in the same
/// directory (`0600` on Unix), then atomically rename it into place. The rename
/// replaces the directory entry itself, so even a race after the check cannot
/// turn this into a write through a symlink.
fn write_script_atomic(path: &Path, content: &[u8]) -> Result<(), PluginLoaderError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            let file_type = meta.file_type();
            if file_type.is_symlink() {
                return Err(PluginLoaderError::ScriptIsSymlink(path.to_path_buf()));
            }
            if !file_type.is_file() {
                return Err(PluginLoaderError::ScriptNotRegularFile(path.to_path_buf()));
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("plugin-script");
    let temp = parent.join(format!(".{file_name}.tmp-{}", unique_temp_suffix()));

    let result = write_temp_then_replace(&temp, path, content);
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Write to the fresh temp path (never an existing entry) and swap it into
/// place. Kept separate so `write_script_atomic` can clean up on any failure.
fn write_temp_then_replace(
    temp: &Path,
    dest: &Path,
    content: &[u8],
) -> Result<(), PluginLoaderError> {
    use std::io::Write;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(temp)?;
    file.write_all(content)?;
    file.flush()?;
    drop(file);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(temp, std::fs::Permissions::from_mode(0o600))?;
    }
    replace_cached_file(temp, dest)?;
    Ok(())
}

/// Immediately before launch, confirm `path` is still our regular file (not a
/// symlink) and still has the exact content hash we wrote. Any mismatch aborts
/// startup instead of handing arbitrary TypeScript to a `--allow-all` runtime.
/// No file content is logged — only the cache path is reported on failure.
fn verify_script(path: &Path, expected_hash: [u8; 32]) -> Result<(), PluginLoaderError> {
    let meta = std::fs::symlink_metadata(path)?;
    let file_type = meta.file_type();
    if file_type.is_symlink() {
        return Err(PluginLoaderError::ScriptIsSymlink(path.to_path_buf()));
    }
    if !file_type.is_file() {
        return Err(PluginLoaderError::ScriptNotRegularFile(path.to_path_buf()));
    }
    let bytes = std::fs::read(path)?;
    if script_hash(&bytes) != expected_hash {
        return Err(PluginLoaderError::ScriptVerificationFailed(
            path.to_path_buf(),
        ));
    }
    Ok(())
}

/// Build an encoded, local `file://` URL for a plugin path.
///
/// `Url::from_file_path` percent-encodes URL-significant characters (`#`, `?`,
/// `%`, spaces) and yields a localhost/empty-host URL on every platform.
/// `format!("file://{}", path.display())` instead breaks on Windows (`file://C:\...`,
/// rejected by the host's local-host check) and for paths containing those
/// characters.
fn file_url(path: &Path) -> Result<String, PluginLoaderError> {
    Url::from_file_path(path)
        .map(|url| url.to_string())
        .map_err(|_| PluginLoaderError::InvalidPluginPath(path.to_path_buf()))
}

/// Validate an npm package name using the same lowercase-only grammar as the
/// host (`[a-z0-9][a-z0-9._-]*`) plus npm's ban on leading/trailing/repeated
/// dots. Scoped names (`@scope/pkg`) validate both the scope and the leaf.
fn is_valid_npm_name(name: &str) -> bool {
    let leaf = match name.strip_prefix('@') {
        Some(rest) => match rest.split_once('/') {
            Some((scope, leaf)) if is_valid_npm_name_part(scope) => leaf,
            _ => return false,
        },
        None => name,
    };
    is_valid_npm_name_part(leaf)
}

/// One name component: `[a-z0-9][a-z0-9._-]*`, no leading/trailing or repeated
/// dot, within npm's 214-character limit.
fn is_valid_npm_name_part(part: &str) -> bool {
    if part.is_empty() || part.len() > 214 {
        return false;
    }
    let mut bytes = part.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    if part.starts_with('.') || part.ends_with('.') || part.contains("..") {
        return false;
    }
    bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
}

/// Accept only fixed versions (`1.2.3`, `1.2.3-beta.1+build.5`), never dist-tags
/// (`latest`) or ranges (`^1.0.0`, `1.x`, `>=1.0.0`).
fn is_fixed_version(version: &str) -> bool {
    let mut bytes = version.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !first.is_ascii_digit() {
        return false;
    }
    if !bytes
        .all(|b| b.is_ascii_digit() || b.is_ascii_alphabetic() || matches!(b, b'.' | b'+' | b'-'))
    {
        return false;
    }
    // `1.x` / `1.X` select a range, so they are not pinned versions.
    !version.split('.').any(|part| part == "x" || part == "X")
}

/// Parse `"pkg@version"` into `(name, version)`. Handles scoped packages like
/// `"@scope/pkg@1.0"`.
///
/// An explicit fixed version and a valid lowercase npm name are required;
/// dist-tags, ranges, wildcards and malformed names are rejected so that
/// repeated installs cannot silently resolve to a different release.
fn parse_npm_spec(spec: &str) -> Result<(&str, &str), PluginLoaderError> {
    // Handle scoped packages: @scope/pkg@version
    let (name, version) = if let Some(stripped) = spec.strip_prefix('@') {
        if let Some(idx) = stripped.find('@') {
            let split = idx + 1;
            (&spec[..split], &spec[split + 1..])
        } else {
            (spec, "")
        }
    } else if let Some(idx) = spec.find('@') {
        (&spec[..idx], &spec[idx + 1..])
    } else {
        (spec, "")
    };

    if !is_valid_npm_name(name) || !is_fixed_version(version) {
        return Err(PluginLoaderError::InvalidNpmSpec(spec.to_string()));
    }

    Ok((name, version))
}

/// Re-validate one cached `(name, version)` dependency using the same grammar
/// as `parse_npm_spec`.
///
/// Cached dependencies are stored as separate name/version fields, so we rebuild
/// the `name@version` spec and require `parse_npm_spec` to both accept it and
/// return exactly the original pair. This reuses the npm-name and fixed-version
/// checks instead of duplicating them, and prevents tampered names from parsing
/// into a different (but valid) dependency.
fn is_valid_cached_dependency(name: &str, version: &str) -> bool {
    let spec = format!("{name}@{version}");
    match parse_npm_spec(&spec) {
        Ok((parsed_name, parsed_version)) => parsed_name == name && parsed_version == version,
        Err(_) => false,
    }
}

/// Read the cached `dependencies` map, dropping any entry that is not a pinned,
/// well-formed npm spec.
///
/// The cache is attacker-writable between runs, so every entry is re-validated
/// through `parse_npm_spec` before it is carried into the next install. Invalid
/// entries are skipped rather than fatal: one poisoned record cannot brick
/// startup, and it is never written back to `package.json` or handed to the
/// installer.
fn read_validated_deps(pkg_json: &Path) -> Map<String, Value> {
    let mut deps = Map::new();
    if !pkg_json.exists() {
        return deps;
    }
    let Ok(content) = std::fs::read_to_string(pkg_json) else {
        return deps;
    };
    let Ok(existing) = serde_json::from_str::<Value>(&content) else {
        return deps;
    };
    let Some(obj) = existing.get("dependencies").and_then(|d| d.as_object()) else {
        return deps;
    };

    for (name, value) in obj {
        let Some(version) = value.as_str() else {
            // Object forms such as npm aliases (`"pkg": "npm:other@1"`) and
            // non-string values are not pinned versions we can vouch for.
            tracing::warn!(
                dependency = name.as_str(),
                "dropping cached dependency without a string version"
            );
            continue;
        };
        if is_valid_cached_dependency(name, version) {
            deps.insert(name.clone(), value.clone());
        } else {
            tracing::warn!(
                dependency = name.as_str(),
                version,
                "dropping invalid cached dependency (not a pinned npm spec)"
            );
        }
    }
    deps
}

fn hook_io_from_context(context: &HookContext) -> (Value, Value) {
    let source = context_values(context);
    let mut input = Map::new();
    let mut output = Map::new();

    match context.event {
        HookEvent::ToolDefinition => {
            copy_first(&source, &mut input, "toolID", &["toolID"]);
            copy_first(&source, &mut output, "description", &["description"]);
            copy_first(&source, &mut output, "parameters", &["parameters"]);
        }
        HookEvent::ToolExecuteBefore => {
            copy_first(&source, &mut input, "tool", &["tool"]);
            copy_first(&source, &mut input, "sessionID", &["sessionID"]);
            copy_first(&source, &mut input, "callID", &["callID"]);
            copy_first(&source, &mut output, "args", &["args"]);
        }
        HookEvent::ToolExecuteAfter => {
            copy_first(&source, &mut input, "tool", &["tool"]);
            copy_first(&source, &mut input, "sessionID", &["sessionID"]);
            copy_first(&source, &mut input, "callID", &["callID"]);
            copy_first(&source, &mut input, "args", &["args"]);
            copy_first(&source, &mut input, "error", &["error"]);
            copy_first(&source, &mut output, "title", &["title"]);
            copy_first(&source, &mut output, "output", &["output"]);
            copy_first(&source, &mut output, "metadata", &["metadata"]);
        }
        HookEvent::ChatSystemTransform => {
            copy_first(&source, &mut input, "sessionID", &["sessionID"]);
            copy_first(&source, &mut input, "model", &["model"]);
            if !input.contains_key("model") {
                if let Some(model) = synthesize_model(&source) {
                    input.insert("model".to_string(), model);
                }
            }
            copy_first(&source, &mut output, "system", &["system"]);
        }
        HookEvent::ChatMessagesTransform => {
            copy_first(&source, &mut output, "messages", &["messages"]);
        }
        HookEvent::ChatParams => {
            copy_first(&source, &mut input, "sessionID", &["sessionID"]);
            copy_first(&source, &mut input, "agent", &["agent"]);
            copy_first(&source, &mut input, "model", &["model"]);
            if !input.contains_key("model") {
                if let Some(model) = synthesize_model(&source) {
                    input.insert("model".to_string(), model);
                }
            }
            copy_first(&source, &mut input, "provider", &["provider"]);
            if !input.contains_key("provider") {
                if let Some(provider) = synthesize_provider(&source) {
                    input.insert("provider".to_string(), provider);
                }
            }
            copy_first(&source, &mut input, "message", &["message"]);
            copy_first(&source, &mut output, "temperature", &["temperature"]);
            copy_first(&source, &mut output, "topP", &["topP"]);
            copy_first(&source, &mut output, "topK", &["topK"]);
            copy_first(&source, &mut output, "options", &["options"]);
            copy_first(&source, &mut output, "maxTokens", &["maxTokens"]);
        }
        HookEvent::ChatHeaders => {
            copy_first(&source, &mut input, "sessionID", &["sessionID"]);
            copy_first(&source, &mut input, "agent", &["agent"]);
            copy_first(&source, &mut input, "model", &["model"]);
            if !input.contains_key("model") {
                if let Some(model) = synthesize_model(&source) {
                    input.insert("model".to_string(), model);
                }
            }
            copy_first(&source, &mut input, "provider", &["provider"]);
            if !input.contains_key("provider") {
                if let Some(provider) = synthesize_provider(&source) {
                    input.insert("provider".to_string(), provider);
                }
            }
            copy_first(&source, &mut input, "message", &["message"]);
            copy_first(&source, &mut output, "headers", &["headers"]);
        }
        HookEvent::ChatMessage => {
            copy_first(&source, &mut input, "sessionID", &["sessionID"]);
            copy_first(&source, &mut input, "agent", &["agent"]);
            copy_first(&source, &mut input, "model", &["model"]);
            if !input.contains_key("model") {
                if let Some(model) = synthesize_model(&source) {
                    input.insert("model".to_string(), model);
                }
            }
            copy_first(&source, &mut input, "messageID", &["messageID"]);
            copy_first(&source, &mut input, "variant", &["variant"]);
            copy_first(&source, &mut input, "has_tool_calls", &["has_tool_calls"]);
            copy_first(&source, &mut output, "message", &["message"]);
            copy_first(&source, &mut output, "parts", &["parts"]);
        }
        HookEvent::SessionCompacting => {
            copy_first(&source, &mut input, "sessionID", &["sessionID"]);
            copy_first(&source, &mut input, "auto", &["auto"]);
            copy_first(&source, &mut input, "completed", &["completed"]);
            copy_first(&source, &mut output, "context", &["context"]);
            copy_first(&source, &mut output, "prompt", &["prompt"]);
        }
        HookEvent::TextComplete => {
            copy_first(&source, &mut input, "sessionID", &["sessionID"]);
            copy_first(&source, &mut input, "messageID", &["messageID"]);
            copy_first(&source, &mut input, "partID", &["partID"]);
            copy_first(&source, &mut output, "text", &["text"]);
        }
        HookEvent::ShellEnv => {
            copy_first(&source, &mut input, "cwd", &["cwd"]);
            copy_first(&source, &mut input, "sessionID", &["sessionID"]);
            copy_first(&source, &mut input, "callID", &["callID"]);
            copy_first(&source, &mut output, "env", &["env"]);
        }
        HookEvent::CommandExecuteBefore => {
            copy_first(&source, &mut input, "command", &["command"]);
            copy_first(&source, &mut input, "sessionID", &["sessionID"]);
            copy_first(&source, &mut input, "arguments", &["arguments"]);
            copy_first(&source, &mut input, "source", &["source"]);
            copy_first(&source, &mut output, "parts", &["parts"]);
        }
        HookEvent::PermissionAsk => {
            copy_first(&source, &mut input, "permission", &["permission"]);
            copy_first(
                &source,
                &mut input,
                "permission_type",
                &["permission_type", "permissionType"],
            );
            copy_first(
                &source,
                &mut input,
                "permission_id",
                &["permission_id", "permissionID"],
            );
            copy_first(&source, &mut output, "status", &["status"]);
        }
        _ => {
            input = source.clone();
            output = source;
        }
    }

    seed_hook_output(context.event.clone(), &mut output);

    (Value::Object(input), Value::Object(output))
}

fn context_values(context: &HookContext) -> Map<String, Value> {
    let mut values = Map::new();
    for (key, value) in &context.data {
        values.insert(key.clone(), value.clone());
        let normalized = normalize_hook_key(key);
        if normalized != *key {
            values.entry(normalized).or_insert_with(|| value.clone());
        }
    }
    if let Some(session_id) = &context.session_id {
        values
            .entry("sessionID".to_string())
            .or_insert_with(|| Value::String(session_id.clone()));
    }
    values
}

fn first_value(source: &Map<String, Value>, keys: &[&str]) -> Option<Value> {
    keys.iter().find_map(|key| source.get(*key).cloned())
}

fn copy_first(
    source: &Map<String, Value>,
    target: &mut Map<String, Value>,
    target_key: &str,
    candidate_keys: &[&str],
) {
    if let Some(value) = first_value(source, candidate_keys) {
        target.insert(target_key.to_string(), value);
    }
}

fn synthesize_model(source: &Map<String, Value>) -> Option<Value> {
    let model_id = first_value(source, &["modelID", "model_id"])?;
    let mut model = Map::new();
    model.insert("modelID".to_string(), model_id.clone());
    model.insert("id".to_string(), model_id);
    if let Some(provider_id) = first_value(source, &["providerID", "provider_id"]) {
        model.insert("providerID".to_string(), provider_id);
    }
    Some(Value::Object(model))
}

fn synthesize_provider(source: &Map<String, Value>) -> Option<Value> {
    let provider_id = first_value(source, &["providerID", "provider_id"])?;
    let mut provider = Map::new();
    provider.insert("id".to_string(), provider_id.clone());
    provider.insert(
        "info".to_string(),
        Value::Object(Map::from_iter([("id".to_string(), provider_id)])),
    );
    Some(Value::Object(provider))
}

fn normalize_hook_key(key: &str) -> String {
    match key {
        "tool_id" => "toolID".to_string(),
        "call_id" => "callID".to_string(),
        "model_id" => "modelID".to_string(),
        "provider_id" => "providerID".to_string(),
        "message_id" => "messageID".to_string(),
        "part_id" => "partID".to_string(),
        "max_tokens" => "maxTokens".to_string(),
        _ => key.to_string(),
    }
}

fn ensure_default(map: &mut Map<String, Value>, key: &str, value: Value) {
    map.entry(key.to_string()).or_insert(value);
}

fn ensure_object(map: &mut Map<String, Value>, key: &str) {
    ensure_default(map, key, Value::Object(Map::new()));
}

fn ensure_array(map: &mut Map<String, Value>, key: &str) {
    ensure_default(map, key, Value::Array(Vec::new()));
}

fn seed_hook_output(event: HookEvent, output: &mut Map<String, Value>) {
    match event {
        HookEvent::ToolDefinition => {
            ensure_default(output, "description", Value::String(String::new()));
            ensure_object(output, "parameters");
        }
        HookEvent::ToolExecuteBefore => {
            ensure_object(output, "args");
        }
        HookEvent::ToolExecuteAfter => {
            ensure_default(output, "title", Value::String(String::new()));
            ensure_default(output, "output", Value::String(String::new()));
            ensure_object(output, "metadata");
        }
        HookEvent::ChatHeaders => {
            ensure_object(output, "headers");
        }
        HookEvent::ChatParams => {
            ensure_default(output, "temperature", Value::Null);
            ensure_default(output, "topP", Value::Null);
            ensure_default(output, "topK", Value::Null);
            ensure_object(output, "options");
        }
        HookEvent::ChatMessage => {
            ensure_default(output, "message", Value::Null);
            ensure_array(output, "parts");
        }
        HookEvent::ChatMessagesTransform => {
            ensure_array(output, "messages");
        }
        HookEvent::ChatSystemTransform => {
            ensure_array(output, "system");
        }
        HookEvent::SessionCompacting => {
            ensure_array(output, "context");
        }
        HookEvent::TextComplete => {
            ensure_default(output, "text", Value::String(String::new()));
        }
        HookEvent::ShellEnv => {
            ensure_object(output, "env");
        }
        HookEvent::CommandExecuteBefore => {
            ensure_array(output, "parts");
        }
        HookEvent::PermissionAsk => {
            ensure_default(output, "status", Value::String("ask".to_string()));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_npm_spec() {
        assert_eq!(parse_npm_spec("foo@1.0.0").unwrap(), ("foo", "1.0.0"));
        assert_eq!(
            parse_npm_spec("@scope/foo@2.0").unwrap(),
            ("@scope/foo", "2.0")
        );
        assert_eq!(
            parse_npm_spec("foo-bar_baz.qux@1.2.3-beta.1+build.5").unwrap(),
            ("foo-bar_baz.qux", "1.2.3-beta.1+build.5")
        );
        assert_eq!(
            parse_npm_spec("@scope/pkg-name@0.0.13").unwrap(),
            ("@scope/pkg-name", "0.0.13")
        );

        // 未指定・空・ワイルドカードの version はエラー（install 毎の最新解釈を防ぐ）
        for spec in ["foo", "@scope/foo", "foo@", "foo@*", "@scope/foo@"] {
            assert!(
                parse_npm_spec(spec).is_err(),
                "未指定 spec はエラーになるべき: {spec}"
            );
        }
    }

    #[test]
    fn parse_npm_spec_rejects_invalid_names_and_non_fixed_versions() {
        for spec in [
            "Foo@1.0.0",
            "foo..bar@1.0.0",
            ".foo@1.0.0",
            "foo.@1.0.0",
            "@Scope/pkg@1.0.0",
            "@scope/pkg..x@1.0.0",
            "@scope@1.0.0",
            "foo@latest",
            "foo@^1.0.0",
            "foo@~1.2.3",
            "foo@1.x",
            "foo@>=1.0.0",
        ] {
            assert!(
                parse_npm_spec(spec).is_err(),
                "不正 spec は拒否されるべき: {spec}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn file_url_keeps_plain_absolute_path_shape() {
        assert_eq!(
            file_url(Path::new("/tmp/plugin.ts")).unwrap(),
            "file:///tmp/plugin.ts"
        );
    }

    #[test]
    fn file_url_percent_encodes_url_significant_characters() {
        let path = std::env::temp_dir().join("plugin #1?100%.ts");
        let url = file_url(&path).expect("絶対パスは file URL へ変換できる");
        assert!(url.starts_with("file:///"), "{url}");
        assert!(url.contains("%20"), "スペース未エンコード: {url}");
        assert!(url.contains("%23"), "# 未エンコード: {url}");
        assert!(url.contains("%3F"), "? 未エンコード: {url}");
        assert!(url.contains("%25"), "% 未エンコード: {url}");
        assert!(url.ends_with(".ts"), "{url}");
    }

    #[test]
    fn file_url_rejects_relative_paths() {
        assert!(file_url(Path::new("relative/plugin.ts")).is_err());
    }

    #[test]
    fn first_value_returns_first_matching_key() {
        let mut source = Map::new();
        source.insert("modelID".to_string(), serde_json::json!("m1"));
        source.insert("model_id".to_string(), serde_json::json!("m2"));
        assert_eq!(
            first_value(&source, &["modelID", "model_id"]),
            Some(serde_json::json!("m1"))
        );
        assert_eq!(first_value(&source, &["missing"]), None);
    }

    #[test]
    fn synthesize_model_uses_snake_case_fallback() {
        let mut source = Map::new();
        source.insert("model_id".to_string(), serde_json::json!("gpt-5"));
        source.insert("provider_id".to_string(), serde_json::json!("openai"));

        let model = synthesize_model(&source).unwrap();
        assert_eq!(model["modelID"], serde_json::json!("gpt-5"));
        assert_eq!(model["id"], serde_json::json!("gpt-5"));
        assert_eq!(model["providerID"], serde_json::json!("openai"));
    }

    #[test]
    fn synthesize_model_without_id_is_none() {
        assert!(synthesize_model(&Map::new()).is_none());
    }

    #[test]
    fn normalize_hook_key_converts_snake_to_camel() {
        assert_eq!(normalize_hook_key("tool_id"), "toolID");
        assert_eq!(normalize_hook_key("max_tokens"), "maxTokens");
        assert_eq!(normalize_hook_key("custom"), "custom");
    }

    /// テスト専用の一意な一時ディレクトリを作成する（外部クレートに依存しない）。
    fn unique_temp_dir(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "opencode-plugin-{label}-{}-{nanos}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("一時ディレクトリを作成できる");
        dir
    }

    #[test]
    fn cached_dependencies_are_revalidated_before_reuse() {
        let dir = unique_temp_dir("cache-deps");
        let pkg_json = dir.join("package.json");
        let fixture = serde_json::json!({
            "name": "opencode-plugins",
            "private": true,
            "dependencies": {
                // 正常な固定バージョンはそのまま保持される
                "good-pkg": "1.2.3",
                "@scope/good": "0.0.13",
                "foo-bar_baz.qux": "1.2.3-beta.1+build.5",
                // 不正・非固定・非文字列エントリはすべて排除される
                "evil-latest": "latest",
                "evil-caret": "^1.0.0",
                "evil-tilde": "~1.2.3",
                "evil-wildcard": "*",
                "evil-range": ">=1.0.0",
                "evil-file": "file:///tmp/evil.tgz",
                "BadName": "1.0.0",
                "@Scope/pkg": "1.0.0",
                "evil-alias": { "npm": "other@latest" },
                "evil-number": 42
            }
        });
        std::fs::write(&pkg_json, serde_json::to_string_pretty(&fixture).unwrap())
            .expect("package.json フィクスチャを書ける");

        let deps = read_validated_deps(&pkg_json);

        assert_eq!(deps.get("good-pkg"), Some(&serde_json::json!("1.2.3")));
        assert_eq!(deps.get("@scope/good"), Some(&serde_json::json!("0.0.13")));
        assert_eq!(
            deps.get("foo-bar_baz.qux"),
            Some(&serde_json::json!("1.2.3-beta.1+build.5"))
        );

        for name in [
            "evil-latest",
            "evil-caret",
            "evil-tilde",
            "evil-wildcard",
            "evil-range",
            "evil-file",
            "BadName",
            "@Scope/pkg",
            "evil-alias",
            "evil-number",
        ] {
            assert!(!deps.contains_key(name), "不正エントリが残った: {name}");
        }
        assert_eq!(deps.len(), 3, "正常な 3 エントリのみ残るべき");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_or_malformed_cache_yields_no_deps() {
        let dir = unique_temp_dir("cache-missing");
        let pkg_json = dir.join("package.json");
        assert!(
            read_validated_deps(&pkg_json).is_empty(),
            "存在しないキャッシュ"
        );

        std::fs::write(&pkg_json, "not valid json").unwrap();
        assert!(read_validated_deps(&pkg_json).is_empty(), "不正な JSON");

        std::fs::write(&pkg_json, r#"{"name":"no-deps"}"#).unwrap();
        assert!(
            read_validated_deps(&pkg_json).is_empty(),
            "dependencies 欠落"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn cache_dir_is_created_with_0700_and_existing_perms_are_corrected() {
        use std::os::unix::fs::PermissionsExt;

        let base = unique_temp_dir("cache-perms");
        let dir = base.join("nested").join("opencode");
        std::fs::create_dir_all(&dir).expect("一時キャッシュディレクトリを作成できる");
        // 既存ディレクトリが他ユーザーから書ける状態を模擬する。
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777))
            .expect("権限を設定できる");

        ensure_secure_cache_dir(&dir).expect("セキュアなキャッシュディレクトリを準備できる");

        let mode = std::fs::metadata(&dir)
            .expect("メタデータを読める")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "既存ディレクトリも 0700 に是正されるべき");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn ensure_secure_cache_dir_rejects_symlink_directory() {
        use std::os::unix::fs::symlink;

        let base = unique_temp_dir("cache-dir-symlink");
        let real = base.join("real-cache");
        let alias = base.join("alias-cache");
        std::fs::create_dir_all(&real).expect("実ディレクトリを作成できる");
        symlink(&real, &alias).expect("symlink を作成できる");

        let err = ensure_secure_cache_dir(&alias).unwrap_err();
        assert!(
            matches!(err, PluginLoaderError::CacheDirIsSymlink(_)),
            "symlink ディレクトリは拒否されるべき: {err}"
        );
        // symlink の先（実ディレクトリ）に chmod などの副作用が及ばないこと。
        let target_is_symlink = std::fs::symlink_metadata(&alias)
            .unwrap()
            .file_type()
            .is_symlink();
        assert!(target_is_symlink, "拒否された symlink 自体は残るべき");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn write_script_atomic_rejects_preexisting_symlink_without_following_it() {
        use std::os::unix::fs::symlink;

        let dir = unique_temp_dir("script-symlink");
        let target = dir.join("victim.ts");
        let script = dir.join("plugin-host.ts");
        std::fs::write(&target, b"do-not-touch").expect("ターゲットを作成できる");
        symlink(&target, &script).expect("symlink を作成できる");

        let err = write_script_atomic(&script, b"malicious").unwrap_err();
        assert!(
            matches!(err, PluginLoaderError::ScriptIsSymlink(_)),
            "symlink は拒否されるべき: {err}"
        );
        assert_eq!(
            std::fs::read(&target)
                .expect("ターゲットを読める")
                .as_slice(),
            &b"do-not-touch"[..],
            "symlink のターゲットは変更されないべき"
        );
        assert!(
            std::fs::symlink_metadata(&script)
                .unwrap()
                .file_type()
                .is_symlink(),
            "拒否された symlink 自体は残るべき"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn write_script_atomic_places_file_with_0600_and_no_temp_leftover() {
        use std::os::unix::fs::PermissionsExt;

        let dir = unique_temp_dir("script-atomic");
        let script = dir.join("plugin-host.ts");
        // 既存の通常ファイルが atomic に置き換わることも確認する。
        std::fs::write(&script, b"old content").expect("既存ファイルを作成できる");

        write_script_atomic(&script, b"new content").expect("スクリプトを原子配置できる");

        assert_eq!(
            std::fs::read(&script).unwrap().as_slice(),
            &b"new content"[..]
        );
        let mode = std::fs::metadata(&script).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "スクリプトは 0600 であるべき");

        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name())
            .filter(|name| name.to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "一時ファイルが残った: {leftovers:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_script_detects_tampering_and_symlink() {
        let dir = unique_temp_dir("script-verify");
        let script = dir.join("builtin.ts");
        let content = b"const ok = true;";
        write_script_atomic(&script, content).expect("スクリプトを配置できる");
        let expected = script_hash(content);

        verify_script(&script, expected).expect("改ざん前は検証を通過する");

        std::fs::write(&script, b"const evil = true;").expect("改ざんできる");
        assert!(matches!(
            verify_script(&script, expected),
            Err(PluginLoaderError::ScriptVerificationFailed(_))
        ));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let target = dir.join("elsewhere.ts");
            std::fs::write(&target, content).expect("ターゲットを作成できる");
            std::fs::remove_file(&script).expect("ファイルを削除できる");
            symlink(&target, &script).expect("symlink を作成できる");
            assert!(matches!(
                verify_script(&script, expected),
                Err(PluginLoaderError::ScriptIsSymlink(_))
            ));
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn script_hash_changes_with_content_and_length() {
        assert_ne!(script_hash(b"a"), script_hash(b"b"));
        assert_ne!(script_hash(b"ab"), script_hash(b"ba"));
        assert_ne!(script_hash(b"a"), script_hash(b"a\0"));
        assert_eq!(script_hash(b"same"), script_hash(b"same"));
    }

    #[test]
    fn script_hash_is_sha256_not_fnv1a() {
        // SHA-256("abc") の既知ベクタ。64-bit FNV-1a に戻っていないことを固定する。
        let expected: [u8; 32] = [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad,
        ];
        assert_eq!(script_hash(b"abc"), expected);
        let empty: [u8; 32] = Sha256::digest(b"").into();
        assert_eq!(script_hash(b""), empty);
    }

    /// Regression: install output larger than the OS pipe buffer must not hang
    /// and must not be accumulated in memory.
    ///
    /// The helper child writes well past the ~64 KiB pipe capacity to *both*
    /// stdout and stderr. With piped stdio the child would block on write and
    /// the wait would never return; `Stdio::null()` discards both streams as
    /// they are written, so `status()` returns promptly and no bytes are
    /// buffered. A timeout converts a regression into a failure instead of a
    /// hung test. Only the exit status is asserted — output is never logged.
    #[cfg(unix)]
    #[tokio::test]
    async fn install_command_drains_large_output_without_deadlock() {
        use std::time::Duration;

        let args = vec![
            "-c".to_string(),
            concat!(
                "head -c 262144 /dev/zero | tr '\\0' x >&2; ",
                "head -c 262144 /dev/zero | tr '\\0' y"
            )
            .to_string(),
        ];
        let cwd = std::env::temp_dir();

        let status = tokio::time::timeout(
            Duration::from_secs(10),
            PluginLoader::run_install_command("sh", &args, &cwd),
        )
        .await
        .expect("install-like command must finish; piped stdout/stderr must be drained")
        .expect("helper command must spawn");

        assert!(
            status.success(),
            "child must exit successfully once both pipes are drained"
        );
    }

    // -----------------------------------------------------------------------
    // R5 regression: fail-closed handling of plugin spawn failures
    // -----------------------------------------------------------------------

    use std::fmt::Write as _;
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    /// Minimal tracing capture so no `tracing-subscriber` dev-dependency is needed.
    #[derive(Clone, Default)]
    struct SpawnLogBuffer(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

    impl SpawnLogBuffer {
        fn lines(&self) -> Vec<String> {
            self.0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }
    }

    struct SpawnCaptureVisitor(String);

    impl Visit for SpawnCaptureVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            let _ = writeln!(self.0, "{}={:?}", field.name(), value);
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            let _ = writeln!(self.0, "{}={}", field.name(), value);
        }
    }

    struct SpawnCaptureSubscriber(SpawnLogBuffer);

    impl Subscriber for SpawnCaptureSubscriber {
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            *metadata.level() <= tracing::Level::ERROR
        }

        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }

        fn record(&self, _: &Id, _: &Record<'_>) {}

        fn record_follows_from(&self, _: &Id, _: &Id) {}

        fn event(&self, event: &Event<'_>) {
            let mut visitor = SpawnCaptureVisitor(String::new());
            event.record(&mut visitor);
            self.0
                 .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(visitor.0);
        }

        fn enter(&self, _: &Id) {}

        fn exit(&self, _: &Id) {}
    }

    /// `PluginLoader` directly, without runtime detection or cache writes.
    fn test_loader() -> PluginLoader {
        PluginLoader {
            clients: RwLock::new(Vec::new()),
            auth_bridges: RwLock::new(HashMap::new()),
            hook_system: Arc::new(PluginSystem::new()),
            runtime: JsRuntime::Node,
            host_script_path: PathBuf::from("/nonexistent/plugin-host.ts"),
            trusted_scripts: StdRwLock::new(Vec::new()),
        }
    }

    fn test_context() -> PluginContext {
        PluginContext {
            worktree: "/test/worktree".to_string(),
            directory: "/test/worktree".to_string(),
            server_url: "http://127.0.0.1:0".to_string(),
        }
    }

    #[tokio::test]
    async fn load_all_returns_err_on_first_spawn_failure() {
        let loader = test_loader();
        // file:// specs avoid npm install; the injected spawn always fails.
        let specs: Vec<String> = vec![
            "file:///secret/token-plugin-one.ts".to_string(),
            "file:///secret/token-plugin-two.ts".to_string(),
        ];
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_for_spawn = std::sync::Arc::clone(&calls);

        let err = loader
            .load_all_with_spawn(&specs, &test_context(), move |_, _, _, _, _| {
                let calls = std::sync::Arc::clone(&calls_for_spawn);
                async move {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Err::<PluginSubprocess, _>(PluginSubprocessError::NotRunning)
                }
            })
            .await
            .expect_err("spawn 失敗は Err を返すべき");

        assert!(
            matches!(&err, PluginLoaderError::PluginSpawnFailed),
            "値を保持しない専用バリアントが返るべき: {err}"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "最初の失敗で即 return し、後続の spawn は試みられないべき"
        );
    }

    #[test]
    fn load_all_spawn_failure_log_is_fixed_and_redacted() {
        let secret_spec = "file:///home/user/.cache/opencode/token-abc123/plugin.ts";
        let raw_error = "secret-raw-error /home/user/.cache/opencode/auth.json token-abc123";
        let specs = vec![secret_spec.to_string()];
        let raw_error_for_spawn = std::sync::Arc::new(raw_error.to_string());

        let logs = SpawnLogBuffer::default();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime for the regression test");
        // Install the subscriber around block_on so it is active exactly while
        // the future is polled (the event is emitted on poll, not on creation).
        let outcome =
            tracing::subscriber::with_default(SpawnCaptureSubscriber(logs.clone()), || {
                runtime.block_on(test_loader().load_all_with_spawn(
                    &specs,
                    &test_context(),
                    |_, _, _, _, _| {
                        let raw_error = std::sync::Arc::clone(&raw_error_for_spawn);
                        async move {
                            Err::<PluginSubprocess, _>(PluginSubprocessError::Protocol(
                                (*raw_error).clone(),
                            ))
                        }
                    },
                ))
            });

        assert!(outcome.is_err(), "spawn 失敗は Err を返すべき");
        let rendered = logs.lines().join("\n");
        assert!(
            rendered.contains("plugin_subprocess_spawn_failed"),
            "固定の event 名が記録されるべき: {rendered}"
        );
        assert!(
            rendered.contains("[REDACTED]"),
            "固定の [REDACTED] が記録されるべき: {rendered}"
        );
        assert!(!rendered.contains(secret_spec), "spec が漏れた: {rendered}");
        assert!(
            !rendered.contains("token-abc123"),
            "パス/トークンが漏れた: {rendered}"
        );
        assert!(
            !rendered.contains("secret-raw-error"),
            "生エラーが漏れた: {rendered}"
        );
        assert!(
            !rendered.contains("/home/user"),
            "キャッシュパスが漏れた: {rendered}"
        );
    }

    // -----------------------------------------------------------------------
    // R2 regression: resource accumulation across reloads (RES-07)
    // -----------------------------------------------------------------------

    /// WARN 以上のイベントを捕捉して redaction を検証するサブスクライバ。
    struct WarnCaptureSubscriber(SpawnLogBuffer);

    impl Subscriber for WarnCaptureSubscriber {
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            *metadata.level() <= tracing::Level::WARN
        }

        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }

        fn record(&self, _: &Id, _: &Record<'_>) {}

        fn record_follows_from(&self, _: &Id, _: &Id) {}

        fn event(&self, event: &Event<'_>) {
            let mut visitor = SpawnCaptureVisitor(String::new());
            event.record(&mut visitor);
            self.0
                 .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(visitor.0);
        }

        fn enter(&self, _: &Id) {}

        fn exit(&self, _: &Id) {}
    }

    #[test]
    fn trust_script_deduplicates_same_path_and_hash() {
        let loader = test_loader();
        let path = PathBuf::from("/cache/plugin.ts");
        let hash = [7u8; 32];

        loader.trust_script(TrustedScript {
            path: path.clone(),
            expected_hash: hash,
        });
        loader.trust_script(TrustedScript {
            path: path.clone(),
            expected_hash: hash,
        });
        assert_eq!(
            loader.trusted_scripts.read().unwrap().len(),
            1,
            "同一 path+hash は 1 エントリだけ保持されるべき"
        );

        loader.trust_script(TrustedScript {
            path: path.clone(),
            expected_hash: [8u8; 32],
        });
        assert_eq!(
            loader.trusted_scripts.read().unwrap().len(),
            2,
            "同一 path でも hash が違えば別エントリ"
        );

        loader.trust_script(TrustedScript {
            path: PathBuf::from("/cache/other.ts"),
            expected_hash: hash,
        });
        assert_eq!(
            loader.trusted_scripts.read().unwrap().len(),
            3,
            "同一 hash でも path が違えば別エントリ"
        );
    }

    #[test]
    fn shutdown_failure_log_is_fixed_and_redacted() {
        let raw_error =
            String::from("secret-token /home/user/.cache/opencode/auth.json token-abc123");
        let logs = SpawnLogBuffer::default();
        tracing::subscriber::with_default(WarnCaptureSubscriber(logs.clone()), || {
            log_shutdown_failure(&raw_error);
        });

        let rendered = logs.lines().join("\n");
        assert!(
            rendered.contains("plugin_shutdown_failed"),
            "固定の event 名が記録されるべき: {rendered}"
        );
        assert!(
            rendered.contains("[REDACTED]"),
            "固定の [REDACTED] が記録されるべき: {rendered}"
        );
        assert!(
            !rendered.contains("secret-token"),
            "トークンが漏れた: {rendered}"
        );
        assert!(
            !rendered.contains("token-abc123"),
            "トークンが漏れた: {rendered}"
        );
        assert!(!rendered.contains("/home/user"), "パスが漏れた: {rendered}");
        assert!(!rendered.contains("auth.json"), "パスが漏れた: {rendered}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_all_clears_registries_and_reload_does_not_accumulate() {
        use std::os::unix::fs::PermissionsExt;

        /// テスト終了時に PATH を必ず元へ戻すガード。
        struct PathGuard(Option<std::ffi::OsString>);
        impl Drop for PathGuard {
            fn drop(&mut self) {
                if let Some(path) = self.0.take() {
                    std::env::set_var("PATH", path);
                }
            }
        }

        let dir = unique_temp_dir("mock-node");
        let node = dir.join("node");
        // `node` の最小スタンドイン。initialize のリクエストフレームを読み飛ばし、
        // 固定の handshake レスポンスを返す。実ランタイムやプラグインは起動しない。
        let script = concat!(
            "#!/bin/sh\n",
            "IFS= read -r _header || true\n",
            "IFS= read -r _blank || true\n",
            "body='{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"name\":\"mock-plugin\",\"hooks\":[],",
            "\"auth\":{\"provider\":\"mock-provider\",\"methods\":[]}},\"error\":null}'\n",
            "printf 'Content-Length: %s\\r\\n\\r\\n%s' \"${#body}\" \"$body\"\n",
        );
        std::fs::write(&node, script).expect("mock node script");
        std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o700))
            .expect("mock node is executable");

        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let mut new_path = dir.as_os_str().to_os_string();
        new_path.push(":");
        new_path.push(&old_path);
        std::env::set_var("PATH", new_path);
        let _path_guard = PathGuard(Some(old_path));

        let loader = test_loader();
        let specs = vec!["file:///mock/plugin.ts".to_string()];

        loader
            .load_all(&specs, &test_context())
            .await
            .expect("initial load");
        assert_eq!(loader.clients().await.len(), 1);
        assert_eq!(loader.auth_bridges().await.len(), 1);

        loader.shutdown_all().await;
        assert!(
            loader.clients().await.is_empty(),
            "shutdown_all は clients を空にすべき"
        );
        assert!(
            loader.auth_bridges().await.is_empty(),
            "shutdown_all は auth_bridges も空にすべき"
        );

        loader
            .load_all(&specs, &test_context())
            .await
            .expect("reload");
        assert_eq!(
            loader.clients().await.len(),
            1,
            "再ロードで古い client が累積してはいけない"
        );
        assert_eq!(loader.auth_bridges().await.len(), 1);

        loader.shutdown_all().await;
        assert!(loader.clients().await.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
