// window.hearth.secrets's Tauri stub (Phase 7, tracking issue #27). Secrets
// storage (`safeStorage` -> `keyring`) is explicitly out of scope for this
// MVP per spec #26's Out of Scope section — same standing gap the Rust ACP
// layer's own `NullSecretLookup` already carries (mcp/to_acp.rs,
// mcp_commands.rs, micro_apps/broker.rs). `list`/`encryptionAvailable`
// honestly report "nothing stored" rather than silently pretending success.

export interface SecretInfo {
  key: string
  hasValue: true
}

export const secrets = {
  list: (): Promise<SecretInfo[]> => Promise.resolve([]),
  set: (_key: string, _value: string): Promise<void> => Promise.resolve(),
  delete: (_key: string): Promise<void> => Promise.resolve(),
  encryptionAvailable: (): Promise<boolean> => Promise.resolve(false),
}
