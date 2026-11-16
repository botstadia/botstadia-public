//! An arena's engine on wasmtime.
//!
//! The arena is trusted once published, so it is never limited, and runs
//! synchronously on the session's own thread. It gets the empty sandbox:
//! randomness from its own stream of the seed, a virtual clock, nothing else.
//!
//! A metered arena ([`Metering::Metered`], the default) counts its work in
//! arena fuel: setup's and each step's, machine-independent, for the
//! step-cost figures and the session's safety limit. Training may load it
//! unmetered for speed.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use botstadia_interface::bindings::host::arena::{Arena, Finish, Turn};
use botstadia_interface::declaration::Declaration;
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

use crate::WasmtimeContext;
use crate::seed::Seed;

/// Whether an arena counts its work in fuel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Metering {
    /// Count setup's and each step's fuel (never limited).
    #[default]
    Metered,
    /// Count nothing: a little faster, for training.
    Unmetered,
}

/// Fuel given to a metered arena store before each call: never reached, so
/// the arena is counted, never limited.
const UNLIMITED: u64 = u64::MAX / 2;

/// The process's arena engine for a metering.
pub fn engine(metering: Metering) -> &'static Engine {
    static METERED: OnceLock<Engine> = OnceLock::new();
    static UNMETERED: OnceLock<Engine> = OnceLock::new();
    let (cell, metered) = match metering {
        Metering::Metered => (&METERED, true),
        Metering::Unmetered => (&UNMETERED, false),
    };
    cell.get_or_init(|| {
        Engine::new(&crate::budget::arena_config(metered)).expect("the arena engine")
    })
}

pub(crate) struct ArenaState {
    wasi: WasiCtx,
    table: ResourceTable,
}

impl WasiView for ArenaState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// A compiled arena and its declaration.
#[derive(Clone)]
pub struct LoadedArena {
    component: Component,
    linker: Arc<Linker<ArenaState>>,
    pub declaration: Arc<Declaration>,
    /// The declaration's bytes, as the engine returned them: what a bot's
    /// `declaration` import returns.
    pub declaration_json: Arc<Vec<u8>>,
    /// `sha256-<hex>` of the engine.
    pub digest: String,
    pub metering: Metering,
}

impl LoadedArena {
    /// Load a metered arena.
    pub fn load(path: &Path) -> Result<Self> {
        Self::load_with(path, Metering::Metered)
    }

    pub fn load_with(path: &Path, metering: Metering) -> Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading the arena at {}", path.display()))?;
        Self::from_bytes_with(&bytes, metering)
    }

    /// A metered arena from its bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        Self::from_bytes_with(bytes, Metering::Metered)
    }

    pub fn from_bytes_with(bytes: &[u8], metering: Metering) -> Result<Self> {
        let engine = engine(metering);
        let component = Component::from_binary(engine, bytes).ctx("compiling the arena")?;
        let mut linker = Linker::<ArenaState>::new(engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).ctx("linking WASI")?;
        let linker = Arc::new(linker);
        let mut store = new_store(&[0; 32], metering);
        let instance =
            Arena::instantiate(&mut store, &component, &linker).ctx("instantiating the arena")?;
        let json = instance
            .call_declaration(&mut store)
            .ctx("calling the arena's declaration()")?;
        let declaration =
            Declaration::from_json(&json).context("decoding the arena's declaration")?;
        Ok(Self {
            component,
            linker,
            declaration: Arc::new(declaration),
            declaration_json: Arc::new(json),
            digest: crate::digest(bytes),
            metering,
        })
    }

    /// A fresh instance for one session.
    pub(crate) fn instantiate(&self, stream: &Seed) -> Result<ArenaInstance> {
        let mut store = new_store(stream, self.metering);
        let arena = Arena::instantiate(&mut store, &self.component, &self.linker)
            .ctx("instantiating the arena")?;
        Ok(ArenaInstance {
            store,
            arena,
            metered: self.metering == Metering::Metered,
            pending_fuel: 0,
        })
    }
}

fn new_store(stream: &Seed, metering: Metering) -> Store<ArenaState> {
    let wasi = crate::seat::sandbox_wasi(stream, None, &[]);
    let mut store = Store::new(
        engine(metering),
        ArenaState {
            wasi,
            table: ResourceTable::new(),
        },
    );
    if metering == Metering::Metered {
        store
            .set_fuel(UNLIMITED)
            .expect("a metered store takes fuel");
    }
    store
}

pub(crate) struct ArenaInstance {
    store: Store<ArenaState>,
    arena: Arena,
    metered: bool,
    /// Arena fuel counted since the last [`ArenaInstance::take_fuel`].
    pending_fuel: u64,
}

impl ArenaInstance {
    /// Run one call into the arena, counting its fuel when metered.
    fn counted<T>(
        &mut self,
        call: impl FnOnce(&Arena, &mut Store<ArenaState>) -> wasmtime::Result<T>,
    ) -> wasmtime::Result<T> {
        if !self.metered {
            return call(&self.arena, &mut self.store);
        }
        self.store.set_fuel(UNLIMITED)?;
        let result = call(&self.arena, &mut self.store);
        let left = self.store.get_fuel()?;
        self.pending_fuel = self.pending_fuel.saturating_add(UNLIMITED - left);
        result
    }

    /// The arena fuel counted since the last take: one turn's (its `step`
    /// and `finished` calls) or setup's. `None` when unmetered.
    pub fn take_fuel(&mut self) -> Option<u64> {
        let fuel = std::mem::take(&mut self.pending_fuel);
        self.metered.then_some(fuel)
    }

    pub fn setup(&mut self, seed: &[u8], seats: u32) -> Result<Result<Turn, String>> {
        self.counted(|arena, store| arena.call_setup(store, seed, seats))
            .ctx("the arena failed in setup")
    }

    pub fn step(&mut self, outputs: &[Option<Vec<u8>>]) -> Result<Turn> {
        self.counted(|arena, store| arena.call_step(store, outputs))
            .ctx("the arena failed")
    }

    pub fn finished(&mut self) -> Result<Option<Finish>> {
        self.counted(|arena, store| arena.call_finished(store))
            .ctx("the arena failed")
    }
}
