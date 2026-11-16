//! The World trait: everything an arena's engine is.

use botstadia_interface::declaration::Declaration;
use botstadia_interface::payload::Values;

use crate::report::{Finish, Report};
use crate::seed::Seed;

/// An arena's engine. `arena-kit` turns it into the `arena` WASM component
/// ([`export_world!`](crate::export_world)) and runs it natively for tests,
/// so an arena author writes no WIT and parses no bytes.
pub trait World: Sized + 'static {
    /// The declaration, from the arena's declaration crate.
    fn declaration() -> Declaration;

    /// Build the scenario. Each random choice comes from a stream of `seed`
    /// under its own label. Seats are already drawn by the host. Report
    /// turn 0's series here.
    fn setup(seed: &Seed, seats: u32, report: &mut Report) -> Result<Self, String>;

    /// Seat `seat`'s input for the coming turn, in declared order. Egocentric:
    /// in the seat's own frame.
    fn input(&self, seat: u32) -> Vec<Values>;

    /// Advance one turn. `outputs[seat]` is `None` when that seat has no
    /// valid action, and the world applies its declared no-action. Outputs
    /// are inside their declared bounds and are never clamped or corrected.
    /// Never trap: handle a numerical blow-up by a rule of the game.
    fn step(&mut self, outputs: &[Option<Vec<Values>>], report: &mut Report);

    /// This turn's replay frame: complete state, never a delta.
    fn frame(&self) -> Vec<u8>;

    /// The result, once the game has ended. `out_of_turns` is set once
    /// maximum turns has arrived, and the world must then return a result:
    /// reaching it is a real ending, decided by the arena's rules.
    fn finish(&self, out_of_turns: bool) -> Option<Finish>;
}
