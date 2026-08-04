// Minimal host surface the turn coordinator depends on.
//
// `electron/main/agents/agent-host.ts` is a large class owning real ACP
// subprocess lifecycle: spawning/connecting the backend, session creation and
// resume, model/mode/config/usage caching, and unexpected-death recovery.
// None of that is needed to port the self-mod turn lifecycle — TS's own
// `TurnCoordinatorDeps` only ever narrows it to `Pick<AgentHost, 'prompt'>`.
// This trait is that same narrow slice; the full class is a separate, later
// port target (its concrete type will implement this trait once it lands).
pub struct PromptOptions {
    /// Renderer session key — one ACP session per key.
    pub key: String,
    pub cwd: Option<String>,
    /// A prior ACP session id to resume, if the backend supports it (W3).
    pub resume_id: Option<String>,
    // Image attachments (TS's `PromptImage[]`) are out of scope here: they
    // depend on the ACP agent protocol types, which aren't ported yet. Add a
    // field here when `agent.ts`'s protocol types land.
}

pub trait AgentHost: Send + Sync {
    /// Run one turn against the current backend; returns the ACP session id
    /// it ran under (for resume on a later turn).
    ///
    /// TS's version can reject with an arbitrary JSON-RPC error *object*, so
    /// `runTurn` has to normalize it into a readable message (unwrap
    /// `.message`, or `JSON.stringify` a message-less object) before it
    /// re-throws. Rust's `Result<_, String>` has no such ambiguity — an
    /// implementor is expected to have already produced a clean message by
    /// the time it returns `Err`, so that normalization step has no Rust
    /// analog and isn't ported.
    fn prompt(&self, text: &str, opts: &PromptOptions) -> Result<String, String>;
}
