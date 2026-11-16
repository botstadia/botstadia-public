//! Play one session: `cargo run --example play -- <arena.wasm> <seed> <bot>…`.
//! Each bot is a bundle ZIP, a bundle directory or a bare component.

use std::sync::Arc;

use botstadia_host::seat::Program;
use botstadia_host::{Bundle, LoadedArena, Seat, Session, SessionOptions, budget, seed};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let arena_path = args.next().expect("usage: play <arena.wasm> <seed> <bot>…");
    let seed: u64 = args.next().expect("a seed").parse()?;
    let budget = budget::current();
    let arena = LoadedArena::load(arena_path.as_ref())?;
    let turn_fuel = arena.declaration.budgets.turn_fuel;
    let seats = args
        .map(|path| -> anyhow::Result<Seat> {
            let bundle = Bundle::load(path.as_ref(), budget)?;
            Ok(Seat::Bot(Arc::new(Program::new(
                bundle, budget, turn_fuel,
            )?)))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let started = std::time::Instant::now();
    let (mut session, _) = Session::start(
        &arena,
        seats,
        &seed::from_u64(seed),
        budget,
        SessionOptions::default(),
    )?;
    let mut fuel = vec![0u64; session.seat_names().len()];
    let mut paused = vec![0u32; fuel.len()];
    while session.finished().is_none() {
        let out = session.step(&[])?;
        for (index, seat) in out.seats.iter().enumerate() {
            fuel[index] = fuel[index].max(seat.wasm_fuel);
            paused[index] += u32::from(seat.paused);
        }
    }
    let finish = session.finished().cloned().unwrap();
    println!(
        "{} turns in {:.2?}: {} places {:?}",
        session.turn(),
        started.elapsed(),
        finish.decided_by,
        finish.places
    );
    println!("peak wasm fuel per seat {fuel:?}, paused turns {paused:?}");
    let (archive, metrics) = session.record()?;
    println!(
        "archive {} frames, metrics {} bytes",
        archive.frames.len(),
        metrics.to_json().len()
    );
    Ok(())
}
