//! Safe MuJoCo for arena engines: load a baked `.mjb` (collision geometry
//! only; visuals go to the scene through the renderer's sidecar), step it,
//! read poses and contacts, and make seed-derived scenario edits.
//!
//! # Errors
//!
//! `mju_error` in the vendored engine prints and exits, which inside a WASM
//! component is a trap, and an arena trap refunds every bet. So a world never
//! drives the engine into an error path: models are baked with a generously
//! sized arena, the host only ever passes outputs inside the declared bounds
//! (which equal the actuators' `ctrlrange`), and the one fallible call is
//! [`MjSim::load`], which returns `Err` for a corrupt `.mjb`.
//!
//! Numerical blow-ups are a game matter: [`MjSim::diverged`] tells a world
//! its state went non-finite, and the world's rules decide what happens (a
//! reset, or a loss for whoever caused it). Never a trap.

use std::ffi::{CStr, CString};

use mujoco_sys as sys;
use sys::{mjw_data, mjw_model};

/// One active contact.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Contact {
    pub geom1: usize,
    pub geom2: usize,
    pub body1: usize,
    pub body2: usize,
    /// Normal force magnitude (N) from the constraint solver.
    pub normal_force: f64,
    /// Contact point, world frame.
    pub pos: [f64; 3],
}

/// One loaded model + its simulation state.
pub struct MjSim {
    model: *const mjw_model,
    data: *mut mjw_data,
    nq: usize,
    nv: usize,
    nu: usize,
    nbody: usize,
    ngeom: usize,
}

// The component runs single-threaded; the pointers never leave the instance.
unsafe impl Send for MjSim {}

impl Drop for MjSim {
    fn drop(&mut self) {
        unsafe {
            sys::mjw_delete_data(self.data);
            sys::mjw_delete_model(self.model);
        }
    }
}

fn obj(type_: i32, m: *const mjw_model, name: &str) -> Option<usize> {
    let c = CString::new(name).ok()?;
    let id = unsafe { sys::mjw_name2id(m, type_, c.as_ptr()) };
    usize::try_from(id).ok()
}

impl MjSim {
    /// Load a `.mjb` baked by TOOLS's `bake-model` with MuJoCo 3.11.0. A
    /// `.mjb` is locked to its MuJoCo version.
    pub fn load(mjb: &[u8]) -> Result<Self, String> {
        let model = unsafe { sys::mjw_load_model(mjb.as_ptr(), mjb.len() as i32) };
        if model.is_null() {
            return Err("mj_loadModelBuffer failed (corrupt or version-mismatched .mjb)".into());
        }
        let data = unsafe { sys::mjw_make_data(model) };
        if data.is_null() {
            unsafe { sys::mjw_delete_model(model) };
            return Err("mj_makeData failed".into());
        }
        let mut sim = Self {
            model,
            data,
            nq: unsafe { sys::mjw_nq(model) } as usize,
            nv: unsafe { sys::mjw_nv(model) } as usize,
            nu: unsafe { sys::mjw_nu(model) } as usize,
            nbody: unsafe { sys::mjw_nbody(model) } as usize,
            ngeom: unsafe { sys::mjw_ngeom(model) } as usize,
        };
        sim.forward();
        Ok(sim)
    }

    /// Every body name, in model order: the seam to the scene.
    pub fn body_names(&self) -> Vec<String> {
        (0..self.nbody)
            .map(|i| self.body_name(i).unwrap_or_else(|| format!("body{i}")))
            .collect()
    }

    /// The mutable model pointer for edits (private; the Sim methods are the
    /// public surface).
    fn model_mut(&mut self) -> *mut mjw_model {
        self.model.cast_mut()
    }

    /// Recompute derived model quantities after mass/inertia edits.
    ///
    /// STATE-PRESERVING: `mj_setConst` computes reference-configuration
    /// constants by loading `qpos0` into the data — which would silently
    /// wipe a keyframe reset, which has happened before. Save and
    /// restore the full input state.
    fn recompute_const(&mut self) {
        let qpos = self.qpos().to_vec();
        let qvel = self.qvel().to_vec();
        let ctrl = self.ctrl().to_vec();
        unsafe { sys::mjw_set_const(self.model_mut(), self.data) };
        self.set_qpos(&qpos);
        self.set_qvel(&qvel);
        self.set_ctrl(&ctrl);
        self.forward();
    }
}

