---
title: ADR-001 — Electron-to-Tauri dev-entrypoint cutover
status: accepted
date: 2026-08-07
---

# ADR-001: Electron-to-Tauri dev-entrypoint cutover

## Context

Spec [#26](https://github.com/deanjstone/Hearth/issues/26) (Port Hearth to
Rust/Tauri, self-mod-capable MVP) ported the app's shell from Electron to
Tauri across six phases (tracking issue
[#27](https://github.com/deanjstone/Hearth/issues/27)): self-mod core, UI
capture/eval bridge, ACP agent runtime, terminal, MCP registry, and
micro-app CSP/permission enforcement. Phase 7 is explicitly the last phase,
and the spec's own Further Notes left one question open on purpose: *"the
decision of when/how to cut the dev entrypoint over from Electron to Tauri
(or run both in parallel for a while) ... is left to whoever picks this
spec up."*

By the start of Phase 7, the Tauri build had reached functional parity with
the Electron build's self-mod-capable MVP scope (spec #26's 32 user
stories), plus Phase 7 itself closed the remaining IPC surface that was
still wired into real, routed UI but had no Tauri equivalent (git panel,
files tab, skills panel, personality/memory settings, routines) — see the
Phase 7 commits on this branch. The two shells could plausibly have kept
running in parallel behind two `pnpm` scripts for a soak period, or the
cutover could be deferred to a later, separate decision point.

The spec's own motivating problem (`docs/decisions/rust-tauri-feasibility.md`,
and spec #26's Problem Statement) is specifically that the Electron shell's
bundled Chromium+Node runtime sits directly next to the self-mod
capability's write-scope guard, and that guard only holds today because the
protected-file list happens to include its own path — not because it's
structurally unreachable. That security property is not actually realized
until the Electron shell (and its self-modifiable-TypeScript scope guard)
stops being the thing a user actually runs day to day.

## Decision

Cut over now, fully: `pnpm dev` launches the Tauri build
(`cd src-tauri && cargo tauri dev`, itself launching the frontend Vite dev
server via a new `beforeDevCommand`) as the default entrypoint. The
Electron build is retired from active use but not deleted from the repo —
`electron/main/**`, `electron/preload/**`, and the `dev:electron` npm script
remain, callable directly, as a rollback path and a historical reference,
until a follow-up decides to remove them outright.

Rejected: running both shells in parallel for a soak period. Every phase up
to and including Phase 7 was already validated per-subsystem (Rust
`#[test]` coverage mirroring the existing Electron `*.test.ts` suites as a
checklist, plus a permanent `tauri-driver`+WebDriverIO CI job for the three
WebKitGTK-coupled behaviors spec #26 names). A parallel-run period would
mostly re-litigate work already covered by that CI job rather than surface
new risk, while leaving two shells to maintain and leaving the scope-guard
security property described above unrealized for no added confidence.

## Consequences

- `pnpm dev` now requires the Rust toolchain and `tauri-cli`
  (`cargo install tauri-cli --locked`) in addition to Node/pnpm — previously
  only needed for `src-tauri/` development directly, now needed for the
  default dev loop.
- `src-tauri/**` changes need a real recompile + process restart
  (`cargo tauri dev` auto-restarts on file change, but this is slower than
  the renderer's Vite HMR) — `CLAUDE.md` now calls this out explicitly for
  any agent self-modifying the app.
- Electron-only features with no Tauri port — the embedded browser tab
  (`window.hearth.browser`, an Electron `WebContentsView` primitive with no
  Tauri equivalent), secrets storage, and auto-update — are stubbed as
  safe no-ops/honest-unavailable responses in `electron/preload-tauri/*`
  rather than ported. Secrets and auto-update/distribution were already
  out of scope for this MVP per spec #26; the browser tab is a new,
  Phase-7-specific scope decision (gated in `BrowserTab.tsx` with a clear
  "not available" notice rather than silently broken controls).
- `docs/ARCHITECTURE.md` still describes the pre-cutover Electron
  architecture in detail (diagrams, IPC channel names, `dugite` usage) and
  needs its own follow-up rewrite — flagged inline in that file and tracked
  as a separate GitHub issue rather than attempted in this same change.
- Follow-up: decide when (if ever) to delete `electron/**` outright, once
  enough confidence has accumulated in the Tauri build that keeping the
  rollback path around stops earning its keep.
