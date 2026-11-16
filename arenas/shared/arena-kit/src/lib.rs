//! The engine side of an arena, written once.
//!
//! An arena's `engine` crate implements [`World`]; its `component` crate is a
//! few lines that call [`export_world!`]. The kit is the glue: it holds the
//! world, decodes outputs against the declaration, gives seats with no valid
//! action `None` so the world applies its declared no-action, encodes inputs,
//! and passes series, events, shaping and the result to the host. An arena
//! author writes no WIT and parses no bytes.
//!
//! - [`seed`]: labelled random streams from the arena's seed
//! - [`report`]: typed series, events, stats and the finishing order
//! - [`sim`]: MuJoCo turns, contacts and the shared replay frame
//! - [`session`]: one session, natively or inside the component
//! - [`testing`]: native sessions and the checks every arena's tests make

pub mod report;
pub mod seed;
pub mod session;
pub mod sim;
pub mod testing;
pub mod world;

pub use botstadia_interface::declaration;
pub use botstadia_interface::payload::Values;
pub use mujoco;
pub use replay_wire;
pub use report::{EventKey, Finish, Report, SeriesKey, StatKey};
pub use seed::{Rng, Seed};
pub use world::World;

#[cfg(feature = "guest")]
#[doc(hidden)]
pub mod guest;