impl MjSim {
    pub fn timestep(&self) -> f64 {
        unsafe { sys::mjw_timestep(self.model) }
    }
    pub fn nq(&self) -> usize {
        self.nq
    }
    pub fn nv(&self) -> usize {
        self.nv
    }
    pub fn nu(&self) -> usize {
        self.nu
    }
    pub fn nbody(&self) -> usize {
        self.nbody
    }
    pub fn ngeom(&self) -> usize {
        self.ngeom
    }

    pub fn body_id(&self, name: &str) -> Option<usize> {
        obj(unsafe { sys::mjw_obj_body() }, self.model, name)
    }
    pub fn body_name(&self, id: usize) -> Option<String> {
        let p = unsafe { sys::mjw_id2name(self.model, sys::mjw_obj_body(), id as i32) };
        if p.is_null() {
            return None;
        }
        Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    }
    pub fn geom_id(&self, name: &str) -> Option<usize> {
        obj(unsafe { sys::mjw_obj_geom() }, self.model, name)
    }
    pub fn geom_body(&self, geom: usize) -> usize {
        unsafe { sys::mjw_geom_bodyid(self.model, geom as i32) as usize }
    }
    /// A body's parent body (the world's is itself, 0).
    pub fn body_parent(&self, body: usize) -> usize {
        unsafe { sys::mjw_body_parentid(self.model, body as i32) as usize }
    }
    /// A geom's contact type and affinity: whether it collides at all.
    pub fn geom_collides(&self, geom: usize) -> bool {
        unsafe {
            sys::mjw_geom_contype(self.model, geom as i32) != 0
                || sys::mjw_geom_conaffinity(self.model, geom as i32) != 0
        }
    }
    /// The signed distance between two geoms in the current poses (negative
    /// when they overlap), up to `distmax`.
    pub fn geom_distance(&mut self, geom1: usize, geom2: usize, distmax: f64) -> f64 {
        unsafe {
            sys::mjw_geom_distance(self.model, self.data, geom1 as i32, geom2 as i32, distmax)
        }
    }
    pub fn site_id(&self, name: &str) -> Option<usize> {
        obj(unsafe { sys::mjw_obj_site() }, self.model, name)
    }
    pub fn joint_id(&self, name: &str) -> Option<usize> {
        obj(unsafe { sys::mjw_obj_joint() }, self.model, name)
    }
    /// A body's first joint, e.g. a free body's free joint, or `None` for a
    /// body with no joints.
    pub fn body_joint(&self, body: usize) -> Option<usize> {
        unsafe {
            if sys::mjw_body_jntnum(self.model, body as i32) < 1 {
                return None;
            }
            usize::try_from(sys::mjw_body_jntadr(self.model, body as i32)).ok()
        }
    }
    pub fn joint_addr(&self, joint: usize) -> (usize, usize) {
        unsafe {
            (
                sys::mjw_jnt_qposadr(self.model, joint as i32) as usize,
                sys::mjw_jnt_dofadr(self.model, joint as i32) as usize,
            )
        }
    }
    pub fn joint_range(&self, joint: usize) -> Option<(f64, f64)> {
        unsafe {
            if sys::mjw_jnt_limited(self.model, joint as i32) == 0 {
                return None;
            }
            let mut out = [0.0; 2];
            sys::mjw_jnt_range(self.model, joint as i32, out.as_mut_ptr());
            Some((out[0], out[1]))
        }
    }
    /// The joint's actuator force range (`jnt_actfrcrange`), where MuJoCo
    /// clamps the summed actuator force on a joint, or `None` when the joint
    /// has no such limit.
    pub fn joint_actfrcrange(&self, joint: usize) -> Option<(f64, f64)> {
        unsafe {
            if sys::mjw_jnt_actfrclimited(self.model, joint as i32) == 0 {
                return None;
            }
            let mut out = [0.0; 2];
            sys::mjw_jnt_actfrcrange(self.model, joint as i32, out.as_mut_ptr());
            Some((out[0], out[1]))
        }
    }
    /// Set a joint's actuator force range: a derated motor's torque limit.
    /// It only takes effect on a joint whose limit is on
    /// (`actuatorfrclimited`), and `lo <= hi` is the caller's to keep.
    pub fn set_joint_actfrcrange(&mut self, joint: usize, range: (f64, f64)) {
        let r = [range.0, range.1];
        unsafe { sys::mjw_set_jnt_actfrcrange(self.model_mut(), joint as i32, r.as_ptr()) };
    }
    pub fn actuator_id(&self, name: &str) -> Option<usize> {
        obj(unsafe { sys::mjw_obj_actuator() }, self.model, name)
    }
    pub fn actuator_ctrlrange(&self, actuator: usize) -> Option<(f64, f64)> {
        unsafe {
            if sys::mjw_actuator_ctrllimited(self.model, actuator as i32) == 0 {
                return None;
            }
            let mut out = [0.0; 2];
            sys::mjw_actuator_ctrlrange(self.model, actuator as i32, out.as_mut_ptr());
            Some((out[0], out[1]))
        }
    }
    pub fn keyframe_id(&self, name: &str) -> Option<usize> {
        obj(unsafe { sys::mjw_obj_key() }, self.model, name)
    }

