//! A seat's Wasmtime process: unzips the bundle, reads its manifest,
//! compiles and preflights, then runs the bot turn by turn on its main
//! thread. Its models go to the seat's inference process through the
//! runner, and so does every `infer` call.

use std::sync::{Arc, Mutex};

use botstadia_host::onnx::runtime::NamedTensor;
use botstadia_host::seat::{
    InferFailure, Inference, Program, SeatInPlace, SeatSettings, TurnReport,
};

use super::frame::{self, Frame, MAX_FRAME, decode, encode};
use super::*;

type Pipe = Arc<Mutex<(Reader, Writer)>>;

/// Inference through the runner: each call is a frame out and its answer
/// back, in order.
struct Relayed(Pipe);

impl Inference for Relayed {
    fn run(
        &mut self,
        model: usize,
        inputs: Vec<NamedTensor>,
    ) -> Result<Vec<NamedTensor>, InferFailure> {
        let mut pipe = self.0.lock().expect("the pipe");
        let (r, w) = &mut *pipe;
        let body = encode(&(model as u32, inputs));
        if frame::write(w, &Frame::new(SEAT_INFER, 0, body)).is_err() {
            return Err(InferFailure::Stopped);
        }
        match frame::read(r) {
            Ok(Some(f)) if f.kind == SEAT_OUTPUTS => {
                decode(&f.body, MAX_FRAME as u64).unwrap_or(Err(InferFailure::Stopped))
            }
            _ => Err(InferFailure::Stopped),
        }
    }
}

pub fn run(
    budget: u32,
    mut r: Reader,
    mut w: Writer,
    filter: &dyn Fn() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let version = botstadia_host::budget::version(budget)
        .ok_or_else(|| anyhow::anyhow!("no budget version {budget}"))?;
    // The engine is made before the filter: it's ours, the bundle isn't.
    let _ = botstadia_host::seat::engine(version);
    filter()?;
    // Everything from here reads what the bundle controls.
    let Some(first) = frame::read(&mut r)? else {
        return Ok(());
    };
    if first.kind == SEAT_CHECK {
        let c: SeatCheck = decode(&first.body, MAX_FRAME as u64)?;
        let checked = botstadia_host::Bundle::from_zip(&c.bundle, version)
            .and_then(|b| Program::new(b, version, c.turn_fuel))
            .map(|_| TurnReport::default())
            .map_err(|e| format!("{e:#}"));
        frame::write(&mut w, &Frame::new(SEAT_STARTED, 0, encode(&checked)))?;
        return Ok(());
    }
    anyhow::ensure!(first.kind == SEAT_START, "a seat starts with its start");
    let mut start: SeatStart = decode(&first.body, MAX_FRAME as u64)?;
    drop(first);
    let settings: SeatSettings = decode(&start.settings, 1 << 20)?;
    let started = |w: &mut Writer, result: Result<TurnReport, String>| {
        frame::write(w, &Frame::new(SEAT_STARTED, 0, encode(&result)))
    };
    // The zip goes once it's unzipped: the seat's memory cap holds the
    // unzipped bundle and one encoding of its models, not every copy.
    let zip = std::mem::take(&mut start.bundle);
    let program = botstadia_host::Bundle::from_zip(&zip, version)
        .and_then(|b| Program::new(b, version, settings.turn_fuel));
    drop(zip);
    let mut program = match program {
        Ok(p) => p,
        Err(e) => {
            started(&mut w, Err(format!("{e:#}")))?;
            return Ok(());
        }
    };
    // Its models to its inference process; wait until they're loaded (or
    // its inference has ended), so no bot code runs before.
    if !program.models.is_empty() {
        // The bytes are moved, not copied: inference is the other process's,
        // so this one never needs them again. The names stay: the seat's
        // model table is built from them, and `infer` finds no model
        // without it.
        let models: Vec<ModelToLoad> = program
            .bundle
            .models
            .iter_mut()
            .zip(&program.models)
            .map(|((name, bytes), info)| ModelToLoad {
                name: name.clone(),
                bytes: std::mem::take(bytes),
                info: info.clone(),
            })
            .collect();
        let body = encode(&models);
        drop(models);
        frame::write(&mut w, &Frame::new(SEAT_MODELS, 0, body))?;
        match frame::read(&mut r)? {
            Some(f) if f.kind == SEAT_LOADED => {}
            _ => anyhow::bail!("expected the models' load"),
        }
    }
    frame::write(&mut w, &Frame::new(SEAT_BEGIN, 0, Vec::new()))?;
    let pipe: Pipe = Arc::new(Mutex::new((r, w)));
    let declaration = Arc::new(start.declaration);
    let seat = SeatInPlace::start(
        &program,
        declaration,
        &settings,
        Box::new(Relayed(pipe.clone())),
    );
    let mut seat = {
        let mut p = pipe.lock().expect("the pipe");
        match seat {
            Ok((seat, init)) => {
                started(&mut p.1, Ok(init))?;
                seat
            }
            Err(e) => {
                started(&mut p.1, Err(format!("{e:#}")))?;
                return Ok(());
            }
        }
    };
    loop {
        let next = {
            let mut p = pipe.lock().expect("the pipe");
            frame::read(&mut p.0)?
        };
        let Some(f) = next else {
            return Ok(());
        };
        anyhow::ensure!(f.kind == SEAT_TURN, "a seat takes only turns now");
        let (turn, input): (u32, Vec<u8>) = decode(&f.body, MAX_FRAME as u64)?;
        let report = seat.turn(turn, input);
        let mut p = pipe.lock().expect("the pipe");
        frame::write(&mut p.1, &Frame::new(SEAT_REPORT, 0, encode(&report)))?;
    }
}
