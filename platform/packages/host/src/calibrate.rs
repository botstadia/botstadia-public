//! Measuring this machine against the budget version's rates: what the
//! enclave reports on boot (and the calibration test prints), so the image's
//! canary and fuel constants are set from the published CPU class itself.

use std::time::Instant;

use crate::budget;

/// Nanoseconds per unit of WASM fuel, from a tight loop of the cost table's
/// ordinary instructions, on the seat's own engine settings.
pub fn ns_per_fuel(iterations: i64) -> anyhow::Result<f64> {
    let wat = r#"(module (func (export "run") (param $n i64) (result i64)
        (local $acc i64)
        (loop $l
          (local.set $acc (i64.add (i64.mul (local.get $acc) (i64.const 6364136223846793005)) (local.get $n)))
          (local.set $n (i64.sub (local.get $n) (i64.const 1)))
          (br_if $l (i64.ne (local.get $n) (i64.const 0))))
        (local.get $acc)))"#;
    let engine = wasmtime::Engine::new(budget::current().seat_config().consume_fuel(true))
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let module = wasmtime::Module::new(&engine, wat).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut store = wasmtime::Store::new(&engine, ());
    store
        .set_fuel(u64::MAX / 2)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let instance =
        wasmtime::Instance::new(&mut store, &module, &[]).map_err(|e| anyhow::anyhow!("{e}"))?;
    let run = instance
        .get_typed_func::<i64, i64>(&mut store, "run")
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let before = store.get_fuel().map_err(|e| anyhow::anyhow!("{e}"))?;
    let started = Instant::now();
    run.call(&mut store, iterations)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let elapsed = started.elapsed().as_secs_f64();
    let spent = before - store.get_fuel().map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(elapsed * 1e9 / spent as f64)
}
