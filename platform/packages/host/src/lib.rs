//! BotStadia's session host: one arena and its seats, run exactly as a
//! session runs.
//!
//! This is the one host. The `botstadia` CLI, `botstadia-train` and Arena
//! Studio's tools run sessions through it, and the enclave will too, adding
//! process isolation, seccomp and keys around it.
//!
//! - [`budget`]: budget versions: the fuel cost table, inference rates and
//!   the limits a bot runs under
//! - [`bundle`]: a bot's bundle: loading one from a ZIP, a directory or a
//!   bare component, and building the deterministic ZIP
//! - [`onnx`]: the ONNX preflight (static graph, allowlist, shapes, the
//!   inference charge and memory bound) and inference on ONNX Runtime
//! - [`seat`]: a bot in a seat, on a thread of its own, fuelled and paused
//! - [`arena`]: an arena's engine on wasmtime
//! - [`session`]: one session, turn by turn, with the platform's validity
//!   rule, writing the replay archive and metrics file
//! - [`seed`]: labelled random streams from a session's seed
//! - [`verify`]: upload verification's checks, shared by `botstadia check`
//!   and the enclave

pub mod arena;
pub mod budget;
pub mod bundle;
pub mod calibrate;
pub mod onnx;
pub mod seat;
pub mod seed;
pub mod session;
pub mod verify;

pub use arena::LoadedArena;
pub use bundle::Bundle;
pub use session::{Seat, Session, SessionOptions};

/// wasmtime 50 has its own error type; carry its chain into anyhow.
pub(crate) trait WasmtimeContext<T> {
    fn ctx(self, what: impl std::fmt::Display) -> anyhow::Result<T>;
}

impl<T> WasmtimeContext<T> for wasmtime::Result<T> {
    fn ctx(self, what: impl std::fmt::Display) -> anyhow::Result<T> {
        self.map_err(|error| anyhow::anyhow!("{what}: {error:#}"))
    }
}

/// `sha256-<hex>` of some bytes: how BotStadia names arenas, bundles and
/// shells by digest.
pub fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(bytes);
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256-");
    for byte in hash {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
