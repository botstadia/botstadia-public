//! The host tests' bots. Each feature is one behaviour; every one answers the
//! fixture arena's input (the previous turn's number) with a value in [-1, 1].

// Each build enables one behaviour, so the others' assignments go unread.
#![allow(unused_assignments)]

use std::cell::Cell;

#[allow(unused_imports)]
use botstadia_interface::bindings::agent::botstadia::agent::host;
use botstadia_interface::bindings::agent::{Guest, export_agent};

thread_local! {
    static TURNS_SEEN: Cell<u32> = const { Cell::new(0) };
}

/// The fixture arena's input: header, then one f32 (the turn).
fn turn_of(input: &[u8]) -> u32 {
    input
        .get(12..16)
        .map_or(0, |b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u32)
}

fn output(value: f32) -> Vec<u8> {
    let mut out = b"BSPO".to_vec();
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&4u32.to_le_bytes());
    out.extend_from_slice(&value.to_le_bytes());
    out
}

/// Spend about `n` loop iterations of work the optimiser can't remove.
#[inline(never)]
fn burn(n: u64) -> u64 {
    let mut acc = 0u64;
    for i in 0..n {
        acc = std::hint::black_box(acc.wrapping_mul(6364136223846793005).wrapping_add(i));
    }
    acc
}

struct Bot;

impl Guest for Bot {
    fn init() {
        if cfg!(feature = "init_trap") {
            panic!("the fixture traps in init");
        }
        TURNS_SEEN.with(|t| t.set(0));
    }

    fn on_tick(input: Vec<u8>) -> Vec<u8> {
        let turn = turn_of(&input);
        TURNS_SEEN.with(|t| t.set(t.get() + 1));
        let seen = TURNS_SEEN.with(|t| t.get());
        if cfg!(feature = "spiky") && turn % 4 == 2 {
            // Far past one turn's fuel: pauses, finishes next turn.
            std::hint::black_box(burn(150_000));
        }
        if cfg!(feature = "forever") {
            loop {
                std::hint::black_box(burn(1_000));
            }
        }
        if cfg!(feature = "trap") && turn == 5 {
            panic!("the fixture traps on turn 5");
        }
        if cfg!(feature = "memcopy") {
            // 16 MiB copied: a bulk operation charged per byte.
            let src = vec![1u8; 16 << 20];
            let mut dst = vec![0u8; 16 << 20];
            dst.copy_from_slice(&src);
            std::hint::black_box(&dst);
        }
        if cfg!(feature = "sleep") {
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
        let mut value = 0.5;
        if cfg!(feature = "clock") {
            let start = std::time::Instant::now();
            std::hint::black_box(burn(10_000));
            let elapsed = start.elapsed().as_nanos() as f32;
            value = (elapsed / 1e6).min(1.0);
        }
        #[cfg(feature = "budget")]
        {
            let budget = host::budget();
            // Answers 1 when the turn number matches and fuel is left.
            value = if budget.turn == turn && budget.fuel_left > 0 {
                1.0
            } else {
                -1.0
            };
        }
        #[cfg(feature = "declaration")]
        {
            let json = String::from_utf8(host::declaration()).unwrap_or_default();
            value = if json.contains("\"host-fixture\"") {
                1.0
            } else {
                -1.0
            };
        }
        #[cfg(feature = "handles")]
        if turn == 1 {
            // Opens a stream on every iteration and never closes one.
            for _ in 0..5_000 {
                std::mem::forget(wasip2::cli::stdout::get_stdout());
            }
        }
        #[cfg(feature = "badcalls")]
        {
            // A hundred calls to a model the bundle doesn't have, each
            // passing a thousand elements.
            let tensor = host::Tensor {
                name: "x".into(),
                shape: vec![1, 1000],
                data: vec![0.0; 1000],
            };
            for _ in 0..100 {
                let _ = host::infer(99, std::slice::from_ref(&tensor));
            }
        }
        #[cfg(feature = "bigcall")]
        if turn == 1 {
            // 32 MiB handed to the host in one call: far past any input its
            // model takes.
            let _ = host::infer(
                0,
                &[host::Tensor {
                    name: "x".into(),
                    shape: vec![1, 8 << 20],
                    data: vec![0.0; 8 << 20],
                }],
            );
        }
        #[cfg(feature = "sockets")]
        {
            // Answers 1 only if every way to a socket is refused.
            use wasip2::sockets::network::IpAddressFamily;
            let tcp = wasip2::sockets::tcp_create_socket::create_tcp_socket(IpAddressFamily::Ipv4);
            let udp = wasip2::sockets::udp_create_socket::create_udp_socket(IpAddressFamily::Ipv4);
            let lookup = wasip2::sockets::ip_name_lookup::resolve_addresses(
                &wasip2::sockets::instance_network::instance_network(),
                "localhost",
            );
            value = if tcp.is_err() && udp.is_err() && lookup.is_err() {
                1.0
            } else {
                -1.0
            };
        }
        #[cfg(feature = "infer")]
        {
            let result = host::infer(
                0,
                &[host::Tensor {
                    name: "x".into(),
                    shape: vec![1, 1],
                    data: vec![turn as f32],
                }],
            );
            value = match result {
                Ok(outputs) => outputs
                    .first()
                    .and_then(|t| t.data.first())
                    .copied()
                    .unwrap_or(-1.0)
                    .clamp(-1.0, 1.0),
                Err(_) => -1.0,
            };
        }
        #[cfg(feature = "infer_many")]
        {
            for i in 0..INFER_MANY {
                if let Ok(outputs) = host::infer(
                    0,
                    &[host::Tensor {
                        name: "x".into(),
                        shape: vec![1, 1],
                        data: vec![i as f32],
                    }],
                ) {
                    value = outputs
                        .first()
                        .and_then(|t| t.data.first())
                        .copied()
                        .unwrap_or(-1.0)
                        .clamp(-1.0, 1.0);
                }
            }
        }
        let _ = seen;
        output(value)
    }
}

export_agent!(Bot with_types_in botstadia_interface::bindings::agent);

/// The calls a turn `infer_many` makes.
#[cfg(feature = "infer_many")]
const INFER_MANY: u32 = 400;
