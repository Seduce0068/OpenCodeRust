/**
 * plugin-host.ts — JSON-RPC host for a single TypeScript plugin.
 *
 * Embedded into the Rust binary via include_str!() and written to
 * ~/.cache/opencode/plugin-host.ts at runtime.
 *
 * Protocol: Content-Length framed JSON-RPC 2.0 over stdin/stdout.
 * stderr is reserved for plugin log output.
 */

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface JsonRpcRequest {
  jsonrpc: "2.0";
  id: number;
  method: string;
  params?: Record<string, unknown>;
}

interface JsonRpcResponse {
  jsonrpc: "2.0";
  id: number;
  result?: unknown;
  error?: { code: number; message: string };
}

interface JsonRpcNotification {
  jsonrpc: "2.0";
  method: string;
  params?: unknown;
}

interface PluginContext {
  worktree: string;
  directory: string;
  serverUrl: string;
}

type UnknownRecord = Record<string, unknown>;

/**
 * Initialize handshake version. Must match `PROTOCOL_VERSION` in Rust's
 * `crates/opencode-plugin/src/subprocess/protocol.rs`.
 */
const PROTOCOL_VERSION = 1;

interface Hooks {
  [key: string]: ((input: unknown, output: unknown) => Promise<unknown>) | unknown;
}

interface AuthMethod {
  type: string;
  label: string;
  inputs?: Record<string, { placeholder?: string; required?: boolean }>;
}

interface AuthorizeResult {
  url?: string;
  instructions?: string;
  method?: string;
  callback?: (code?: string) => Promise<unknown>;
}

interface AuthHook {
  provider: string;
  methods: AuthMethod[];
  authorize?: (method: AuthMethod, inputs?: Record<string, string>) => Promise<AuthorizeResult>;
  loader?: () => Promise<{
    apiKey?: string;
    fetch?: typeof globalThis.fetch;
    [key: string]: unknown;
  }>;
}

// ---------------------------------------------------------------------------
// Content-Length framing
// ---------------------------------------------------------------------------

const encoder = new TextEncoder();
const decoder = new TextDecoder();

function encodeResponse(response: JsonRpcResponse): Uint8Array {
  const body = JSON.stringify(response);
  const header = `Content-Length: ${Buffer.byteLength(body)}\r\n\r\n`;
  return encoder.encode(header + body);
}

// ---------------------------------------------------------------------------
// stdin EOF tracking
// ---------------------------------------------------------------------------

/**
 * Minimal view of the Node stdin readable stream used by the framing reader.
 * `process.stdin` is cast to this so the host stays runtime-agnostic (Node and
 * Deno's node-compat both expose on/once/off). Kept conservative for
 * `deno check`: no runtime-specific types leak into the checked surface.
 */
interface StdinSource {
  read(size: number): Uint8Array | null;
  once(event: string, cb: () => void): void;
  on(event: string, cb: () => void): void;
  off(event: string, cb: () => void): void;
  readonly readableEnded?: boolean;
}

/**
 * True once stdin has emitted "end" or "close". Kept for the process lifetime
 * so readExact can tell "no buffered data right now" (read() === null) apart
 * from a real end-of-stream. Before this existed, readExact waited only on
 * "readable"; at EOF that event never fires again, so the main loop never
 * reached its normal `msg === null` exit and the child stayed alive waiting
 * for Rust's kill_on_drop.
 */
let stdinEnded = false;
let eofWatchInstalled = false;

function stdinAsSource(): StdinSource {
  return process.stdin as unknown as StdinSource;
}

/** Subscribe to the EOF signals exactly once. */
function installEofWatch(reader: StdinSource): void {
  if (eofWatchInstalled) return;
  eofWatchInstalled = true;
  const markEnded = () => {
    stdinEnded = true;
  };
  reader.on("end", markEnded);
  reader.on("close", markEnded);
}

