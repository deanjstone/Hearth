# Hearth

Hearth is a Linux (WSL2) desktop client for coding agents (Claude Code or Codex) that can
edit its own running interface. It's a Rust/Tauri shell (ported from an earlier
Electron build — see [docs/decisions/](docs/decisions/)) around a React renderer
served by a live Vite dev server, so when the agent edits Hearth's own source the
change hot-reloads into the window with no restart, and every edit is a git commit
you can revert. The agent can also see and drive the app the way you would, through
a small MCP server it is given.

The one example that captures it: say "add a Stats item to the sidebar," and the
agent edits the app's own source, the sidebar reshapes in front of you, and the
change shows up in the Changes view with one-click undo.

![Version](https://img.shields.io/badge/version-0.1.0-FF6B35?style=flat-square)
![Platform](https://img.shields.io/badge/Linux-WSL2-111111?style=flat-square&logo=linux&logoColor=white)
[![License](https://img.shields.io/badge/license-Apache%202.0-3178C6?style=flat-square)](LICENSE)

<p align="center">
  <img src="docs/screenshots/06-chat-dark.png" width="860"
       alt="Hearth editing its own UI: the agent reads the codebase, writes new files, and adds a sidebar feature to the running app.">
</p>

## Status

Personal project, v0.1.0. Linux (WSL2) dev mode only: run from source, there is
no packaged build for this platform. Roughly 23k lines with 475 passing tests. It
works and it is tested, but it is not a product and comes with no support. Use it
at your own risk.

You bring your own agent. Hearth drives the Claude Code or Codex you already
authenticated with `claude login` / `codex login` (or your own API key) over the
open [Agent Client Protocol](https://agentclientprotocol.com). It never stores,
brokers, or sees your credentials, and it hosts nothing: files and conversations
stay on your machine.

## Build from source

There is no packaged build for Linux; this is a dev-mode-only port. You need
Node 22, [pnpm](https://pnpm.io) (version pinned in `package.json`'s
`packageManager` field), a Rust toolchain plus `tauri-cli`
(`cargo install tauri-cli --locked`), WebKitGTK dev libraries
(`libwebkit2gtk-4.1-dev` on Debian/Ubuntu), `build-essential` and `python3`
(for the `node-pty` native rebuild, only needed if you also run the retired
Electron build via `pnpm dev:electron`), and a locally-authenticated agent.

```bash
pnpm install
pnpm run routes:gen             # generate the TanStack route tree (needed before typecheck from a fresh clone)
pnpm run dev                    # builds the Rust/Tauri shell + starts the Vite dev server, with live HMR (Claude backend)
```

Same app on the Codex backend:

```bash
HEARTH_AGENT=codex pnpm run dev
```

Checks:

```bash
pnpm run test                   # ACP translation, git, self-mod, scope guard, boot watchdog, classifier
pnpm run typecheck
pnpm run lint
```

Useful flags for UI work: `HEARTH_FAKE_AGENT=1` runs a scripted agent with no
model and no auth; `HEARTH_PERMISSION_MODE=default` prompts on every edit instead
of auto-accepting.

## The self-mod safety model, in brief

Hearth lets the agent edit almost anything in the repo, including the main process.
Safety comes from recoverability, not from fencing the agent out.

- **Scope guard.** Every write is classified into one of three tiers. *Blocked*
  paths are never writable (secrets, credential and shell-init files, system
  directories, git internals). The *protected island* (the self-mod engine, the
  boot watchdog, the hook config) is editable only with explicit approval and is
  written dependency-free so the agent cannot disarm a guardrail indirectly.
  Everything else is the *canvas*. Writes route through Hearth's own file
  capability, and shell writes that try to bypass the guard are forced back onto
  the mediated path.
- **Boot watchdog.** A main-process edit that bricks startup is caught by a marker
  armed before each self-mod restart and cleared only on a healthy boot. If the
  next boot still finds the marker, the edit never came up, so the watchdog
  auto-reverts that commit and relaunches, with a bounded attempt count to avoid a
  revert loop.
- **Atomic parallel edits.** A single agent's edit hot-swaps immediately. When two
  or more subagents write concurrently, a snapshot-overlay Vite plugin pins their
  pre-edit baselines and swaps the batch in atomically, so the live UI never shows
  a half-applied state.
- **Versioned undo.** Every self-edit is a `Hearth-SelfMod` git commit, grouped by
  turn into file-disjoint commits, and reversible from the Changes view.

## Architecture

The shell is Rust/Tauri, ported from an earlier Electron build — see
[ADR-001](docs/decisions/adr-001-electron-to-tauri-cutover.md) for the cutover
decision and [docs/decisions/rust-tauri-feasibility.md](docs/decisions/rust-tauri-feasibility.md)
for the per-subsystem port rationale. [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
still describes the pre-cutover Electron design in detail and is flagged stale
at the top pending a full rewrite; domain vocabulary in [CONCEPTS.md](CONCEPTS.md)
still applies unchanged. The self-evolution engine now lives in
`src-tauri/src/selfmod/`; `scope_guard.rs` and `boot_watchdog.rs` are the two
files worth reading first. Packaging and auto-update (`docs/AUTO-UPDATE.md`,
`docs/PACKAGING-V3-PLAN.md`) describe the retired Electron build's plans and
remain out of scope for the Tauri MVP.

## License

[Apache-2.0](LICENSE).
