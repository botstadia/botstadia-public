//! Helpers for MuJoCo worlds: stepping a turn's sub-steps, noting the turn's
//! contacts, and building the shared replay frame.

use mujoco::MjSim;
use replay_wire::{ContactNote, Frame, Marker, Setup};

/// The turn's notable contacts, each body pair's peak normal force across
/// the sub-steps.
#[derive(Debug, Default, Clone)]
pub struct ContactTracker {
    peaks: Vec<ContactNote>,
}

impl ContactTracker {
    /// Note the current contacts. Call after every sub-step.
    pub fn observe(&mut self, sim: &MjSim) {
        for contact in sim.contacts() {
            let (a, b) = (
                contact.body1.min(contact.body2),
                contact.body1.max(contact.body2),
            );
            let bodies = [a as u16, b as u16];
            let force = contact.normal_force as f32;
            let point = contact.pos.map(|value| value as f32);
            match self.peaks.iter_mut().find(|note| note.bodies == bodies) {
                Some(note) if force > note.force => {
                    note.force = force;
                    note.point = point;
                }
                Some(_) => {}
                None => self.peaks.push(ContactNote {
                    bodies,
                    force,
                    point,
                }),
            }
        }
    }

    /// The turn's contacts at or above `threshold` newtons, strongest first,
    /// and a fresh start for the next turn. The threshold keeps frames small.
    pub fn take(&mut self, threshold: f32) -> Vec<ContactNote> {
        let mut notes: Vec<ContactNote> = self
            .peaks
            .drain(..)
            .filter(|note| note.force >= threshold)
            .collect();
        notes.sort_by(|a, b| b.force.total_cmp(&a.force));
        notes
    }
}

/// Step the sim `substeps` times, noting contacts along the way.
pub fn step_turn(sim: &mut MjSim, substeps: u32, contacts: &mut ContactTracker) {
    for _ in 0..substeps {
        sim.step();
        contacts.observe(sim);
    }
}

/// Every body's `[x, y, z, qw, qx, qy, qz]`, in model order.
pub fn body_poses(sim: &MjSim) -> Vec<[f32; 7]> {
    (0..sim.nbody())
        .map(|body| {
            let [x, y, z] = sim.body_pos(body);
            let [qw, qx, qy, qz] = sim.body_quat(body);
            [x, y, z, qw, qx, qy, qz].map(|value| value as f32)
        })
        .collect()
}

/// The shared frame for this turn. Turn 0's carries the setup.
pub fn frame(
    sim: &MjSim,
    model: &str,
    turn: u32,
    contacts: Vec<ContactNote>,
    markers: Vec<Marker>,
    arena: Vec<u8>,
) -> Frame {
    Frame {
        turn,
        bodies: body_poses(sim),
        contacts,
        markers,
        arena,
        setup: (turn == 0).then(|| Setup {
            model: model.to_string(),
            body_names: sim.body_names(),
        }),
    }
}