/**
 * Read exactly `n` bytes from stdin.
 *
 * On EOF the returned slice is shorter than `n` (possibly empty). An empty
 * slice is the explicit EOF signal consumed by readMessage(), which maps it to
 * null so the main loop exits normally instead of hanging on "readable".
 *
 * Static verification note: this file is embedded via include_str!() and has
 * no TS-side test harness; the parent validates it with `deno check`. No
 * token or code value is ever logged here.
 */
async function readExact(n: number): Promise<Uint8Array> {
  const chunks: Uint8Array[] = [];
  let remaining = n;

  const reader = stdinAsSource();
  installEofWatch(reader);

  while (remaining > 0) {
    // EOF already observed (event-driven or via the stream's own flag).
    if (stdinEnded || reader.readableEnded) {
      stdinEnded = true;
      break;
    }

    const chunk: Uint8Array | null = reader.read(remaining);
    if (chunk !== null) {
      if (chunk.length === 0) {
        // Some runtimes report EOF with an empty buffer instead of null.
        stdinEnded = true;
        break;
      }
      chunks.push(chunk);
      remaining -= chunk.length;
      continue;
    }

    // read() === null means "no buffered data", not necessarily EOF. Wait for
    // either more data or an EOF signal; awaiting only "readable" is the
    // original hang. The stdinEnded re-check closes the race where EOF fired
    // between read() and listener registration.
    await new Promise<void>((resolve) => {
      const done = () => {
        reader.off("readable", done);
        reader.off("end", done);
        reader.off("close", done);
        resolve();
      };
      reader.once("readable", done);
      reader.once("end", done);
      reader.once("close", done);
      if (stdinEnded) done();
    });
  }

  const received = chunks.reduce((sum, c) => sum + c.length, 0);
  if (received === 0) return new Uint8Array(0); // EOF signal for readMessage
  const first = chunks[0];
  if (chunks.length === 1 && first !== undefined && received === n) return first;
  const result = new Uint8Array(received);
  let offset = 0;
  for (const c of chunks) {
    result.set(c, offset);
    offset += c.length;
  }
  return result;
}

/**
 * Read one Content-Length framed JSON-RPC message from stdin.
 * Returns null on EOF.
 */
async function readMessage(): Promise<JsonRpcRequest | null> {
  // Read header lines until empty line
  let header = "";
  while (true) {
    const byte = await readExact(1);
    if (byte.length === 0) return null;
    header += decoder.decode(byte);
    if (header.endsWith("\r\n\r\n")) break;
  }

  const match = header.match(/Content-Length:\s*(\d+)/i);
  if (!match) {
    throw new Error(`Invalid header: ${header}`);
  }

  const contentLength = parseInt(match[1], 10);
  const body = await readExact(contentLength);
  // A short body means stdin ended mid-frame. Map it to EOF (null) so the main
  // loop takes the normal exit path instead of throwing on a partial body.
  if (body.length < contentLength) return null;

  const parsed: unknown = JSON.parse(decoder.decode(body));
  const record =
    typeof parsed === "object" && parsed !== null ? (parsed as UnknownRecord) : null;
  if (record?.["jsonrpc"] !== "2.0") {
    const received = record?.["jsonrpc"];
    const message = `Invalid JSON-RPC version: expected "2.0", got ${JSON.stringify(received)}`;
    // Reply with a JSON-RPC error when an id is available; the main loop then
    // breaks and the host exits, so a mismatched peer cannot keep running.
    const id = record?.["id"];
    if (typeof id === "number") {
      sendError(id, -32600, message);
    }
    throw new Error(message);
  }
  return parsed as JsonRpcRequest;
}

function send(response: JsonRpcResponse): void {
  process.stdout.write(encodeResponse(response));
}

