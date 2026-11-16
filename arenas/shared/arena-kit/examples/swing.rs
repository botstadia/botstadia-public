//! The swing test world as the `arena` WASM component, proving the export
//! macro. Build: `cargo build -p arena-kit --example swing --features guest
//! --target wasm32-wasip2`.

include!("../tests/support/swing_world.rs");

arena_kit::export_world!(Swing);
