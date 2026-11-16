//! The `arena` component's glue: one session per instance, held for the
//! instance's life.

use std::cell::RefCell;

pub use botstadia_interface as interface;
pub use botstadia_interface::bindings::arena::{Event, Finish, Turn};

use crate::session::{FinishOut, Session, TurnOut};
use crate::world::World;

pub struct Slot<W: World>(RefCell<Option<Session<W>>>);

impl<W: World> Default for Slot<W> {
    fn default() -> Self {
        Self(RefCell::new(None))
    }
}

fn turn(out: TurnOut) -> Turn {
    Turn {
        inputs: out.inputs,
        frame: out.frame,
        series: out.series,
        events: out
            .events
            .into_iter()
            .map(|(index, seat)| Event { index, seat })
            .collect(),
        shaping: out.shaping,
    }
}

fn finish(out: FinishOut) -> Finish {
    Finish {
        places: out.places,
        decided_by: out.decided_by,
        stats: out.stats,
    }
}

pub fn declaration<W: World>() -> Vec<u8> {
    W::declaration().to_json()
}

pub fn setup<W: World>(slot: &Slot<W>, seed: Vec<u8>, seats: u32) -> Result<Turn, String> {
    let (session, out) = Session::<W>::setup(&seed, seats)?;
    *slot.0.borrow_mut() = Some(session);
    Ok(turn(out))
}

pub fn step<W: World>(slot: &Slot<W>, outputs: Vec<Option<Vec<u8>>>) -> Turn {
    let mut session = slot.0.borrow_mut();
    let session = session.as_mut().expect("the host calls setup before step");
    turn(session.step(&outputs))
}

pub fn finished<W: World>(slot: &Slot<W>) -> Option<Finish> {
    slot.0
        .borrow_mut()
        .as_mut()
        .and_then(Session::finished)
        .map(finish)
}

/// Export a [`World`] as the `arena` WASM component. The calling crate
/// enables `arena-kit`'s `guest` feature.
#[macro_export]
macro_rules! export_world {
    ($world:ty) => {
        thread_local! {
            static __ARENA_KIT_SLOT: $crate::guest::Slot<$world> = ::core::default::Default::default();
        }

        struct __ArenaKitExport;

        impl $crate::guest::interface::bindings::arena::Guest for __ArenaKitExport {
            fn declaration() -> ::std::vec::Vec<u8> {
                $crate::guest::declaration::<$world>()
            }
            fn setup(seed: ::std::vec::Vec<u8>, seats: u32) -> ::core::result::Result<$crate::guest::Turn, ::std::string::String> {
                __ARENA_KIT_SLOT.with(|slot| $crate::guest::setup(slot, seed, seats))
            }
            fn step(outputs: ::std::vec::Vec<::core::option::Option<::std::vec::Vec<u8>>>) -> $crate::guest::Turn {
                __ARENA_KIT_SLOT.with(|slot| $crate::guest::step(slot, outputs))
            }
            fn finished() -> ::core::option::Option<$crate::guest::Finish> {
                __ARENA_KIT_SLOT.with(|slot| $crate::guest::finished(slot))
            }
        }

        $crate::guest::interface::bindings::arena::export_arena!(
            __ArenaKitExport with_types_in $crate::guest::interface::bindings::arena
        );
    };
}