function sendNotification(method: string, params?: unknown): void {
  const body = JSON.stringify({
    jsonrpc: "2.0",
    method,
    params,
  } satisfies JsonRpcNotification);
  const header = `Content-Length: ${Buffer.byteLength(body)}\r\n\r\n`;
  process.stdout.write(encoder.encode(header + body));
}

function sendResult(id: number, result: unknown): void {
  send({ jsonrpc: "2.0", id, result });
}

function sendError(id: number, code: number, message: string): void {
  send({ jsonrpc: "2.0", id, error: { code, message } });
}

// ---------------------------------------------------------------------------
// Plugin state
// ---------------------------------------------------------------------------

let pluginHooks: Hooks = {};
let authHook: AuthHook | null = null;
let customFetch: typeof globalThis.fetch | null = null;

/**
 * Lifetime of an unconsumed OAuth flow. Expired flows are never invoked and
 * are lazily purged on the next authorize/callback (see purgeExpiredAuthFlows).
 * A timer is deliberately not used so the host's event loop is not kept alive
 * after stdin EOF.
 */
const AUTH_FLOW_TTL_MS = 10 * 60 * 1000;

interface PendingAuthFlow {
  callback: (code?: string) => Promise<unknown>;
  /** Wall-clock deadline: Date.now() at creation + AUTH_FLOW_TTL_MS. */
  expiresAt: number;
}

/**
 * One entry per in-flight authorize() call, keyed by the opaque flowId
 * returned to the caller. Replaces the old single-slot pendingAuthCallback so
 * concurrent OAuth flows cannot overwrite one another.
 */
const pendingAuthFlows = new Map<string, PendingAuthFlow>();

let authFlowCounter = 0;

/** Generate an opaque flow identifier (never derived from a token or code). */
function generateAuthFlowId(): string {
  const cryptoObj = (globalThis as UnknownRecord)["crypto"] as
    | { randomUUID?: () => string }
    | undefined;
  if (cryptoObj && typeof cryptoObj.randomUUID === "function") {
    return cryptoObj.randomUUID();
  }
  return `flow-${Date.now().toString(36)}-${(authFlowCounter++).toString(36)}-${
    Math.random().toString(36).slice(2)
  }`;
}

/** Remove every flow whose deadline has passed. No token/code is inspected. */
function purgeExpiredAuthFlows(now: number): void {
  for (const [flowId, flow] of pendingAuthFlows) {
    if (now >= flow.expiresAt) {
      pendingAuthFlows.delete(flowId);
    }
  }
}

// ---------------------------------------------------------------------------
// Plugin input compatibility helpers
// ---------------------------------------------------------------------------

