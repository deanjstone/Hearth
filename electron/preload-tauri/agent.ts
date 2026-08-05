// window.hearth.agent's Tauri shim, backed by the real Tauri commands built in
// Chunk 5 (src-tauri/src/agent_commands.rs, spec deanjstone/Hearth#48) — the
// final piece of Phase 3. Mirrors electron/preload/index.ts's `agent` object
// method-for-method, EXCEPT `prompt`'s return type: see agent_commands.rs's
// header comment for why this calls AgentHostEngine directly instead of
// routing through TurnCoordinator (full self-mod-turn integration needs a
// real session store, not ported to Tauri yet), and so resolves void instead
// of Electron's `SelfModResult | null`.

import type {
  AgentErrorPayload,
  AgentKind,
  AgentUpdatePayload,
  AvailableCommand,
  BackendStatus,
  ConfigOption,
  ModeState,
  ModelState,
  PromptCapabilities,
  PromptImage,
  Usage,
} from '../shared/protocol.js'
import { onEvent, tauri } from './tauri-global.js'

export const agent = {
  prompt: (sessionId: string, cwd: string, text: string, images?: PromptImage[]): Promise<void> =>
    tauri().core.invoke('agent_prompt', { sessionId, cwd, text, images }) as Promise<void>,
  cancel: (sessionId?: string): Promise<void> => tauri().core.invoke('agent_cancel', { sessionId }) as Promise<void>,
  getBackend: (): Promise<AgentKind> => tauri().core.invoke('agent_backend_get') as Promise<AgentKind>,
  setBackend: (kind: AgentKind): Promise<BackendStatus> =>
    tauri().core.invoke('agent_backend_set', { kind }) as Promise<BackendStatus>,
  getModels: (): Promise<ModelState> => tauri().core.invoke('agent_models_get') as Promise<ModelState>,
  setModel: (modelId: string): Promise<void> => tauri().core.invoke('agent_model_set', { modelId }) as Promise<void>,
  onModelsChanged: (cb: (state: ModelState) => void) => onEvent<ModelState>('agent:models:changed', cb),
  getModes: (): Promise<ModeState> => tauri().core.invoke('agent_modes_get') as Promise<ModeState>,
  setMode: (modeId: string): Promise<void> => tauri().core.invoke('agent_mode_set', { modeId }) as Promise<void>,
  onModeChanged: (cb: (state: ModeState) => void) => onEvent<ModeState>('agent:mode:changed', cb),
  getConfigOptions: (): Promise<ConfigOption[]> => tauri().core.invoke('agent_config_get') as Promise<ConfigOption[]>,
  setConfigOption: (configId: string, value: string | boolean): Promise<void> =>
    tauri().core.invoke('agent_config_set', { configId, value }) as Promise<void>,
  onConfigChanged: (cb: (options: ConfigOption[]) => void) => onEvent<ConfigOption[]>('agent:config:changed', cb),
  getUsage: (): Promise<Usage | null> => tauri().core.invoke('agent_usage_get') as Promise<Usage | null>,
  onUsageChanged: (cb: (usage: Usage) => void) => onEvent<Usage>('agent:usage:changed', cb),
  getPromptCapabilities: (): Promise<PromptCapabilities> =>
    tauri().core.invoke('agent_prompt_caps_get') as Promise<PromptCapabilities>,
  getCommands: (): Promise<AvailableCommand[]> =>
    tauri().core.invoke('agent_commands_get') as Promise<AvailableCommand[]>,
  onCommandsChanged: (cb: (commands: AvailableCommand[]) => void) =>
    onEvent<AvailableCommand[]>('agent:commands:changed', cb),
  onBackendChanged: (cb: (status: BackendStatus) => void) => onEvent<BackendStatus>('agent:backend:changed', cb),
  onUpdate: (cb: (payload: AgentUpdatePayload) => void) => onEvent<AgentUpdatePayload>('agent:update', cb),
  onError: (cb: (payload: AgentErrorPayload) => void) => onEvent<AgentErrorPayload>('agent:error', cb),
}
