# Electron → Rust/Tauri: feasibility assessment

**Status:** research only, not a decision. No implementation has started. This
is not an ADR — it's an input to one, if we decide to write one later.

**Scope of the question:** keep `src/**` (React/TS frontend) unchanged, and
replace the Electron shell (`electron/main/**`, `electron/preload/**`) with a
Tauri (Rust) shell that loads the same frontend in a webview.

**Method:** read the actual source, not the docs. Everything below is graded
against the current code in this repo, not against Electron/Tauri feature
lists in the abstract.

---

## 1. Self-mod engine (`electron/main/self-mod/**`, `boot-watchdog.ts`)

**Classification: needs adaptation (mechanical rewrite, low risk).**

- `scope-guard.ts` (`classifyWrite`) — pure function over `node:path`/`node:os`
  string logic, no Electron or even async I/O involved. Translates to Rust
  almost line-for-line (`std::path`, `std::env`). Because this is the actual
  security boundary (protected files have *no* approval escape hatch, by
  design), a Rust reimplementation is arguably a strict improvement: the
  boundary would live in compiled, non-self-modifiable code, closing off a
  theoretical "agent edits the guard that constrains it" path that today only
  doesn't happen because the protected-file list includes `self-mod/` itself.
- `boot-watchdog.ts` — JSON marker file, arm/confirm/inspect state machine,
  bounded revert attempts. Plain `fs`/`path`. Trivial port.
- `self-mod-service.ts` — orchestration (rejecting protected/blocked writes
  pre-commit, per-subagent-group commits, blocking typecheck gate before
  reload, undo/redo via git revert). All portable logic; its only
  Electron-shaped dependency is the `HmrController` it calls into (see §5,
  reload driver), which is a clean injected interface already.

None of this reads or writes Electron APIs. It's Node-flavored TypeScript
using only `fs`/`path`/`os`/`child_process`-adjacent primitives, all of which
have direct Rust stdlib equivalents. This is the most straightforward area in
the whole port — the work is translation effort, not architecture.

---

## 2. ACP agent bridge (`electron/main/agents/**`)

**Classification: needs adaptation for the protocol layer, but blocked by
one hard dependency: a JS runtime to execute the vendored adapters.**

The ACP session/prompt/permission logic (`acp-client.ts`, `agent-host.ts`,
`claude.ts`, `codex.ts`) is transport-agnostic JSON-RPC over stdio. A Rust
ACP client is realistic — Zed publishes an `agent-client-protocol` Rust
crate for exactly this protocol, since ACP is their protocol. Rewriting
`AcpClient`'s session lifecycle, `ndJsonStream` handling, and permission
routing in Rust is real work (~360 lines today, non-trivial state machine)
but is *scoped* work against a known target, not exploratory.

The hard part is what those adapters *are*: `@zed-industries/claude-agent-acp`
and `@agentclientprotocol/codex-acp` are **npm packages** — JS entry points
that wrap the real `claude`/`codex` CLIs. Today, Hearth runs them via a neat
trick: `spawn(process.execPath, [bin], { env: { ELECTRON_RUN_AS_NODE: '1' } })`
— i.e. it uses the Electron binary itself as a Node interpreter, so there's
no dependency on the user having Node.js installed separately. Electron
ships Node for exactly this reason.

A Tauri binary has no bundled JS engine. To keep using these adapters as-is,
a Rust shell has three options, and none of them is "nothing changes":

1. **Bundle a real Node.js binary as a Tauri sidecar** and spawn adapters
   through it. Tauri's sidecar mechanism supports this cleanly, but it puts
   the ~50-100MB Node runtime back into the app bundle — one of the main
   things a Tauri port is usually done *to avoid*.
2. **Require system Node.js** and shell out to it. Smaller bundle, but a real
   regression in "just works" UX — Electron's whole point here was not
   needing this.