    pub fn time(&self) -> f64 {
        unsafe { sys::mjw_time(self.data) }
    }
    pub fn qpos(&self) -> &[f64] {
        unsafe { core::slice::from_raw_parts(sys::mjw_qpos(self.data), self.nq) }
    }
    pub fn qvel(&self) -> &[f64] {
        unsafe { core::slice::from_raw_parts(sys::mjw_qvel(self.data), self.nv) }
    }
    pub fn set_qpos(&mut self, qpos: &[f64]) {
        let n = qpos.len().min(self.nq);
        unsafe {
            core::slice::from_raw_parts_mut(sys::mjw_qpos(self.data), self.nq)[..n]
                .copy_from_slice(&qpos[..n]);
        }
    }
    pub fn set_qvel(&mut self, qvel: &[f64]) {
        let n = qvel.len().min(self.nv);
        unsafe {
            core::slice::from_raw_parts_mut(sys::mjw_qvel(self.data), self.nv)[..n]
                .copy_from_slice(&qvel[..n]);
        }
    }
    pub fn ctrl(&self) -> &[f64] {
        unsafe { core::slice::from_raw_parts(sys::mjw_ctrl(self.data), self.nu) }
    }
    pub fn set_ctrl(&mut self, ctrl: &[f64]) {
        let n = ctrl.len().min(self.nu);
        unsafe {
            core::slice::from_raw_parts_mut(sys::mjw_ctrl(self.data), self.nu)[..n]
                .copy_from_slice(&ctrl[..n]);
        }
    }
    pub fn reset(&mut self, keyframe: Option<usize>) {
        unsafe {
            match keyframe {
                Some(k) => sys::mjw_reset_data_keyframe(self.model, self.data, k as i32),
                None => sys::mjw_reset_data(self.model, self.data),
            }
            sys::mjw_forward(self.model, self.data);
        }
    }

