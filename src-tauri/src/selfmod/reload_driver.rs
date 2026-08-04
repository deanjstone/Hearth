// Trait replicating hmr.ts's `ReloadDriver` interface.
//
// TS models the covered reload as an optional method
// (`coveredReload?: () => void | Promise<void>`); Rust traits have no optional
// methods, so it's split into a capability query (`supports_covered_reload`,
// default false) plus an action method (`covered_reload`, default no-op) —
// callers check the capability before calling the action, replicating the JS
// "is this method present" check from hmr.ts's `apply`.
pub trait ReloadDriver {
    fn reload_window(&self);
    fn restart_app(&self);
    fn supports_covered_reload(&self) -> bool {
        false
    }
    fn covered_reload(&self) {}
}