3. **Get non-JS adapters.** Not fully verified in this pass, but worth an
   explicit sub-spike: OpenAI's Codex CLI core is itself written in Rust, so
   `@agentclientprotocol/codex-acp` may turn out to be a thin JS shim around a
   Rust/native binary rather than a JS implementation proper — if so, the
   Codex side could plausibly speak ACP without needing Node at all. This is
   **unconfirmed** and should be checked before estimating around it; the
   Claude side (`@zed-industries/claude-agent-acp`) wraps the `claude` CLI,
   which is itself a JS/Node program, so option 3 likely does not apply there
   regardless.

This is the single biggest "the assumption behind the estimate might be
wrong" risk in the whole port (see §9).

---

## 3. node-pty terminal integration

**Classification: needs adaptation — and a rare case where Rust is a clear
win, not just parity.**

`electron/main/terminal/pty.ts` wraps `node-pty`, a native Node addon that
requires `electron-rebuild` against Electron's ABI (`postinstall:
"electron-rebuild -f -w node-pty"` in `package.json` — a real, currently-paid
build-complexity tax). The Rust equivalent, `portable-pty` (from the wezterm
project), is mature, widely used in production terminal apps, and — since
this fork already targets Linux only per the project's `CLAUDE.md` — the
cross-platform PTY quirks that make `portable-pty` occasionally fiddly on
Windows don't even apply here.

The `TerminalManager` logic around it (`defaultShell()` resolution,
`loginPath()` merging, credential-scrubbing via `buildChildEnv(...,
{ scrubInheritedKeys: true })`) is ordinary process/env logic with no
Electron dependency — ports directly. Net effect: this area gets *simpler* in
Rust, not just equivalent, because the native-module-ABI problem disappears
entirely.

---

## 4. Git operations (dugite)

**Classification: needs adaptation, not a rewrite of the logic — the
approach that transfers cleanest is "keep shelling out to git."**

`electron/main/self-mod/git.ts` (333 lines) does everything through dugite's
`exec`, which spawns a bundled git binary with an argv array — no shell, no
injection surface. The actual git usage is deep: porcelain status parsing,
rename detection, `git revert` with conflict handling, and — notably — a
hand-rolled net-effect undo/redo graph algorithm that reads structured
`Hearth-*` trailers out of commit bodies (`parseRevertTarget`,
`buildRevertGraph`, `isApplied`, `effectiveRevertOf`).

Two real Rust options exist:
- **`git2-rs`** (libgit2 bindings) — supports revert and most plumbing, but
  trailer-based commit-body parsing and some porcelain-equivalent output
  aren't first-class; you'd be walking the commit graph manually rather than
  parsing CLI output.
- **`gix`** (pure-Rust git) — earlier-maturity for some operations used here
  (revert-with-conflict semantics specifically).
- **Shell out to a bundled `git` binary via `std::process::Command`,
  argv-array, no shell** — the same spawn model dugite already uses, and the
  string-parsing logic in `git.ts` transliterates to Rust close to 1:1,
  since it's not really "using dugite," it's using git's CLI output shape,
  which dugite just gives you a typed wrapper over.

The third option is the pragmatic one: it preserves the exact command
surface and conflict-handling behavior that's already been debugged, at the
cost of being "less idiomatic Rust" than `git2-rs`. This is moderate,
well-scoped rewrite effort, not a design risk.

---

## 5. IPC surface (`window.hearth`, `electron/preload/index.ts`, `ipc.ts`)

**Classification: needs full rewrite — large surface area, but mechanical
once the pattern is set.**

The preload exposes `window.hearth` via `contextBridge.exposeInMainWorld`
across roughly 20 namespaces (`agent`, `permission`, `view`, `selfMod`,
`git`, `workspaces`, `sessions`, `routines`, `terminal`, `files`, `browser`,
`personality`, `memory`, `secrets`, `auth`, `mcp`, `skills`, `data`, `about`,
`update`, `win`, `microApps`, `morph`, `cursor`) and ~90+ methods, each
following one of three shapes: `invoke` (request/response), `send`
(fire-and-forget), `on` (subscribe to main→renderer events).

Tauri's model (`#[tauri::command]` + `invoke`, plus `emit`/`listen` for
events) maps conceptually 1:1 onto this pattern. Since `src/**` is explicitly
staying unchanged, the practical plan is: keep every call site as
`window.hearth.agent.prompt(...)` etc., and write a thin JS compatibility
shim (loaded instead of Electron's preload) that translates each of those
~90 calls into a Tauri `invoke('agent_prompt', ...)` /
`listen('agent:event', ...)` call. The shim is boilerplate-heavy but low-risk
— it's a systematic transformation, not a design problem. The real cost is
reimplementing every one of those ~90 handlers' *behavior* in Rust on the
backend side, which isn't really "IPC cost," it's the sum of all the other
sections in this document (agents, terminal, git, secrets, etc.) — the IPC
layer itself is just the wiring between them.

---

## 6. Auto-update

**Classification: needs adaptation — the cleanest 1:1 swap in the whole
port.**

`electron/main/updater.ts` wraps `electron-updater`'s `autoUpdater` against a
generic-provider feed (currently a public Cloudflare R2 URL), with
`autoDownload: true` / `autoInstallOnAppQuit: false`, and it coordinates with
the self-mod boot-watchdog marker so an update doesn't race a pending
self-mod restart. `tauri-plugin-updater` (official) supports the same
generic/static-JSON-feed model. The signing mechanism changes (Apple
code-signing/Squirrel-style vs. minisign keypairs for Tauri), and the
boot-watchdog coordination logic needs re-wiring against whatever event hooks
the Tauri plugin exposes, but there's no conceptual gap here — this is
solid, well-trodden ground.

---

## 7. MCP server integration (`electron/main/mcp/**`)

**Classification: needs adaptation — no Electron dependency, moderate
rewrite.**

- `registry.ts` — plain JSON-persisted CRUD for server configs, secrets
  referenced by key rather than stored inline. Pure `fs`/`crypto`. Trivial
  port.
- `to-acp.ts` — pure mapping function (config + secret lookup → ACP
  `McpServer` wire shape). Trivial port; no I/O at all.
- `probe.ts` — spawns a configured MCP server and runs a real
  `initialize`/`tools/list` handshake over stdio (newline-delimited
  JSON-RPC), or does a reachability check for http/sse transports. This is
  `node:child_process.spawn` + manual JSON-RPC framing over stdio — no
  Electron dependency, and no dependency on the Node MCP SDK either (the
  JSON-RPC handshake is hand-rolled here, not delegated to
  `@modelcontextprotocol/sdk`). Ports directly to
  `std::process::Command`/`tokio::process` in Rust; if a stronger typed layer
  is wanted later, an official Rust MCP SDK (`rmcp`) exists and is maturing,
  but isn't required — the hand-rolled approach already used here is
  actually easier to port than reaching for an SDK would be.

This whole area is a clean, contained rewrite — probably a day or two of
actual Rust work given the logic is already fully specified by the existing
TypeScript.

---

## 8. Areas the user's list didn't name but the code makes load-bearing

These showed up while reading the actual `main/**` tree and materially
affect the estimate, so they're included rather than left out for being
off-script.

### 8a. The self-mod HMR trick (`electron/vite-plugins/self-mod-overlay.ts`)

**Classification: reusable as-is, in principle — but this is the load-bearing
unknown for the whole feature, and needs empirical verification, not just
architectural reasoning.**

This is the mechanism that lets a parallel multi-file agent turn apply
atomically without the live UI flickering through half-edited state: a Vite
dev-server plugin that pins touched modules to their pre-edit baseline
content (via the `load()` hook), wraps Vite's internal HMR websocket `send`
across whichever channel shape the running Vite version uses (`ws`, `hot`,
`environments.client.hot`) to drop outgoing `full-reload` messages while a
turn is active, then swaps every pinned module in one atomic pass via
`server.reloadModule()` when the turn ends.

The critical fact: **this plugin runs inside the Vite dev-server process,
which is a separate process from Electron main already** — coordination
happens over a tiny HTTP endpoint (`/__hearth/self-mod`), not over any
Electron IPC channel. It has zero references to `electron`. It is a Vite
plugin, full stop. That means it is *architecturally* portable to a Tauri
shell without modification: Tauri, like Electron, just points a webview at a
URL; if that URL is still served by this same Vite dev server, the plugin
keeps working exactly as today, regardless of what's rendering the page.

What is **not** verified — and can't be settled by reading source, only by
running it — is whether the *webview* backend on Linux behaves the same as
Chromium here. Tauri on Linux uses **WebKitGTK**, not Chromium. This project
already runs Electron/Chromium exclusively (per this fork's `CLAUDE.md`,
it's Linux-only), so there is zero existing evidence about how WebKitGTK's
websocket reconnect timing, module-graph reload behavior, or dev-tools-esque
quirks compare. Concretely at risk: Vite's HMR client script itself (which
`self-mod-overlay.ts` doesn't own — it only wraps the *server* side) has to
behave identically enough on WebKitGTK for the client-side half of this
dance to hold up. This is **the single most important thing to prototype**
before committing further (see §10).

### 8b. "Electron-as-Node" as a general-purpose trick, not just for ACP adapters

Beyond the ACP adapters (§2), the same `process.execPath` +
`ELECTRON_RUN_AS_NODE=1` pattern is also used to run the Hearth MCP bridge
server script (`electron/main/agent-tools/hearth-mcp-server.mjs`) — i.e.
Electron's bundled Node is being used as a general "run this vendored JS"
capability, not a one-off. Every place this pattern appears needs the same
sidecar-Node-or-require-system-Node tradeoff from §2. It's the same risk
counted once, not a new one, but worth naming so the estimate doesn't
undercount how many call sites depend on it.

### 8c. Live UI capture + scripting (`electron/main/agent-bridge.ts`)

**Classification: needs full rewrite, with a real capability gap.**

This is the mechanism behind `view_app`/`read_ui`/`click`/`fill`/`eval_js` —
the tools this very session is using to see and drive the running app. It's
a loopback HTTP server (token-authed, DNS-rebinding-guarded) that does two
things Electron makes easy and Tauri does not have an equally mature
built-in for:

- **`webContents.capturePage()`** — pixel-perfect screenshot of a
  `BrowserWindow`'s rendered content, including an off-screen hidden window
  for route captures that don't disturb the user's actual view. Tauri v2
  does not ship an equivalent "capture this webview to a PNG buffer" API as
  a first-class primitive; WebKitGTK doesn't expose the same
  render-to-buffer hook Chromium's CDP gives Electron for free. Getting this
  in a Tauri/WebKitGTG world realistically means either OS-level screen
  capture (X11/Wayland-specific, and doesn't cleanly support "capture a
  hidden off-screen window") or a JS-side canvas-based capture injected into
  the page (imperfect — won't capture things outside the DOM's own paint
  path the way a real compositor-level screenshot does).
- **`webContents.executeJavaScript(code, true)`** — arbitrary JS execution
  in the live page with a returned, JSON-serialized result. Tauri's
  `WebviewWindow::eval()` can run JS, but getting a return value back is not
  as direct as Electron's `executeJavaScript` promise — usually done via a
  round-trip through `invoke`/event, which is workable but is a real
  redesign of this bridge, not a drop-in swap.

This is squarely in the "needs full rewrite, and parity is genuinely
uncertain" bucket — it's a second strong candidate for a top risk (see §10),
separate from the HMR question, because it affects the agent's own
self-observation loop, which is core to what makes Hearth self-mod work at
all (an agent that can't see its own UI after an edit can't verify the edit
worked).

### 8d. Morph/overlay window + session-level CSP enforcement

- `windows/overlay-window.ts` — a frameless, transparent, always-on-top,
  multi-display-spanning `BrowserWindow` used for the flash-free morph
  cover, driven by `screen.getAllDisplays()` and Electron's window
  compositing flags (`setIgnoreMouseEvents`, `setAlwaysOnTop(true,
  'screen-saver')`). Tauri's `WebviewWindow` supports transparent/frameless/
  always-on-top windows too, and multi-monitor APIs exist, but exact
  parity for *this specific combination* (click-through toggling +
  spans-all-displays + z-order pinned above a focused window) on
  WebKitGTK/Linux compositors (which vary — X11 vs. Wayland session, and
  which window manager) is unverified. Lower risk than 8a/8c because it's
  cosmetic (flash-free reload), not functionality-blocking if it doesn't
  fully work — worst case is a visible flash during structural reloads,
  which is a regression, not a break.
- `micro-apps/session-policy.ts` — uses Electron's
  `session.webRequest.onHeadersReceived` to *inject and override* CSP
  headers on responses (the actual security control keeping micro-apps'
  network egress locked down), plus `session.setPermissionRequestHandler`/
  `setPermissionCheckHandler` to deny all device permissions by default.
  This is real security-relevant logic, and Tauri does not have an
  equivalent to Electron's `session.webRequest` interception API — Tauri's
  request handling is comparatively thin. Enforcing this class of policy in
  Tauri would likely require a custom Rust `WebviewWindow` request handler
  or protocol-level intercept, which is a real (if bounded) design problem,
  not just a rewrite. Flagged because it's a security control, not a
  convenience feature — any gap here is a regression in the compromised-
  micro-app threat model this code explicitly defends against.

### 8e. Secrets (`secrets/secret-store.ts`)

**Classification: needs adaptation — clean seam, straightforward target.**

Already designed with a pluggable `CryptoBackend` interface
(`available`/`encrypt`/`decrypt`) so the store itself doesn't know it's
talking to Electron's `safeStorage`. On Linux, `safeStorage` is *itself*
backed by libsecret — so the Rust port target (the `keyring` crate, or
`secret-service` directly) talks to the literal same OS-level secret store
Electron already uses today. This is about as low-risk as a "needs
adaptation" item gets: implement one small trait, same on-disk format logic
carries over unchanged.

---

## 9. Effort sizing (solo dev, evenings/weekends pace)

Rough, not a committed estimate — the two unresolved risks in §10 could move
this by months in either direction depending on what a spike finds.

| Area | Sizing |
|---|---|
| Self-mod engine (scope-guard, watchdog, service) | 1–2 weeks |
| Git operations (shell-out approach) | 1–2 weeks |
| node-pty → portable-pty | few days |
| Secrets (safeStorage → keyring) | few days |
| MCP registry/probe/to-acp | few days |
| Auto-update → tauri-plugin-updater | ~1 week |
| ACP protocol client (Rust ACP crate) | 2–3 weeks |
| ACP adapter runtime problem (§2/§8b) | **unknown — depends entirely on which of the 3 options gets picked; sidecar-Node is 1–2 weeks of plumbing, "find non-JS adapters" is unknown until checked** |
| IPC layer (~90 methods across 20 namespaces) | 3–5 weeks, mostly mechanical |
| Live UI capture/scripting bridge (agent-bridge.ts) | **unknown — genuinely blocked on whether a WebKitGTK capture/eval story exists that's good enough; 1 week if a clean path exists, open-ended if not** |
| Morph overlay window | 1–2 weeks, degradable (can ship without flash-free morph as a v1 cutback) |
| Session CSP/permission policy | 1–2 weeks, assuming a workable Tauri request-intercept path exists |
| Vite-HMR-in-WebKitGTK verification (8a) | **unknown until spiked — this gates whether self-mod works at all** |

Ballpark, if the two "unknown" risk areas resolve favorably: **roughly
3–5 months** of evenings/weekends solo work to reach parity, not counting
testing/hardening/platform-specific bugs that always show up in this kind of
port. If either risk area resolves unfavorably, this is not a "port," it's a
redesign of that specific feature area, and the estimate should be treated
as void until re-scoped.

This matches the shape of the earlier T3 Code assessment
(`reference_t3code_electron_to_rust_eval.md` in memory) for a sibling
project — "partial rewrite, not a refactor," multi-week-to-multi-month for
1-2 engineers — though that was a different codebase and shouldn't be
treated as a substitute for this analysis, only as loose corroboration that
the shape of the estimate is plausible.

---

## 10. Top risks that could blow up the estimate

1. **Vite HMR self-mod trick on WebKitGTK (§8a).** The entire self-mod
   feature — an agent editing its own running UI live — depends on Vite's
   dev-server HMR websocket behaving predictably under
   `self-mod-overlay.ts`'s interception. That interception is Vite-level,
   not Electron-level, so it should port architecturally unchanged — but
   nobody has run this app's actual HMR client against WebKitGTK instead of
   Chromium. **This gates whether the core feature works at all in a Tauri
   shell**, not just whether it works well.

2. **The ACP adapter runtime problem (§2/§8b).** Electron gets to skip
   "does the user have Node.js installed" by using its own bundled Node.
   Tauri has no equivalent, and the vendored ACP adapters
   (`@zed-industries/claude-agent-acp` at minimum) are npm packages that need
   *something* to execute them as JS. Every option (bundle Node as a
   sidecar, require system Node, or find a non-JS-dependent adapter for at
   least the Codex side) has a real cost or a real unknown attached, and this
   affects the most central feature of the app (talking to the coding
   agent at all).

3. **Live UI capture + JS-eval bridge on WebKitGTK (§8c).** `capturePage()`
   and `executeJavaScript()` with return values are Chromium/CDP-flavored
   conveniences Tauri doesn't provide as directly. This bridge is what lets
   the agent see and verify its own edits — a self-mod loop that can't
   observe its own result is meaningfully degraded, not just less polished.

Any one of these three, if it resolves badly, changes the honest framing
from "port" to "redesign this specific subsystem" — which is why none of
them should be estimated around further without being spiked first.

---

## 11. Recommendation

**Worth one narrowly-scoped spike before investing more estimation or design
time. Not yet worth committing to a real port.**

The reasoning: a large fraction of the codebase (self-mod core, git ops,
terminal, secrets, MCP registry, auto-update — roughly half the surface
area by file count) is unremarkable Node logic that would port cleanly to
Rust with mechanical, low-risk effort. If that were the whole story, this
would be a straightforward "yes, worth doing, budget N weeks" call. It
isn't the whole story — the three risks in §10 are all central to what
makes Hearth *Hearth* (self-editing UI, talking to the agent, agent
self-observation), and none of them can be resolved by more reading. They
need to be run.

**Smallest useful spike to de-risk the biggest unknown (§10.1, the Vite-HMR
question, since it's both the highest-blast-radius risk and the cheapest to
test):**

Stand up a minimal Tauri shell (no IPC layer, no ACP, no self-mod service —
none of it) that does nothing but point a `WebviewWindow` at this repo's
*actual* running Vite dev server URL, with `self-mod-overlay.ts` loaded
exactly as it is today. Then, by hand or via a small script, replicate a
self-mod turn against it directly through the `/__hearth/self-mod` HTTP
endpoint (pin → edit a file on disk → apply) and watch whether the webview
hot-swaps the module without a full reload, the same way it does in the
Electron app today. This requires no Rust IPC design, no ACP work, no
rewriting anything else — it isolates exactly the one unverified claim
("this Vite plugin doesn't care what's rendering the page") and tests it
directly. A day or two of work, and it's a hard yes/no on the risk that
gates everything else.

If that spike passes, the next-cheapest thing to check is whether
`@agentclientprotocol/codex-acp` has a non-JS core (§2 option 3) — a
half-day of reading that package's actual dependency tree, not a build
task — since it directly changes the shape (and size) of the ACP runtime
problem before deciding between sidecar-Node and requiring system Node.

---

## 12. Spike results: §10.1, Vite-HMR self-mod trick on WebKitGTK

**Verdict: confirmed viable.** The self-mod HMR trick is Vite-level, not
Electron-level, exactly as §8a/§10.1 argued — it survives an unmodified
webview swap to WebKitGTK with no code changes to `self-mod-overlay.ts`.

**What was built** (`spike/`, WSLg/Ubuntu on this machine): a minimal Tauri
v2 shell (`spike/tauri-hmr-check/`) with no IPC layer, no ACP, no bundled
frontend — `app.windows[0].url` in `tauri.conf.json` points straight at a
standalone Vite dev server (`spike/vite.config.spike.ts`) that loads the
*exact same* renderer plugin set as `electron.vite.config.ts`
(`selfModOverlay(repoRoot)`, `tanstackRouter`, `react()`, `tailwind()`), so
the server under test is identical to what Electron loads today — the only
variable is which webview renders it. A temporary instrumentation module
(`src/shell/SpikeMarker.ts`, since reverted) reported a module-level
counter and a root-mount counter to a dev-only `/__spike/marker` endpoint,
tagged by `host` (`tauri` via `window.__TAURI__`, `electron` via
`window.hearth`). `spike/run-sequence.mjs` drove the real
`/__hearth/self-mod` HTTP endpoint through pin → edit-on-disk → apply →
turn-start/turn-end, polling the marker log and asserting against each step.

**Control run** (Electron, built via `electron-vite build` and launched
with `ELECTRON_RENDERER_URL` pointed at the same spike server — same
harness, known-good webview, sanity-checks the test methodology itself):
6/6 passed (`spike/results-electron.json`).

**Test run** (Tauri/WebKitGTK 2.52.3, `cargo build` against `tauri v2.11.5`
/ `wry v0.55.1`, launched pointed at the same spike server): 6/6 passed,
identical results to the control (`spike/results-tauri.json`).

| Criterion | Electron (control) | Tauri/WebKitGTK (test) |
|---|---|---|
| HMR websocket connects, marker reports land | PASS (`moduleValue=1`) | PASS (`moduleValue=1`) |
| `pin` suppresses the live update | PASS | PASS |
| `apply` produces the new value (targeted HMR swap) | PASS (`moduleValue=2`) | PASS (`moduleValue=2`) |
| `apply` does not trigger a full reload | PASS (root-mount unchanged) | PASS (root-mount unchanged) |
| Full-reload change during an active turn is suppressed | PASS (root-mount unchanged) | PASS (root-mount unchanged) |
| Full-reload change after `turn-end` is not suppressed | PASS (root-mount incremented) | PASS (root-mount incremented) |

**Caveat, unrelated to the HMR question:** the spike's Tauri window has no
preload/IPC bridge (out of scope by design — see "what was built" above),
so `window.hearth` is undefined and the app's `ErrorBoundary` catches a
`window.hearth.workspaces` crash after the initial render. This is expected
and doesn't affect the result: both marker reports (`module-counter`,
`root-mount`) fire from `src/main.tsx`/`SpikeMarker.ts` *before*
`RouterProvider` renders and throws, so the HMR signal path under test
completes regardless. Confirmed via the marker log and a screenshot of the
running window (showing the caught error, not a hang or a blank window —
i.e. WebKitGTK is rendering the app shell, CSS, and React's error UI
correctly up to that point).

**Not tested by this spike** (still open, per §10 risks #2 and #3): the ACP
adapter runtime problem, and live UI capture / JS-eval bridge parity on
WebKitGTK. Neither is affected by this result.

**Cleanup:** the temporary `src/main.tsx` edit and `src/shell/SpikeMarker.ts`
have been reverted. `spike/` is kept in the repo (build output gitignored)
as reproducible evidence for whoever picks this up next; see `spike/README.md`.
