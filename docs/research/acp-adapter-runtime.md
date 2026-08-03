# Research: ACP adapter JS-runtime dependency audit

**Ticket:** [deanjstone/Hearth#14](https://github.com/deanjstone/Hearth/issues/14)
**Builds on:** `docs/decisions/rust-tauri-feasibility.md` §2, §8b, §10.2 —
this resolves the "unconfirmed" flag on option 3 there.

**Question:** does either vendored ACP adapter have an execution path that
doesn't require a Node-compatible JS runtime — i.e. could a Rust/Tauri shell
invoke a native binary directly and skip JS entirely for that agent backend?

**Method:** fetched the exact package versions Hearth pins
(`package.json` lines 60, 78: `@agentclientprotocol/codex-acp@^0.0.44`,
`@zed-industries/claude-agent-acp@^0.23.1`) via `npm view` (registry
metadata) and `npm pack` (actual published tarball contents), extracted them,
and read the real `dist/*.js` source and `package.json` `bin`/`dependencies`
fields. No node_modules for these packages exists in this checkout
(`npm view`/`npm pack` fetched fresh from the registry). Also pulled their
runtime dependencies (`@openai/codex@0.128.0`, its platform package
`@openai/codex@0.128.0-linux-x64`, and `@anthropic-ai/claude-agent-sdk@0.2.83`)
the same way, since the real answer lives one layer down from the adapter
packages themselves.

---

## 1. `@agentclientprotocol/codex-acp` (Codex side)

**Answer: no non-JS path for the adapter itself. The underlying `codex` agent
core is genuinely native Rust — but `codex-acp` is not a thin shim around
it. It's a ~4,300-line-of-own-source TypeScript application that does real
protocol translation, and that part has no way to skip JS.**

### What's actually native

`@openai/codex` (the `codex-acp` dependency declared as `"@openai/codex":
"^0.128.0"` — confirmed via `npm view @agentclientprotocol/codex-acp@0.0.44
dependencies`) ships as:

- A tiny JS launcher: `bin/codex.js` (main npm package, `npm pack
  @openai/codex@0.128.0`) — 200 lines, whose entire job is to detect
  `platform`/`arch`, resolve the matching optional platform package
  (`@openai/codex-linux-x64` etc., declared in
  `optionalDependencies`), and `spawn(binaryPath, process.argv.slice(2), {
  stdio: "inherit" })` the real binary. No agent logic here.
- The real binary: `@openai/codex-linux-x64` (i.e.
  `npm:@openai/codex@0.128.0-linux-x64`, confirmed via `npm pack`) contains
  `vendor/x86_64-unknown-linux-musl/codex/codex` — confirmed via `file`:
  `ELF 64-bit LSB pie executable, x86-64, ... static-pie linked, stripped`.
  226 MB, statically linked musl target. **This is genuinely native Rust**,
  not a JS-in-disguise binary.

So OpenAI's actual agent core, "Codex," is a real native executable, exactly
as the feasibility doc speculated.

### What `codex-acp` actually is

`npm pack @agentclientprotocol/codex-acp@0.0.44` → `dist/index.js` is an
esbuild bundle, 21,006 lines total. Grepping for the bundler's `// src/*.ts`
source-file markers shows it's not just re-exporting bundled dependencies
(`vscode-jsonrpc`, `@agentclientprotocol/sdk`, `diff`, `open`) — the tail of
the bundle (lines ~16689–21006, ~4,300 lines) is `codex-acp`'s own source:

```
src/CodexJsonRpcConnection.ts
src/StdUtils.ts
src/Logger.ts
src/ACPSessionConnection.ts
src/TokenCount.ts
src/CodexToolCallMapper.ts
src/CommandUtils.ts
src/CodexEventHandler.ts
src/ApprovalOptionId.ts
src/CodexApprovalHandler.ts
src/CodexElicitationHandler.ts
src/CodexAuthMethod.ts
src/ModelId.ts
src/AgentMode.ts
src/CodexAcpClient.ts
src/CodexCommands.ts
src/AcpExtensions.ts
src/CodexAcpServer.ts
src/CodexAppServerClient.ts
src/login.ts
src/index.ts
```

That file list is the tell: this is session-state management
(`ACPSessionConnection`), tool-call shape translation
(`CodexToolCallMapper`), permission/approval routing
(`CodexApprovalHandler`, `CodexElicitationHandler`), and two separate
JSON-RPC layers (`CodexJsonRpcConnection`/`CodexAppServerClient` talking to
the native `codex` binary's own **"app-server" protocol**, and
`CodexAcpServer` speaking **ACP** on stdio to Hearth). These are two
different wire protocols and `codex-acp` is the translator between them —
real logic, not a passthrough.

Confirmed directly in `dist/index.js`:

```js
// line 16826
function startCodexConnection(codexPath, env) {
  if (codexPath) {
    codex = spawn(codexPath, ["app-server"], { env: spawnEnv });
  } else {
    const bundledCodexPath = createRequire(import.meta.url).resolve("@openai/codex/bin/codex.js");
    codex = spawn(process.execPath, [bundledCodexPath, "app-server"], { env: spawnEnv });
  }
}
```

`codex-acp` reads a `CODEX_PATH` env var (line 20900/20973) and, if set,
spawns that path directly with `["app-server"]` — so you *can* point it at
the native binary (`vendor/.../codex/codex`) and skip the tiny JS launcher
`bin/codex.js`. But that only removes the launcher, not `codex-acp` itself.
`codex-acp`'s own `dist/index.js` — the process that does session handling,
tool-call mapping, and ACP protocol framing — is still a Node.js entry point
(`#!/usr/bin/env node`, `bin: { "codex-acp": "dist/index.js" }`) that has to
run under a Node-compatible interpreter regardless of `CODEX_PATH`. The
native binary speaks Codex's own **app-server** JSON-RPC dialect, not ACP —
Hearth's `AcpClient` (`electron/main/agents/acp-client.ts`) only knows ACP,
so something has to sit between them, and that something is `codex-acp`'s
JS.

### One relevant thing this surfaced beyond the original question

`codex-acp`'s own `package.json` `scripts` (visible in `npm view
@agentclientprotocol/codex-acp@0.0.44 --json`) include:

```
"bundle:linux-x64": "bun build src/index.ts --minify --sourcemap --compile --target=bun-linux-x64-baseline --outfile dist/bin/codex-acp-x64-linux"
```

i.e. the *upstream project itself* has a build path that uses Bun's
`--compile` flag to produce a standalone native executable of `codex-acp`
(no npm package ships this — the published tarball only has `dist/index.js`
and the four other files listed above; `find extracted/codex-acp -type f`
returned only `package.json`, `LICENSE`, `README.md`, `dist/index.js`). This
wasn't part of the original question but is directly relevant to "avoid
bundling Node": rather than bundling a full Node.js sidecar (~50-100MB, the
option the feasibility doc rules out), it may be possible to build (or
obtain, if OpenAI/agentclientprotocol ever publish it as a release asset) a
Bun-compiled single-file `codex-acp` binary and ship *that* instead — much
smaller than a Node runtime, and it embeds Bun's own JS engine so no system
Node dependency exists at runtime either. This is not verified to exist as a
downloadable artifact today — only that the upstream repo's own scripts
build it that way. Worth a follow-up spike if the sidecar route is chosen
over "require system Node."

---

## 2. `@zed-industries/claude-agent-acp` (Claude side)

**Answer: confirmed no non-JS path, as the feasibility doc guessed — but
verified by reading source, not assumed.** Every layer here is JS/Node, all
the way down to the actual `claude` CLI binary.

`npm view @zed-industries/claude-agent-acp@0.23.1 dependencies`:
```json
{
  "zod": "^3.25.0 || ^4.0.0",
  "@agentclientprotocol/sdk": "0.17.0",
  "@anthropic-ai/claude-agent-sdk": "0.2.83"
}
```

No native binary dependency anywhere in the tree.

`npm pack @zed-industries/claude-agent-acp@0.23.1` → `dist/index.js` (26
lines, plain source not a bundle) is a thin entry that calls `runAcp()` from
`dist/acp-agent.js` (1,633 lines — session lifecycle, tool mapping,
permission-mode handling; this is what `electron/main/agents/claude.ts`'s
comments already correctly describe as "vendors the Claude Code CLI and
exposes it over ACP"). `dist/acp-agent.js` imports `query`,
`getSessionMessages`, `listSessions` from `@anthropic-ai/claude-agent-sdk`.

Traced one level further, into `@anthropic-ai/claude-agent-sdk@0.2.83`
itself (`npm pack`): its published files include `cli.js` (13MB) — `file
cli.js` reports `Node.js script executable, Unicode text, UTF-8 text, with
very long lines` (i.e. minified JS text, not a compiled binary), and its
header confirms it's the actual product: `// Version: 2.1.83` /
`// (c) Anthropic PBC.` — this is the real `claude` CLI, shipped as JS, not
a wrapper around a separate native binary. (The SDK also vendors
`ripgrep`/`tree-sitter-bash` native addons under `vendor/`, but those are
helper tools the CLI shells out to for search/parsing, not the agent core.)

`acp-agent.js` resolves and spawns this CLI directly:

```js
// dist/acp-agent.js line 27
export async function claudeCliPath() {
    return isStaticBinary()
        ? (await import("@anthropic-ai/claude-agent-sdk/embed")).default
        : import.meta.resolve("@anthropic-ai/claude-agent-sdk").replace("sdk.mjs", "cli.js");
}
```

and further down, when constructing the `query()` options passed into the
SDK:

```js
executable: isStaticBinary() ? undefined : process.execPath,
...(process.env.CLAUDE_CODE_EXECUTABLE
    ? { pathToClaudeCodeExecutable: process.env.CLAUDE_CODE_EXECUTABLE }
    : isStaticBinary()
        ? { pathToClaudeCodeExecutable: await claudeCliPath() }
        : {}),
```

`process.execPath` is passed as the interpreter to run `cli.js` with — the
exact same "use my own execPath as the Node interpreter" trick Hearth uses
today (`ELECTRON_RUN_AS_NODE=1`), just one layer further down the chain.
There's no branch here that reaches a native binary; `isStaticBinary()` (see
below) is the only other path, and it still runs the same JS, just
differently packaged.