    pub fn set_body_mass(&mut self, body: usize, mass: f64) {
        unsafe { sys::mjw_set_body_mass(self.model_mut(), body as i32, mass) };
        self.recompute_const();
    }
    pub fn set_geom_friction(&mut self, geom: usize, friction: [f64; 3]) {
        unsafe { sys::mjw_set_geom_friction(self.model_mut(), geom as i32, friction.as_ptr()) };
    }
    pub fn set_free_body_pose(&mut self, body: usize, pos: [f64; 3], quat: [f64; 4]) {
        unsafe {
            let jntadr = sys::mjw_body_jntadr(self.model, body as i32);
            if jntadr < 0
                || sys::mjw_body_jntnum(self.model, body as i32) != 1
                || sys::mjw_jnt_type(self.model, jntadr) != sys::mjw_jnt_free()
            {
                return; // not a free body: a bug in the world, but never a trap
            }
            let adr = sys::mjw_jnt_qposadr(self.model, jntadr) as usize;
            let qpos = core::slice::from_raw_parts_mut(sys::mjw_qpos(self.data), self.nq);
            qpos[adr..adr + 3].copy_from_slice(&pos);
            qpos[adr + 3..adr + 7].copy_from_slice(&quat);
        }
    }
    pub fn set_body_pos(&mut self, body: usize, pos: [f64; 3]) {
        unsafe { sys::mjw_set_body_pos(self.model_mut(), body as i32, pos.as_ptr()) };
    }
    /// Set a body's orientation in its parent's frame (`body_quat`), e.g. a
    /// static gate's yaw drawn from the seed. Takes effect from the next
    /// `forward()` or step.
    pub fn set_body_quat(&mut self, body: usize, quat: [f64; 4]) {
        unsafe { sys::mjw_set_body_quat(self.model_mut(), body as i32, quat.as_ptr()) };
    }
    pub fn set_site_pos(&mut self, site: usize, pos: [f64; 3]) {
        unsafe { sys::mjw_set_site_pos(self.model_mut(), site as i32, pos.as_ptr()) };
    }

    pub fn step(&mut self) {
        unsafe { sys::mjw_step(self.model, self.data) };
    }
    pub fn forward(&mut self) {
        unsafe { sys::mjw_forward(self.model, self.data) };
    }
    pub fn apply_body_wrench(&mut self, body: usize, force: [f64; 3], torque: [f64; 3]) {
        unsafe {
            let xfrc =
                core::slice::from_raw_parts_mut(sys::mjw_xfrc_applied(self.data), self.nbody * 6);
            let base = body * 6;
            xfrc[base..base + 3].copy_from_slice(&force);
            xfrc[base + 3..base + 6].copy_from_slice(&torque);
        }
    }

    pub fn body_pos(&self, body: usize) -> [f64; 3] {
        let mut out = [0.0; 3];
        unsafe { sys::mjw_body_xpos(self.data, body as i32, out.as_mut_ptr()) };
        out
    }
    pub fn body_quat(&self, body: usize) -> [f64; 4] {
        let mut out = [0.0; 4];
        unsafe { sys::mjw_body_xquat(self.data, body as i32, out.as_mut_ptr()) };
        out
    }
    pub fn body_linvel(&self, body: usize) -> [f64; 3] {
        let mut v = [0.0; 6];
        unsafe { sys::mjw_body_velocity(self.model, self.data, body as i32, v.as_mut_ptr()) };
        [v[3], v[4], v[5]]
    }
    pub fn body_angvel(&self, body: usize) -> [f64; 3] {
        let mut v = [0.0; 6];
        unsafe { sys::mjw_body_velocity(self.model, self.data, body as i32, v.as_mut_ptr()) };
        [v[0], v[1], v[2]]
    }
    pub fn body_mass(&self, body: usize) -> f64 {
        unsafe { sys::mjw_body_mass(self.model, body as i32) }
    }
    pub fn site_pos(&self, site: usize) -> [f64; 3] {
        let mut out = [0.0; 3];
        unsafe { sys::mjw_site_xpos(self.data, site as i32, out.as_mut_ptr()) };
        out
    }
    pub fn geom_pos(&self, geom: usize) -> [f64; 3] {
        let mut out = [0.0; 3];
        unsafe { sys::mjw_geom_xpos(self.data, geom as i32, out.as_mut_ptr()) };
        out
    }
    pub fn geom_quat(&self, geom: usize) -> [f64; 4] {
        let mut out = [0.0; 4];
        unsafe { sys::mjw_geom_xquat(self.data, geom as i32, out.as_mut_ptr()) };
        out
    }
    pub fn contacts(&self) -> Vec<Contact> {
        let n = unsafe { sys::mjw_ncon(self.data) } as usize;
        (0..n)
            .map(|i| unsafe {
                let (mut g1, mut g2) = (0i32, 0i32);
                let mut pos = [0.0; 3];
                let mut f = 0.0;
                sys::mjw_contact(
                    self.model,
                    self.data,
                    i as i32,
                    &mut g1,
                    &mut g2,
                    pos.as_mut_ptr(),
                    &mut f,
                );
                Contact {
                    geom1: g1 as usize,
                    geom2: g2 as usize,
                    body1: self.geom_body(g1 as usize),
                    body2: self.geom_body(g2 as usize),
                    normal_force: f,
                    pos,
                }
            })
            .collect()
    }
    /// The actuator force in joint space (`qfrc_actuator`, length `nv`):
    /// what the motors put on each dof, after MuJoCo's per-joint
    /// `jnt_actfrcrange` clamp.
    pub fn qfrc_actuator(&self) -> &[f64] {
        unsafe { core::slice::from_raw_parts(sys::mjw_qfrc_actuator(self.data), self.nv) }
    }
    pub fn actuator_force(&self) -> Vec<f64> {
        unsafe { core::slice::from_raw_parts(sys::mjw_actuator_force(self.data), self.nu).to_vec() }
    }
}

