// window.hearth.git's Tauri shim, backed by the Phase 7 Rust commands
// (src-tauri/src/git_commands.rs, src-tauri/src/git_panel.rs). Only the
// repo-root workspace is supported (`cwd` is accepted for call-site parity
// but ignored on the Rust side) — see git_commands.rs's own header comment.

import { tauri } from './tauri-global.js'

export interface StatusFile {
  path: string
  oldPath?: string
  tag: 'new' | 'modified' | 'deleted' | 'renamed' | 'untracked'
  staged: boolean
  unstaged: boolean
}

export interface GitStatus {
  branch: string | null
  ahead: number
  behind: number
  files: StatusFile[]
}

export interface BranchInfo {
  current: string | null
  branches: string[]
}

export interface PrResult {
  created: boolean
  detail: string
}

export interface DiffRow {
  t: 'add' | 'del' | 'ctx' | 'hunk'
  code: string
  ln: number | null
}

export interface DiffFile {
  file: string
  oldPath?: string
  tag: 'new' | 'modified' | 'deleted' | 'renamed'
  add: number
  del: number
  rows: DiffRow[]
}

export interface DiffSummary {
  files: DiffFile[]
  add: number
  del: number
  branch: string | null
}

export const git = {
  diff: (cwd?: string, rev?: string): Promise<DiffSummary> => tauri().core.invoke('git_diff', { cwd, rev }) as Promise<DiffSummary>,
  status: (cwd?: string): Promise<GitStatus> => tauri().core.invoke('git_status', { cwd }) as Promise<GitStatus>,
  stage: (paths: string[], cwd?: string): Promise<void> => tauri().core.invoke('git_stage', { paths, cwd }) as Promise<void>,
  unstage: (paths: string[], cwd?: string): Promise<void> => tauri().core.invoke('git_unstage', { paths, cwd }) as Promise<void>,
  commit: (message: string, cwd?: string): Promise<string> => tauri().core.invoke('git_commit', { message, cwd }) as Promise<string>,
  branches: (cwd?: string): Promise<BranchInfo> => tauri().core.invoke('git_branches', { cwd }) as Promise<BranchInfo>,
  switchBranch: (name: string, create: boolean, cwd?: string): Promise<void> =>
    tauri().core.invoke('git_switch_branch', { name, create, cwd }) as Promise<void>,
  createPr: (title: string, body: string, cwd?: string): Promise<PrResult> =>
    tauri().core.invoke('git_create_pr', { title, body, cwd }) as Promise<PrResult>,
}
