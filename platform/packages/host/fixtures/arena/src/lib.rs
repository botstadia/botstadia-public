//! The host tests' arena. Each seat's input is the turn number; each seat
//! answers with one value in [-1, 1]. It records every seat's last value as a
//! series, counts valid actions as a stat, and after twenty turns the seat
//! with the higher sum wins.

use std::cell::RefCell;

use botstadia_interface::bindings::arena::{Event, Finish, Guest, Turn, export_arena};
use botstadia_interface::declaration::*;
use botstadia_interface::payload::{self, Kind, Values};

const TURNS: u32 = 20;

fn declaration() -> Declaration {
    Declaration {
        arena_id: "host-fixture".into(),
        arena_version: "0.1.0".into(),
        changelog: vec![ChangelogEntry {
            version: "0.1.0".into(),
            changes: "First.".into(),
        }],
        payload_schema_version: 1,
        entrants: Entrants::Range { min: 1, max: 2 },
        inputs: vec![ValueDecl::new(
            "turn",
            Dtype::F32,
            &[1],
            Bounds::Uniform {
                min: 0.0,
                max: 1000.0,
            },
            "The turn number.",
        )],
        outputs: vec![ValueDecl::new(
            "value",
            Dtype::F32,
            &[1],
            Bounds::Uniform {
                min: -1.0,
                max: 1.0,
            },
            "Any value.",
        )],
        no_action: "the value is 0".into(),
        control_rate: 10,
        budgets: Budgets {
            turn_fuel: 1_000_000,
            init_fuel: 4_000_000,
        },
        max_turns: TURNS,
        wire: WireSizes {
            max_input_bytes: 64,
            max_output_bytes: 64,
            max_frame_bytes: 64,
        },
        stats: vec![StatDecl {
            key: "acted".into(),
            label: "Acted".into(),
            unit: "turns".into(),
            kind: StatKind::Count,
            form: Aggregate::Mean,
            better: Better::Higher,
            doc: "Turns with a valid action.".into(),
        }],
        series: vec![SeriesDecl {
            key: "value".into(),
            label: "Value".into(),
            unit: "".into(),
            scope: SeriesScope::PerSeat,
            doc: "Each seat's value.".into(),
        }],
        events: vec![],
        shaping_reward: "The seat's value each turn.".into(),
        min_trainer_version: "0.1.0".into(),
    }
}

#[derive(Default)]
struct State {
    seats: u32,
    turn: u32,
    sums: Vec<f32>,
    acted: Vec<f64>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn turn_out(state: &State, values: &[f32]) -> Turn {
    let decl = declaration();
    let input = payload::encode(
        Kind::Input,
        1,
        &decl.inputs,
        &[Values::F32(vec![state.turn as f32])],
    )
    .unwrap();
    Turn {
        inputs: vec![input; state.seats as usize],
        frame: state.turn.to_le_bytes().to_vec(),
        series: vec![values.to_vec()],
        events: Vec::<Event>::new(),
        shaping: values.to_vec(),
    }
}

struct Arena;

impl Guest for Arena {
    fn declaration() -> Vec<u8> {
        declaration().to_json()
    }

    fn setup(_seed: Vec<u8>, seats: u32) -> Result<Turn, String> {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            *state = State {
                seats,
                turn: 0,
                sums: vec![0.0; seats as usize],
                acted: vec![0.0; seats as usize],
            };
            Ok(turn_out(&state, &vec![0.0; seats as usize]))
        })
    }

    fn step(outputs: Vec<Option<Vec<u8>>>) -> Turn {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.turn += 1;
            let decl = declaration();
            let values: Vec<f32> = outputs
                .iter()
                .map(|output| match output {
                    Some(bytes) => payload::decode(Kind::Output, 1, &decl.outputs, bytes)
                        .ok()
                        .and_then(|v| v[0].get(0))
                        .unwrap_or(0.0),
                    None => 0.0,
                })
                .collect();
            for (seat, output) in outputs.iter().enumerate() {
                state.sums[seat] += values[seat];
                if output.is_some() {
                    state.acted[seat] += 1.0;
                }
            }
            turn_out(&state, &values)
        })
    }

    fn finished() -> Option<Finish> {
        STATE.with(|state| {
            let state = state.borrow();
            if state.turn < TURNS {
                return None;
            }
            let mut order: Vec<u32> = (0..state.seats).collect();
            order.sort_by(|a, b| state.sums[*b as usize].total_cmp(&state.sums[*a as usize]));
            Some(Finish {
                places: order.into_iter().map(|seat| vec![seat]).collect(),
                decided_by: "On sums".into(),
                stats: vec![state.acted.clone()],
            })
        })
    }
}

export_arena!(Arena with_types_in botstadia_interface::bindings::arena);
