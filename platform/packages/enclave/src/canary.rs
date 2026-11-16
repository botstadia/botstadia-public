//! The backstop's clock (BOTS.md#the-backstop): a canary loop on a core of
//! its own, touching memory as well as computing, ticking at a calibrated
//! rate. Seats' turns, loads, compiles and the safety limit are timed in its
//! ticks, so a pause or slowdown of the whole machine slows the clock with
//! it and never fires a backstop.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The canary's memory: larger than a core's L2, so each tick touches
/// memory, not just cache.
const BUFFER: usize = 4 << 20;
/// Bytes walked per tick (one cache line in every 64 bytes).
const STRIDE: usize = 64;
const LINES_PER_TICK: usize = 4096;

/// The running canary.
pub struct Canary {
    ticks: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
}

impl Canary {
    /// Start the canary on its own thread, pinned to `core` where the
    /// platform allows.
    pub fn start(core: Option<usize>) -> anyhow::Result<Self> {
        let ticks = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (t, s) = (ticks.clone(), stop.clone());
        std::thread::Builder::new()
            .name("canary".into())
            .spawn(move || {
                if let Some(core) = core {
                    let _ = botstadia_sandbox::pin(&[core]);
                }
                let mut buf = vec![0u8; BUFFER];
                let mut at = 0usize;
                let mut acc = 0u64;
                while !s.load(Ordering::Relaxed) {
                    for _ in 0..LINES_PER_TICK {
                        acc = acc
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(buf[at] as u64);
                        buf[at] = acc as u8;
                        at = (at + STRIDE) % BUFFER;
                    }
                    t.fetch_add(1, Ordering::Relaxed);
                }
                std::hint::black_box(acc);
            })?;
        Ok(Self { ticks, stop })
    }

    /// A clock reading the canary.
    pub fn clock(&self) -> Ticks {
        Ticks(self.ticks.clone())
    }
}

impl Drop for Canary {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// A reading of the canary: cheap to clone, read from any thread.
#[derive(Clone)]
pub struct Ticks(Arc<AtomicU64>);

impl Ticks {
    pub fn now(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    /// A clock that never moves, for tests that must never time out.
    pub fn frozen() -> Self {
        Self(Arc::new(AtomicU64::new(0)))
    }
}

/// This machine measured against the image's calibration: canary ticks
/// and WASM fuel a second, over `seconds` of the canary. Reported on boot so
/// the constants below are set from the published CPU class itself.
pub fn measure(clock: &Ticks, seconds: f64) -> anyhow::Result<(f64, f64)> {
    let started = std::time::Instant::now();
    let from = clock.now();
    std::thread::sleep(std::time::Duration::from_secs_f64(seconds));
    let ticks = (clock.now() - from) as f64 / started.elapsed().as_secs_f64();
    let fuel = 1e9 / botstadia_host::calibrate::ns_per_fuel(50_000_000)?;
    Ok((ticks, fuel))
}

/// The calibration (constants of the image, published with the CPU class).
///
/// Measured on the published CPU class itself: a `c3-standard-8` with TDX
/// and SMT off, under Confidential Space, on 2026-11-10 (102,600 canary
/// ticks and 1.015e10 WASM fuel a second; every enclave reports its own
/// rates on boot, in its heartbeat's `calibration`).
pub mod calibration {
    /// Canary ticks a second on the CPU class.
    pub const TICKS_PER_SECOND: u64 = 102_600;
    /// WASM fuel a second on the CPU class.
    pub const FUEL_PER_SECOND: u64 = 10_150_000_000;
    /// The backstop's multiple of a full turn's calibrated time.
    pub const BACKSTOP: u64 = 10;
    /// A process that hasn't stopped this long after it was told to is
    /// killed: half a second.
    pub const KILL_GRACE_TICKS: u64 = TICKS_PER_SECOND / 2;
    /// Unzip, model parsing, preflight and compile, before the backstop's
    /// multiple: 30 seconds.
    pub const COMPILE_CEILING_TICKS: u64 = 30 * TICKS_PER_SECOND;
    /// A bundle's model loads, before the backstop's multiple: 10 seconds.
    pub const LOAD_CEILING_TICKS: u64 = 10 * TICKS_PER_SECOND;

    /// A full turn of `fuel`, in ticks, at the calibrated rates.
    pub fn ticks_for_fuel(fuel: u64) -> u64 {
        ((fuel as u128 * TICKS_PER_SECOND as u128).div_ceil(FUEL_PER_SECOND as u128)) as u64
    }

    /// The backstop for a turn of `fuel`: never under one tick.
    pub fn backstop(fuel: u64) -> u64 {
        ticks_for_fuel(fuel).max(1) * BACKSTOP
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_canary_ticks_and_stops() {
        let canary = Canary::start(None).unwrap();
        let clock = canary.clock();
        let started = std::time::Instant::now();
        while clock.now() < 10 && started.elapsed().as_secs() < 10 {
            std::thread::yield_now();
        }
        assert!(clock.now() >= 10);
        drop(canary);
    }

    /// This machine's rates (`--ignored --nocapture`, in release).
    #[test]
    #[ignore = "a measurement, not a check"]
    fn measure_this_machine() {
        let canary = Canary::start(None).unwrap();
        let (ticks, fuel) = measure(&canary.clock(), 3.0).unwrap();
        println!("canary ticks/s {ticks:.0}, fuel/s {fuel:.3e}");
    }

    #[test]
    fn the_backstop_scales_with_fuel_and_has_a_floor() {
        use calibration::*;
        assert_eq!(backstop(0), BACKSTOP);
        assert!(backstop(FUEL_PER_SECOND) >= TICKS_PER_SECOND * BACKSTOP);
        assert!(backstop(2 * FUEL_PER_SECOND) > backstop(FUEL_PER_SECOND));
    }
}
