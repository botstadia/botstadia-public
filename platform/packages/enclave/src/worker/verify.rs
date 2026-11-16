//! Verification's worker (BOTS.md, isolation; TRUST.md#warm-ups): the
//! bundle's unzip, manifest, compile, preflight and costs, and the short
//! timing session against its arena, all in one confined process holding no
//! key. It runs its timing session's seat on threads of its own, so its
//! filter allows starting threads, and only threads.

use botstadia_host::LoadedArena;

use super::frame::{self, Frame, MAX_FRAME, decode, encode};
use super::*;

pub fn run(
    budget: u32,
    mut r: Reader,
    mut w: Writer,
    filter: &dyn Fn() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let version = botstadia_host::budget::version(budget)
        .ok_or_else(|| anyhow::anyhow!("no budget version {budget}"))?;
    let _ = botstadia_host::seat::engine(version);
    botstadia_host::onnx::runtime::start_environment()?;
    let Some(first) = frame::read(&mut r)? else {
        return Ok(());
    };
    anyhow::ensure!(
        first.kind == VERIFY_START,
        "verification starts with its start"
    );
    let start: VerifyStart = decode(&first.body, MAX_FRAME as u64)?;
    // The arena is ours: it compiles before the filter.
    let arena = LoadedArena::from_bytes(&start.arena)?;
    filter()?;
    let report = match botstadia_host::Bundle::from_zip(&start.bundle, version) {
        Ok(bundle) => Ok(botstadia_host::verify::verify(
            bundle,
            &arena,
            version,
            &start.seed,
        )),
        Err(e) => Err(format!("the bundle doesn't load: {e:#}")),
    };
    frame::write(&mut w, &Frame::new(VERIFY_REPORT, 0, encode(&report)))?;
    Ok(())
}
