// Micro-app sandbox subsystem (Phase 6, tracking issue #27): the per-app
// Vite dev server lifecycle, the W3/W6 CSP reverse proxy that's the actual
// enforcement point (WebKitGTK can't rewrite headers on real http:// traffic
// — see csp_proxy.rs's own header comment), egress capability grants, the
// W7 credential broker, and scaffolding. Ported from
// electron/main/micro-apps/*.

pub mod broker;
pub mod capabilities;
pub mod csp_proxy;
pub mod scaffold;
pub mod server;
pub mod validate;
