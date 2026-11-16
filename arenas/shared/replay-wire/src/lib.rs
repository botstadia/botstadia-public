//! The replay frame every MuJoCo arena writes, once per turn.
//!
//! A frame is complete state, never a delta, so the player shows any turn
//! from that turn's frame alone. It carries what's shared by every MuJoCo
//! arena (body poses, notable contacts, markers) and one opaque section the
//! arena's own player decodes (score, round, the next gate, the pose card).
//!
//! Coordinates are raw MuJoCo: Z-up, metres, quaternions `[w, x, y, z]`. The
//! sim instance's root in the scene carries the rotation to the renderer's
//! Y-up.
//!
//! Encoding: `b"RW"`, a format byte, then postcard. A reader refuses a format
//! it doesn't know rather than mis-reading it, and the player keeps a reader
//! for every format any served engine version emits.

use serde::{Deserialize, Serialize};

const MAGIC: &[u8; 2] = b"RW";
/// The format this crate writes. Bump on any change to these types, and keep
/// the old reader.
pub const FORMAT: u8 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Frame {
    pub turn: u32,
    /// Every body's `[x, y, z, qw, qx, qy, qz]`, in the model's body order
    /// ([`Setup::body_names`]).
    pub bodies: Vec<[f32; 7]>,
    /// The turn's notable contacts, above the arena's threshold.
    pub contacts: Vec<ContactNote>,
    /// Sim points without a body, bound to `marker_<name>` nodes.
    pub markers: Vec<Marker>,
    /// The arena's own state, for its own player.
    pub arena: Vec<u8>,
    /// On turn 0's frame only.
    pub setup: Option<Setup>,
}

/// Two bodies that touched hard enough to matter this turn. The player
/// turns it into sound and effects; the engine never knows about either.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ContactNote {
    /// Body indices, in model order.
    pub bodies: [u16; 2],
    /// The peak normal force over the turn, in newtons.
    pub force: f32,
    /// Where, world frame.
    pub point: [f32; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub name: String,
    pub pos: [f32; 3],
    pub radius: f32,
    pub visible: bool,
}

/// What the player needs once, before binding the scene.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Setup {
    /// The model's name, which names its visual sidecar.
    pub model: String,
    pub body_names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    NotAFrame,
    UnknownFormat(u8),
    Corrupt(String),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAFrame => write!(f, "not a MuJoCo replay frame"),
            Self::UnknownFormat(format) => write!(
                f,
                "replay frame format {format} isn't one this player reads"
            ),
            Self::Corrupt(text) => write!(f, "corrupt replay frame: {text}"),
        }
    }
}

impl std::error::Error for FrameError {}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(16 + self.bodies.len() * 28 + self.arena.len());
        bytes.extend_from_slice(MAGIC);
        bytes.push(FORMAT);
        bytes.extend_from_slice(&postcard::to_allocvec(self).expect("a frame always serialises"));
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, FrameError> {
        if bytes.len() < 3 || &bytes[..2] != MAGIC {
            return Err(FrameError::NotAFrame);
        }
        match bytes[2] {
            FORMAT => postcard::from_bytes(&bytes[3..])
                .map_err(|error| FrameError::Corrupt(error.to_string())),
            other => Err(FrameError::UnknownFormat(other)),
        }
    }

    /// A body's pose by name, given the setup's order.
    pub fn body(&self, setup: &Setup, name: &str) -> Option<[f32; 7]> {
        let index = setup.body_names.iter().position(|body| body == name)?;
        self.bodies.get(index).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_binds_by_name_and_refuses_strangers() {
        let setup = Setup {
            model: "pendulum".into(),
            body_names: vec!["world".into(), "arm".into()],
        };
        let frame = Frame {
            turn: 0,
            bodies: vec![[0.0; 7], [1.0, 2.0, 3.0, 1.0, 0.0, 0.0, 0.0]],
            contacts: vec![ContactNote {
                bodies: [0, 1],
                force: 12.5,
                point: [0.0, 0.0, 1.0],
            }],
            markers: vec![Marker {
                name: "goal".into(),
                pos: [1.0, 0.0, 0.0],
                radius: 0.2,
                visible: true,
            }],
            arena: vec![7, 7],
            setup: Some(setup.clone()),
        };
        let bytes = frame.encode();
        let back = Frame::decode(&bytes).unwrap();
        assert_eq!(back, frame);
        assert_eq!(back.body(&setup, "arm").unwrap()[0], 1.0);
        assert_eq!(back.body(&setup, "nope"), None);
        assert_eq!(Frame::decode(b"xx"), Err(FrameError::NotAFrame));
        let mut future = bytes;
        future[2] = 9;
        assert_eq!(Frame::decode(&future), Err(FrameError::UnknownFormat(9)));
    }

    #[test]
    fn a_humanoid_fits_well_under_the_frame_ceiling() {
        let frame = Frame {
            bodies: vec![[0.5; 7]; 40],
            ..Frame::default()
        };
        assert!(frame.encode().len() < 64 * 1024 / 20);
    }
}
