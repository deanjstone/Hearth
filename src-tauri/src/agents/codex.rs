// Codex backend adapter resolution — spawns @agentclientprotocol/codex-acp,
// which vendors the @openai/codex CLI and exposes it over ACP. Same shape as
// the Claude backend, just a different adapter and no crash-shielding quirk.
// Ported from electron/main/agents/codex.ts.
//
// Auth: subscription-only this phase (spec #48's Out of Scope — no api-key
// path). We inherit the user's existing `codex login` (~/.codex) by spawning
// in their environment — the same credential the `codex` CLI uses.

use super::acp_client::{resolve_adapter_bin, AcpClient, AdapterSpec};
use super::agent::{AgentConfig, AgentKind};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The vendored adapter package + bin name, shared with `startup_check.rs` so
/// the eager availability check can't drift from what actually gets spawned.
pub const PACKAGE: &str = "@agentclientprotocol/codex-acp";
pub const BIN: &str = "codex-acp";

fn resolve_adapter(config: &AgentConfig, repo_root: &Path) -> Result<AdapterSpec, String> {
    // Run the vendored codex-acp bin (which drives the vendored @openai/codex),
    // not whatever `codex` is on PATH.
    let bin = resolve_adapter_bin(repo_root, PACKAGE, BIN)?;
    Ok(AdapterSpec {
        command: "node".to_string(),
        args: vec![bin.to_string_lossy().into_owned()],
        cwd: config.cwd.clone(),
        env: HashMap::new(),
    })
}

/// Build the Codex backend's `AcpClient`. `repo_root` resolves the vendored
/// adapter package's bin; `config` supplies the task cwd and (subscription-only,
/// this phase) auth mode.
pub fn new_agent(config: AgentConfig, repo_root: PathBuf) -> AcpClient {
    AcpClient::new(AgentKind::Codex, move || {
        resolve_adapter(&config, &repo_root)
    })
}