function shellQuote(value: string): string {
  return `'${value.replace(/'/g, `'\\''`)}'`;
}

function templateToShellCommand(parts: TemplateStringsArray, values: unknown[]): string {
  let command = parts[0] ?? "";
  for (let i = 0; i < values.length; i++) {
    const value = values[i];
    const serialized =
      typeof value === "string"
        ? value
        : typeof value === "number" || typeof value === "boolean"
          ? String(value)
          : JSON.stringify(value);
    command += shellQuote(serialized) + (parts[i + 1] ?? "");
  }
  return command;
}

async function runShellCommand(command: string): Promise<{
  stdout: string;
  stderr: string;
  exitCode: number;
}> {
  const childProcess = await import("node:child_process");
  return await new Promise((resolve, reject) => {
    const child = childProcess.spawn("bash", ["-lc", command], {
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk: Buffer | string) => {
      stdout += String(chunk);
    });
    child.stderr.on("data", (chunk: Buffer | string) => {
      stderr += String(chunk);
    });
    child.on("error", (err: unknown) => {
      reject(err);
    });
    child.on("close", (code: number | null) => {
      const exitCode = code ?? 1;
      const result = { stdout, stderr, exitCode };
      if (exitCode === 0) {
        resolve(result);
        return;
      }
      reject(new Error(stderr || `Command failed with exit code ${exitCode}`));
    });
  });
}

function createShell(): (parts: TemplateStringsArray, ...values: unknown[]) => Promise<unknown> {
  const maybeBun = (globalThis as UnknownRecord)["Bun"];
  if (maybeBun && typeof maybeBun === "object") {
    const dollar = (maybeBun as UnknownRecord)["$"];
    if (typeof dollar === "function") {
      return dollar as (parts: TemplateStringsArray, ...values: unknown[]) => Promise<unknown>;
    }
  }

  return async (parts: TemplateStringsArray, ...values: unknown[]): Promise<unknown> => {
    const command = templateToShellCommand(parts, values);
    return await runShellCommand(command);
  };
}

function createNoopClientProxy(path: string[] = []): unknown {
  const fn = async () => ({});
  return new Proxy(fn, {
    get(_target, prop: string | symbol) {
      if (typeof prop === "symbol") {
        if (prop === Symbol.toStringTag) return "PluginNoopClient";
        return undefined;
      }
      if (prop === "then") return undefined;
      if (prop === "toString") {
        return () => `[PluginNoopClient ${path.join(".")}]`;
      }
      return createNoopClientProxy([...path, prop]);
    },
    apply() {
      return Promise.resolve({});
    },
  });
}

async function createPluginClient(
  context: PluginContext,
  pluginPath: string,
): Promise<unknown> {
  const candidateUrls = new Set<string>();
  const addCandidate = (url: string) => {
    candidateUrls.add(url);
  };

  try {
    const sdk = (await import("@opencode-ai/sdk")) as UnknownRecord;
    const createOpencodeClient = sdk["createOpencodeClient"];
    if (typeof createOpencodeClient === "function") {
      return (createOpencodeClient as (config: UnknownRecord) => unknown)({
        baseUrl: context.serverUrl,
        directory: context.directory,
      });
    }
  } catch {
    // Fallback below.
  }

  try {
    const pathMod = await import("node:path");
    const urlMod = await import("node:url");

    const addCandidatePath = (path: string) => {
      addCandidate(urlMod.pathToFileURL(path).href);
    };

    let pluginFsPath: string | null = null;
    if (pluginPath.startsWith("file://")) {
      pluginFsPath = urlMod.fileURLToPath(pluginPath);
    } else if (pluginPath.startsWith("/")) {
      pluginFsPath = pluginPath;
    }

    addCandidatePath(
      pathMod.join(
        process.cwd(),
        "node_modules",
        "@opencode-ai",
        "sdk",
        "dist",
        "index.js",
      ),
    );

    if (pluginFsPath) {
      let cursor = pathMod.dirname(pluginFsPath);
      while (true) {
        addCandidatePath(
          pathMod.join(
            cursor,
            "node_modules",
            "@opencode-ai",
            "sdk",
            "dist",
            "index.js",
          ),
        );
        const parent = pathMod.dirname(cursor);
        if (parent === cursor) break;
        cursor = parent;
      }
    }

    try {
      const moduleMod = await import("node:module");
      const addFromRequire = (basePath: string) => {
        try {
          const req = moduleMod.createRequire(basePath);
          const resolved = req.resolve("@opencode-ai/sdk");
          addCandidate(urlMod.pathToFileURL(resolved).href);
        } catch {
          // Try next base path.
        }
      };
      addFromRequire(pathMod.join(process.cwd(), "package.json"));
      if (pluginFsPath) {
        addFromRequire(pluginFsPath);
        addFromRequire(pathMod.join(pathMod.dirname(pluginFsPath), "package.json"));
      }
    } catch {
      // createRequire path resolution is optional.
    }
  } catch {
    // If node:module/node:url is unavailable, keep noop fallback.
  }

  for (const url of candidateUrls) {
    try {
      const sdk = (await import(url)) as UnknownRecord;
      const createOpencodeClient = sdk["createOpencodeClient"];
      if (typeof createOpencodeClient === "function") {
        return (createOpencodeClient as (config: UnknownRecord) => unknown)({
          baseUrl: context.serverUrl,
          directory: context.directory,
        });
      }
    } catch {
      // Try next candidate.
    }
  }

  return createNoopClientProxy(["client"]);
}

function buildPluginInput(context: PluginContext, client: unknown): UnknownRecord {
  let serverUrl: string | URL = context.serverUrl;
  try {
    serverUrl = new URL(context.serverUrl);
  } catch {
    // Keep as string if URL parsing fails.
  }

  return {
    // Legacy Rust host shape (already used by some plugins)
    context,
    // TS plugin ecosystem shape
    client,
    directory: context.directory,
    worktree: context.worktree,
    serverUrl,
    project: {
      directory: context.directory,
      worktree: context.worktree,
    },
    $: createShell(),
  };
}

// ---------------------------------------------------------------------------
// Plugin import-path validation (H-03 trust boundary)
// ---------------------------------------------------------------------------

// npm package name: lowercase alphanumerics plus `.`, `_`, `-`; must not start
// with `.` or `_`. Scoped names (`@scope/pkg`) are supported as well.
const NPM_NAME = "[a-z0-9][a-z0-9._-]*";
const NPM_BARE_RE = new RegExp(`^(?:@${NPM_NAME}/)?${NPM_NAME}$`);
// An optional version suffix is accepted for validation only; the version is
// never used as an import specifier (`import("pkg@1.2.3")` is unresolvable).
const NPM_VERSIONED_RE = new RegExp(
  `^(?:@${NPM_NAME}/)?${NPM_NAME}@[0-9][0-9A-Za-z.+-]*$`,
);

type ValidatedPluginPath =
  | { ok: true; importPath: string }
  | { ok: false; reason: string };

/**
 * Validate the value `resolve_plugin()` handed to the host *before* it is used
 * as an `import()` specifier, and derive the actual import target.
 *
 * Accepted forms:
 *   - an explicit local `file://` URL (imported in normalized, encoded form);
 *   - a bare npm package name, optionally scoped (e.g. `pkg`, `@scope/pkg`);
 *   - a `pkg@version` / `@scope/pkg@version` spec — the version is validated
 *     but stripped before import, because runtimes resolve bare specifiers
 *     against the shared `node_modules/` (cwd is set by the loader).
 *
 * Everything else (absolute/relative filesystem paths, `..` traversal, other
 * URL schemes such as `node:` / `data:`, empty or malformed values) is rejected
 * so `import()` cannot be pointed at an arbitrary local path or module.
 *
 * Note: the exact version is pinned at install time in
 * `install_npm_packages()` (package.json dependencies), not in this spec.
 */
function validatePluginImportPath(pluginPath: unknown): ValidatedPluginPath {
  if (typeof pluginPath !== "string" || pluginPath.length === 0) {
    return { ok: false, reason: "pluginPath must be a non-empty string" };
  }

  if (pluginPath.startsWith("file://")) {
    let url: URL;
    try {
      url = new URL(pluginPath);
    } catch {
      return { ok: false, reason: "malformed file:// pluginPath" };
    }
    if (url.protocol !== "file:") {
      return { ok: false, reason: "plugin file URL must use the file: protocol" };
    }
    // `file://host/...` is not a local file path (e.g. a UNC/SMB share).
    if (url.host !== "" && url.host !== "localhost") {
      return { ok: false, reason: "plugin file URL must be local" };
    }
    if (!url.pathname || url.pathname === "/") {
      return { ok: false, reason: "plugin file URL must include a file path" };
    }

    // Percent-encoded dot segments (`%2e%2e`, `.%2e`, ...) must not be able to
    // smuggle a `..` segment past the check below. A stray '%' that is not a
    // valid escape is not a traversal vector, so fall back to the raw path.
    let decodedPathname = url.pathname;
    try {
      decodedPathname = decodeURIComponent(url.pathname);
    } catch {
      // Keep the raw pathname for the segment check.
    }
    if (
      url.pathname.split("/").includes("..") ||
      decodedPathname.split("/").includes("..")
    ) {
      return { ok: false, reason: "plugin file URL must not contain '..' traversal" };
    }

    // Import the URL parser's normalized, still percent-encoded form. The
    // loader emits `Url::from_file_path()`-style encoded URLs, so this is a
    // no-op for legitimate paths while collapsing any dot segments the parser
    // already resolved (the original untrusted string is never imported).
    return { ok: true, importPath: url.href };
  }

  if (NPM_VERSIONED_RE.test(pluginPath)) {
    // lastIndexOf('@') is the version separator here: scoped names have '@'
    // only at index 0, and the version suffix is the final one.
    return { ok: true, importPath: pluginPath.slice(0, pluginPath.lastIndexOf("@")) };
  }

  if (NPM_BARE_RE.test(pluginPath)) {
    return { ok: true, importPath: pluginPath };
  }

  return {
    ok: false,
    reason:
      "pluginPath must be a bare npm package name, a pkg@version spec, or an explicit file:// URL",
  };
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async function handleInitialize(
  id: number,
  params: { pluginPath: string; context: PluginContext; protocolVersion?: number },
): Promise<void> {
  // Both sides advertise the version. A missing field is accepted for
  // compatibility with older clients (treated as the current version), but an
  // explicit mismatch fails the handshake.
  if (
    params?.protocolVersion !== undefined &&
    params.protocolVersion !== PROTOCOL_VERSION
  ) {
    sendError(
      id,
      -32600,
      `Protocol version mismatch: expected ${PROTOCOL_VERSION}, got ${params.protocolVersion}`,
    );
    return;
  }

  const validated = validatePluginImportPath(params?.pluginPath);
  if (!validated.ok) {
    sendError(id, -32602, `Rejected plugin import: ${validated.reason}`);
    return;
  }

  try {
    const mod = await import(validated.importPath);
    const pluginFn = mod.default ?? mod;

    if (typeof pluginFn !== "function") {
      sendError(id, -32600, "Plugin module does not export a function");
      return;
    }

    // Build a PluginInput compatible with upstream @opencode-ai/plugin shape,
    // while keeping `context` for backward compatibility.
    const client = await createPluginClient(params.context, validated.importPath);
    const pluginInput = buildPluginInput(params.context, client);

    const hooks: Hooks = await pluginFn(pluginInput);
    pluginHooks = hooks;

    // Collect hook names
    const hookNames: string[] = [];
    for (const key of Object.keys(hooks)) {
      if (key === "auth" || key === "event" || key === "config") continue;
      if (typeof hooks[key] === "function") {
        hookNames.push(key);
      }
    }

    // Extract auth metadata if present
    let authMeta: { provider: string; methods: AuthMethod[] } | undefined;
    if (hooks.auth && typeof hooks.auth === "object") {
      authHook = hooks.auth as AuthHook;
      authMeta = {
        provider: authHook.provider,
        methods: authHook.methods.map((m) => ({
          type: m.type,
          label: m.label,
        })),
      };
    }

    sendResult(id, {
      protocolVersion: PROTOCOL_VERSION,
      name: validated.importPath.split("/").pop()?.replace(/\.[tj]s$/, "") ?? "unknown",
      hooks: hookNames,
      auth: authMeta,
    });
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : String(err);
    sendError(id, -32603, `Failed to initialize plugin: ${msg}`);
  }
}

async function handleHookInvoke(
  id: number,
  params: { hook: string; input: unknown; output: unknown },
): Promise<void> {
  const handler = pluginHooks[params.hook];
  if (typeof handler !== "function") {
    sendError(id, -32601, `Hook not found: ${params.hook}`);
    return;
  }

  try {
    // TS parity: `config` hooks mutate the first argument in-place.
    // Use one shared object for both input/output so in-place edits are preserved.
    if (params.hook === "config") {
      const seed =
        (params.output as UnknownRecord | null) ??
        (params.input as UnknownRecord | null) ??
        ({} as UnknownRecord);
      const result = await handler(seed, seed);
      sendResult(id, { output: result ?? seed });
      return;
    }

    const result = await handler(params.input, params.output);
    sendResult(id, { output: result ?? params.output });
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : String(err);
    sendError(id, -32603, `Hook ${params.hook} failed: ${msg}`);
  }
}

async function handleAuthAuthorize(
  id: number,
  params: { methodIndex: number; inputs?: Record<string, string> },
): Promise<void> {
  if (!authHook?.authorize) {
    sendError(id, -32601, "No auth.authorize handler");
    return;
  }

  try {
    const method = authHook.methods[params.methodIndex];
    if (!method) {
      sendError(id, -32602, `Invalid method index: ${params.methodIndex}`);
      return;
    }

    const result = await authHook.authorize(method, params.inputs);
    const payload: UnknownRecord = {
      url: result.url,
      instructions: result.instructions,
      method: result.method,
    };

    if (result.callback) {
      const now = Date.now();
      purgeExpiredAuthFlows(now);
      const flowId = generateAuthFlowId();
      pendingAuthFlows.set(flowId, {
        callback: result.callback,
        expiresAt: now + AUTH_FLOW_TTL_MS,
      });
      // New clients correlate callback() with this id. Older clients ignore
      // the extra field, so the wire response stays backward compatible.
      payload["flowId"] = flowId;
    }

    sendResult(id, payload);
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : String(err);
    sendError(id, -32603, `auth.authorize failed: ${msg}`);
  }
}

async function handleAuthCallback(
  id: number,
  params: { code?: string; flowId?: string },
): Promise<void> {
  const now = Date.now();
  purgeExpiredAuthFlows(now);

  let flowId = params?.flowId;
  if (flowId === undefined) {
    // Backward compatibility: legacy single-flow callers omit flowId.
    // Use the sole pending entry; never guess when several flows are in flight,
    // since that would reintroduce the wrong-flow overwrite this change fixes.
    const pendingIds = [...pendingAuthFlows.keys()];
    if (pendingIds.length === 0) {
      sendError(id, -32601, "No pending auth callback");
      return;
    }
    if (pendingIds.length > 1) {
      sendError(id, -32602, "Multiple pending auth flows: flowId is required");
      return;
    }
    const [onlyPendingId] = pendingIds;
    if (onlyPendingId === undefined) {
      sendError(id, -32601, "No pending auth callback");
      return;
    }
    flowId = onlyPendingId;
  }

  const flow = pendingAuthFlows.get(flowId);
  if (!flow) {
    sendError(id, -32601, "No pending auth callback for the given flowId");
    return;
  }

  try {
    const result = await flow.callback(params?.code);
    // Consume only this flow; parallel OAuth flows remain pending.
    pendingAuthFlows.delete(flowId);
    sendResult(id, result);
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : String(err);
    sendError(id, -32603, `auth.callback failed: ${msg}`);
  }
}

async function handleAuthLoad(id: number): Promise<void> {
  if (!authHook?.loader) {
    sendError(id, -32601, "No auth.loader handler");
    return;
  }

  try {
    const loaded = await authHook.loader();
    const hasCustomFetch = typeof loaded.fetch === "function";
    if (hasCustomFetch) {
      customFetch = loaded.fetch!;
    }

    sendResult(id, {
      apiKey: loaded.apiKey,
      hasCustomFetch,
    });
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : String(err);
    sendError(id, -32603, `auth.load failed: ${msg}`);
  }
}

async function handleAuthFetch(
  id: number,
  params: { url: string; method: string; headers: Record<string, string>; body?: string },
): Promise<void> {
  if (!customFetch) {
    sendError(id, -32601, "No custom fetch available");
    return;
  }

  try {
    const resp = await customFetch(params.url, {
      method: params.method,
      headers: params.headers,
      body: params.body,
    });

    const respHeaders: Record<string, string> = {};
    resp.headers.forEach((v: string, k: string) => {
      respHeaders[k] = v;
    });

    const body = await resp.text();
    sendResult(id, {
      status: resp.status,
      headers: respHeaders,
      body,
    });
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : String(err);
    sendError(id, -32603, `auth.fetch failed: ${msg}`);
  }
}

async function handleAuthFetchStream(
  id: number,
  params: { url: string; method: string; headers: Record<string, string>; body?: string },
): Promise<void> {
  if (!customFetch) {
    sendError(id, -32601, "No custom fetch available");
    return;
  }

  try {
    const resp = await customFetch(params.url, {
      method: params.method,
      headers: params.headers,
      body: params.body,
    });

    const respHeaders: Record<string, string> = {};
    resp.headers.forEach((v: string, k: string) => {
      respHeaders[k] = v;
    });

    // First response carries status/headers so Rust can begin the stream pipeline.
    sendResult(id, {
      status: resp.status,
      headers: respHeaders,
    });

    if (!resp.body) {
      sendNotification("auth.fetch.stream.end", { requestId: id });
      return;
    }

    const reader = resp.body.getReader();
    const decoder = new TextDecoder();
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      if (!value || value.length === 0) continue;
      const chunk = decoder.decode(value, { stream: true });
      if (chunk.length === 0) continue;
      sendNotification("auth.fetch.stream.chunk", {
        requestId: id,
        chunk,
      });
    }

    const rest = decoder.decode();
    if (rest.length > 0) {
      sendNotification("auth.fetch.stream.chunk", {
        requestId: id,
        chunk: rest,
      });
    }
    sendNotification("auth.fetch.stream.end", { requestId: id });
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : String(err);
    sendNotification("auth.fetch.stream.error", {
      requestId: id,
      message: msg,
    });
    sendNotification("auth.fetch.stream.end", { requestId: id });
  }
}

// ---------------------------------------------------------------------------
// Main loop
// ---------------------------------------------------------------------------

async function main(): Promise<void> {
  // Set stdin to raw binary mode
  if (typeof process.stdin.setEncoding === "function") {
    // Don't call setEncoding — we want raw bytes
  }
  process.stdin.resume();

  while (true) {
    let msg: JsonRpcRequest | null;
    try {
      msg = await readMessage();
    } catch {
      break; // stdin closed or parse error
    }

    if (msg === null) break; // EOF

    const { id, method, params } = msg;

    switch (method) {
      case "initialize":
        await handleInitialize(id, params as Parameters<typeof handleInitialize>[1]);
        break;
      case "hook.invoke":
        await handleHookInvoke(id, params as Parameters<typeof handleHookInvoke>[1]);
        break;
      case "auth.authorize":
        await handleAuthAuthorize(id, params as Parameters<typeof handleAuthAuthorize>[1]);
        break;
      case "auth.callback":
        await handleAuthCallback(id, params as Parameters<typeof handleAuthCallback>[1]);
        break;
      case "auth.load":
        await handleAuthLoad(id);
        break;
      case "auth.fetch":
        await handleAuthFetch(id, params as Parameters<typeof handleAuthFetch>[1]);
        break;
      case "auth.fetch.stream":
        await handleAuthFetchStream(id, params as Parameters<typeof handleAuthFetchStream>[1]);
        break;
      case "shutdown":
        sendResult(id, {});
        process.exit(0);
      default:
        sendError(id, -32601, `Unknown method: ${method}`);
    }
  }
}

main().catch((err) => {
  process.stderr.write(`plugin-host fatal: ${err}\n`);
  process.exit(1);
});