impl MjSim {
    /// Whether the state has gone non-finite. The world decides what that
    /// means by its own rules; it never traps.
    pub fn diverged(&self) -> bool {
        self.qpos()
            .iter()
            .chain(self.qvel())
            .any(|value| !value.is_finite())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PENDULUM: &[u8] = include_bytes!("../tests/fixtures/pendulum.mjb");

    #[test]
    fn loads_steps_and_reads_back() {
        let mut sim = MjSim::load(PENDULUM).expect("the fixture loads");
        assert_eq!(sim.body_names(), vec!["world", "arm"]);
        let arm = sim.body_id("arm").unwrap();
        let hinge = sim.joint_id("hinge").unwrap();
        let motor = sim.actuator_id("motor").unwrap();
        assert_eq!(sim.actuator_ctrlrange(motor), Some((-1.0, 1.0)));
        let (qadr, _) = sim.joint_addr(hinge);
        sim.reset(sim.keyframe_id("start"));
        let start = sim.qpos()[qadr];
        sim.set_ctrl(&[1.0]);
        for _ in 0..200 {
            sim.step();
        }
        assert!(sim.qpos()[qadr] != start, "the motor moves the arm");
        assert!(!sim.diverged());
        assert!(sim.body_pos(arm).iter().all(|value| value.is_finite()));
    }

    #[test]
    fn joint_space_actuator_force_and_body_quat() {
        let mut sim = MjSim::load(PENDULUM).unwrap();
        let hinge = sim.joint_id("hinge").unwrap();
        assert_eq!(sim.body_joint(sim.body_id("arm").unwrap()), Some(hinge));
        assert_eq!(sim.body_joint(0), None);
        let (_, dof) = sim.joint_addr(hinge);
        assert_eq!(
            sim.joint_actfrcrange(hinge),
            None,
            "the fixture's hinge has no torque limit"
        );
        sim.set_ctrl(&[0.5]);
        sim.forward();
        // A gear-5 motor at half control puts 2.5 N·m on its hinge.
        assert!((sim.qfrc_actuator()[dof] - 2.5).abs() < 1e-9);
        let arm = sim.body_id("arm").unwrap();
        let half = std::f64::consts::FRAC_1_SQRT_2;
        sim.set_body_quat(arm, [half, 0.0, 0.0, half]);
        sim.forward();
        assert!(sim.body_quat(arm).iter().all(|value| value.is_finite()));
    }

    #[test]
    fn corrupt_models_are_errors_not_traps() {
        assert!(MjSim::load(b"not a model").is_err());
    }

    #[test]
    fn non_finite_state_is_reported() {
        let mut sim = MjSim::load(PENDULUM).unwrap();
        sim.set_qvel(&[f64::NAN]);
        assert!(sim.diverged());
    }
}