**One nuance, parallel to the Codex-side Bun finding above:** the code
checks `process.env.CLAUDE_AGENT_ACP_IS_SINGLE_FILE_BUN` and, when set,
imports `@anthropic-ai/claude-agent-sdk/embed` instead of resolving
`cli.js` by path. That's evidence Zed has (or had) a Bun-`--compile`
single-file-binary distribution of `claude-agent-acp` for their own use
(same pattern as `codex-acp`'s `bundle:*` scripts) — but it does not change
the answer: a Bun-compiled binary still embeds and runs the same JS
(`cli.js`, `acp-agent.js`) inside Bun's own JS engine. It's a different
*packaging* of the JS runtime dependency, not an escape from it. No npm
tarball inspected here (`claude-agent-acp` or `claude-agent-sdk`) ships such
a compiled binary as a published artifact.

**Deprecation note (unrelated to the runtime question, but relevant to
future upkeep):** `npm view @zed-industries/claude-agent-acp@0.23.1` reports
`"deprecated": "This package has been renamed to
@agentclientprotocol/claude-agent-acp. Please migrate to continue receiving
updates."` The renamed package's `latest` (`0.64.2`) has the same dependency
shape (`@anthropic-ai/claude-agent-sdk` still present, now `0.3.220`) — same
conclusion applies, this isn't a version-specific quirk that newer releases
fix.

---

## 3. Summary answer to the ticket's question

Neither adapter has an execution path that avoids a Node-compatible JS
runtime:

- **Codex side:** the *agent core* (`@openai/codex`'s vendored binary) is
  genuinely native Rust and could in principle be driven directly from Rust
  by speaking its "app-server" JSON-RPC protocol — but nobody does that
  today; `codex-acp` is the thing that speaks that protocol, and it is a
  real ~4,300-line TypeScript application (session state, tool-call
  mapping, approval/elicitation routing, dual JSON-RPC framing) that has to
  run under Node/Bun regardless of whether it spawns the JS launcher or the
  native binary via `CODEX_PATH`. Skipping it means reimplementing all of
  that logic in Rust against Codex's app-server protocol directly — a real
  rewrite, not a "point Tauri at the binary" swap.
- **Claude side:** confirmed, not assumed — zero native binary anywhere in
  the dependency tree. `claude-agent-acp` wraps
  `@anthropic-ai/claude-agent-sdk`, which wraps the actual `claude` CLI
  (`cli.js`, 13MB of JS), spawned via `process.execPath`. Every layer is JS.

This closes out §10 risk #2 in the feasibility doc as "confirmed
unfavorable, but with one new mitigating option": sidecar-Node and
require-system-Node remain the two viable options from the original
3-option list; the "find non-JS adapters" option (§2 option 3) is now
answered **no** for both adapters, not just the Claude side. The one new
data point is that both `codex-acp` and (evidently) `claude-agent-acp` have
upstream Bun single-file-binary build paths, which — if reproducible — could
shrink the "sidecar" option's bundle-size cost significantly below a full
Node.js runtime, since it's a much smaller, JS-engine-embedded binary rather
than a general-purpose Node install. That's a new sub-option worth a short
follow-up spike (build `codex-acp` locally with `bun build --compile`,
measure the resulting binary size) before committing to "bundle full
Node.js" as the sidecar plan.

---

## Sources

- `npm view @agentclientprotocol/codex-acp@0.0.44 --json` (dependencies,
  bin, scripts)
- `npm view @openai/codex@0.128.0 bin optionalDependencies dependencies
  --json`
- `npm pack @openai/codex@0.128.0` → `bin/codex.js` (read in full)
- `npm pack @openai/codex@0.128.0-linux-x64` → `vendor/x86_64-unknown-linux-musl/codex/codex`
  (`file` output confirming native ELF binary)
- `npm pack @agentclientprotocol/codex-acp@0.0.44` → `dist/index.js` (21,006
  lines; `// src/*.ts` markers at lines 16689–21006 read directly, including
  `startCodexConnection` at line 16826 and `startAcpServer`/`CODEX_PATH`
  handling at lines 20900–20989)
- `npm view @zed-industries/claude-agent-acp@0.23.1 --json` (dependencies,
  bin, deprecation notice)
- `npm pack @zed-industries/claude-agent-acp@0.23.1` → `dist/index.js`
  (26 lines), `dist/acp-agent.js` (1,633 lines; `claudeCliPath()` at line
  27, `executable`/`pathToClaudeCodeExecutable` construction ~line 974)
- `npm view @anthropic-ai/claude-agent-sdk@0.2.83 dependencies bin main type
  --json`
- `npm pack @anthropic-ai/claude-agent-sdk@0.2.83` → `cli.js` (`file` output:
  `Node.js script executable, Unicode text ...`), `sdk.mjs`
- `npm view @agentclientprotocol/claude-agent-acp@latest dependencies
  repository --json` (renamed-package confirmation, same dependency shape at
  `0.64.2`)
- In-repo: `package.json` lines 60/78 (version pins), `electron/main/agents/codex.ts`,
  `electron/main/agents/claude.ts` (today's `ELECTRON_RUN_AS_NODE` spawn
  pattern this research is evaluated against)
