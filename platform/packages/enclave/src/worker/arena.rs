//! The arena's process: runs the session (`botstadia-host`'s `Session`)
//! with every seat linked through the runner, and writes the archive and
//! metrics. It holds no key, and a bot reaching it through its actions
//! still has the kernel's isolation to escape.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use botstadia_host::seat::{SeatLink, SeatSettings, TurnReport};
use botstadia_host::session::{ArenaFault, EntrantUnavailable, SpawnLink};
use botstadia_host::{LoadedArena, Seat, Session, SessionOptions};

use super::frame::{self, Frame, MAX_FRAME, decode, encode};
use super::*;

/// The pipe to the runner, with frames that arrived for a seat before it
/// asked for them.
struct Shared {
    r: Reader,
    w: Writer,
    waiting: HashMap<(u8, u32), Vec<Frame>>,
}

impl Shared {
    /// The next frame of `kind` for `seat`.
    fn next(&mut self, kind: u8, seat: u32) -> anyhow::Result<Frame> {
        if let Some(list) = self.waiting.get_mut(&(kind, seat))
            && !list.is_empty()
        {
            return Ok(list.remove(0));
        }
        loop {
            let f =
                frame::read(&mut self.r)?.ok_or_else(|| anyhow::anyhow!("the runner went away"))?;
            if f.kind == kind && f.seat == seat {
                return Ok(f);
            }
            self.waiting.entry((f.kind, f.seat)).or_default().push(f);
        }
    }
}

type Pipe = Arc<Mutex<Shared>>;

struct Link(Pipe);

struct Linked {
    pipe: Pipe,
    seat: u32,
}

impl SpawnLink for Link {
    fn spawn(
        &self,
        seat: usize,
        settings: SeatSettings,
    ) -> anyhow::Result<(Box<dyn SeatLink>, TurnReport)> {
        let seat = seat as u32;
        let mut p = self.0.lock().expect("the pipe");
        frame::write(&mut p.w, &Frame::new(ARENA_SPAWN, seat, encode(&settings)))?;
        let f = p.next(ARENA_SEAT_STARTED, seat)?;
        let started: Result<TurnReport, String> = decode(&f.body, MAX_FRAME as u64)?;
        let init = started.map_err(|e| anyhow::anyhow!(e))?;
        Ok((
            Box::new(Linked {
                pipe: self.0.clone(),
                seat,
            }),
            init,
        ))
    }
}

impl SeatLink for Linked {
    fn send(&mut self, turn: u32, input: Vec<u8>) {
        let mut p = self.pipe.lock().expect("the pipe");
        let _ = frame::write(
            &mut p.w,
            &Frame::new(ARENA_TURN, self.seat, encode(&(turn, input))),
        );
    }

    fn collect(&mut self) -> TurnReport {
        let mut p = self.pipe.lock().expect("the pipe");
        p.next(ARENA_REPORT, self.seat)
            .ok()
            .and_then(|f| decode(&f.body, MAX_FRAME as u64).ok())
            .unwrap_or_else(|| TurnReport {
                trapped: Some("the seat's report was lost".into()),
                ..TurnReport::default()
            })
    }
}

pub fn run(
    budget: u32,
    mut r: Reader,
    w: Writer,
    filter: &dyn Fn() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let version = botstadia_host::budget::version(budget)
        .ok_or_else(|| anyhow::anyhow!("no budget version {budget}"))?;
    let Some(first) = frame::read(&mut r)? else {
        return Ok(());
    };
    anyhow::ensure!(first.kind == ARENA_START, "an arena starts with its start");
    let start: ArenaStart = decode(&first.body, MAX_FRAME as u64)?;
    // The arena is ours and published: it compiles before the filter, which
    // guards against what bots' actions do to it.
    let arena = LoadedArena::from_bytes(&start.arena);
    filter()?;
    let pipe: Pipe = Arc::new(Mutex::new(Shared {
        r,
        w,
        waiting: HashMap::new(),
    }));
    let done = match arena {
        Err(e) => ArenaDone::ArenaFault(format!("{e:#}")),
        Ok(arena) => play(&arena, &start, version, &pipe),
    };
    let mut p = pipe.lock().expect("the pipe");
    frame::write(&mut p.w, &Frame::new(ARENA_DONE, 0, encode(&done)))?;
    Ok(())
}

fn classify(error: &anyhow::Error) -> ArenaDone {
    if let Some(f) = error.chain().find_map(|e| e.downcast_ref::<ArenaFault>()) {
        return ArenaDone::ArenaFault(f.to_string());
    }
    if let Some(u) = error
        .chain()
        .find_map(|e| e.downcast_ref::<EntrantUnavailable>())
    {
        return ArenaDone::EntrantUnavailable(u.seat as u32);
    }
    ArenaDone::Failed(format!("{error:#}"))
}

fn play(
    arena: &LoadedArena,
    start: &ArenaStart,
    version: &'static botstadia_host::budget::BudgetVersion,
    pipe: &Pipe,
) -> ArenaDone {
    let link: Arc<dyn SpawnLink> = Arc::new(Link(pipe.clone()));
    let seats: Vec<Seat> = start
        .names
        .iter()
        .map(|name| Seat::Linked {
            name: name.clone(),
            link: link.clone(),
        })
        .collect();
    let (mut session, first) = match Session::start(
        arena,
        seats,
        &start.seed,
        version,
        SessionOptions::default(),
    ) {
        Ok(s) => s,
        Err(e) => return classify(&e),
    };
    let mut trapped = Vec::new();
    let mut arena_fuel = first.arena_fuel.unwrap_or(0);
    let mut note = |out: &botstadia_host::session::StepOut| {
        for (seat, t) in out.seats.iter().enumerate() {
            if let Some(why) = &t.trapped {
                trapped.push((seat as u32, out.turn, why.clone()));
            }
        }
    };
    note(&first);
    let mut finish = first.finish.clone();
    let none: Vec<Option<Vec<u8>>> = vec![None; start.names.len()];
    while finish.is_none() {
        let out = match session.step(&none) {
            Ok(o) => o,
            Err(e) => return classify(&e),
        };
        note(&out);
        arena_fuel += out.arena_fuel.unwrap_or(0);
        finish = out.finish;
    }
    let places = finish.expect("the loop ends on a finish").places;
    match session.record() {
        Ok((archive, metrics)) => ArenaDone::Finished {
            places,
            archive: archive.encode(),
            metrics: serde_json::to_vec(&metrics).unwrap_or_default(),
            trapped,
            arena_fuel,
        },
        Err(e) => ArenaDone::Failed(format!("{e:#}")),
    }
}
