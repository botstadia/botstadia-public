//! Raw FFI over the vendored MuJoCo engine's `mjw_*` accessor shim
//! (`csrc/shim.c`). `MjModelPtr`/`MjDataPtr` are opaque; every field access
//! goes through a shim call the C compiler checked against the real headers.
//! The safe API is the `mujoco` crate beside this one.

#![allow(non_camel_case_types)]

use core::ffi::{c_char, c_double, c_int, c_uchar};

/// Opaque `mjModel`.
#[repr(C)]
pub struct mjw_model {
    _private: [u8; 0],
}

/// Opaque `mjData`.
#[repr(C)]
pub struct mjw_data {
    _private: [u8; 0],
}

unsafe extern "C" {
    pub fn mjw_nq(m: *const mjw_model) -> c_int;
    pub fn mjw_nv(m: *const mjw_model) -> c_int;
    pub fn mjw_nu(m: *const mjw_model) -> c_int;
    pub fn mjw_nbody(m: *const mjw_model) -> c_int;
    pub fn mjw_ngeom(m: *const mjw_model) -> c_int;
    pub fn mjw_nsite(m: *const mjw_model) -> c_int;
    pub fn mjw_njnt(m: *const mjw_model) -> c_int;
    pub fn mjw_nkey(m: *const mjw_model) -> c_int;
    pub fn mjw_timestep(m: *const mjw_model) -> c_double;

    pub fn mjw_name2id(m: *const mjw_model, type_: c_int, name: *const c_char) -> c_int;
    pub fn mjw_id2name(m: *const mjw_model, type_: c_int, id: c_int) -> *const c_char;

    pub fn mjw_body_jntadr(m: *const mjw_model, body: c_int) -> c_int;
    pub fn mjw_body_jntnum(m: *const mjw_model, body: c_int) -> c_int;
    pub fn mjw_body_mass(m: *const mjw_model, body: c_int) -> c_double;
    pub fn mjw_set_body_mass(m: *mut mjw_model, body: c_int, mass: c_double);
    pub fn mjw_set_body_pos(m: *mut mjw_model, body: c_int, pos: *const c_double);
    pub fn mjw_set_body_quat(m: *mut mjw_model, body: c_int, quat: *const c_double);
    pub fn mjw_set_site_pos(m: *mut mjw_model, site: c_int, pos: *const c_double);
    pub fn mjw_set_geom_friction(m: *mut mjw_model, geom: c_int, friction: *const c_double);
    pub fn mjw_geom_bodyid(m: *const mjw_model, geom: c_int) -> c_int;
    pub fn mjw_body_parentid(m: *const mjw_model, body: c_int) -> c_int;
    pub fn mjw_geom_contype(m: *const mjw_model, geom: c_int) -> c_int;
    pub fn mjw_geom_conaffinity(m: *const mjw_model, geom: c_int) -> c_int;
    pub fn mjw_geom_distance(
        m: *const mjw_model,
        d: *mut mjw_data,
        geom1: c_int,
        geom2: c_int,
        distmax: c_double,
    ) -> c_double;

    pub fn mjw_jnt_type(m: *const mjw_model, jnt: c_int) -> c_int;
    pub fn mjw_jnt_qposadr(m: *const mjw_model, jnt: c_int) -> c_int;
    pub fn mjw_jnt_dofadr(m: *const mjw_model, jnt: c_int) -> c_int;
    pub fn mjw_jnt_limited(m: *const mjw_model, jnt: c_int) -> c_int;
    pub fn mjw_jnt_range(m: *const mjw_model, jnt: c_int, out: *mut c_double);
    pub fn mjw_jnt_actfrclimited(m: *const mjw_model, jnt: c_int) -> c_int;
    pub fn mjw_jnt_actfrcrange(m: *const mjw_model, jnt: c_int, out: *mut c_double);
    pub fn mjw_set_jnt_actfrcrange(m: *mut mjw_model, jnt: c_int, range: *const c_double);
    pub fn mjw_actuator_ctrllimited(m: *const mjw_model, act: c_int) -> c_int;
    pub fn mjw_actuator_ctrlrange(m: *const mjw_model, act: c_int, out: *mut c_double);
    pub fn mjw_actuator_trnid(m: *const mjw_model, act: c_int) -> c_int;

    pub fn mjw_time(d: *const mjw_data) -> c_double;
    pub fn mjw_qpos(d: *mut mjw_data) -> *mut c_double;
    pub fn mjw_qvel(d: *mut mjw_data) -> *mut c_double;
    pub fn mjw_ctrl(d: *mut mjw_data) -> *mut c_double;
    pub fn mjw_xfrc_applied(d: *mut mjw_data) -> *mut c_double;
    pub fn mjw_actuator_force(d: *mut mjw_data) -> *mut c_double;
    pub fn mjw_qfrc_actuator(d: *mut mjw_data) -> *mut c_double;

    pub fn mjw_body_xpos(d: *const mjw_data, body: c_int, out: *mut c_double);
    pub fn mjw_body_xquat(d: *const mjw_data, body: c_int, out: *mut c_double);
    pub fn mjw_site_xpos(d: *const mjw_data, site: c_int, out: *mut c_double);
    pub fn mjw_geom_xpos(d: *const mjw_data, geom: c_int, out: *mut c_double);
    pub fn mjw_geom_xquat(d: *const mjw_data, geom: c_int, out: *mut c_double);
    pub fn mjw_body_velocity(
        m: *const mjw_model,
        d: *const mjw_data,
        body: c_int,
        out: *mut c_double,
    );

    pub fn mjw_ncon(d: *const mjw_data) -> c_int;
    pub fn mjw_contact(
        m: *const mjw_model,
        d: *const mjw_data,
        i: c_int,
        geom1: *mut c_int,
        geom2: *mut c_int,
        pos: *mut c_double,
        normal_force: *mut c_double,
    );

    pub fn mjw_load_model(buffer: *const c_uchar, len: c_int) -> *const mjw_model;
    pub fn mjw_delete_model(m: *const mjw_model);
    pub fn mjw_make_data(m: *const mjw_model) -> *mut mjw_data;
    pub fn mjw_delete_data(d: *mut mjw_data);
    pub fn mjw_step(m: *const mjw_model, d: *mut mjw_data);
    pub fn mjw_forward(m: *const mjw_model, d: *mut mjw_data);
    pub fn mjw_set_const(m: *mut mjw_model, d: *mut mjw_data);
    pub fn mjw_reset_data(m: *const mjw_model, d: *mut mjw_data);
    pub fn mjw_reset_data_keyframe(m: *const mjw_model, d: *mut mjw_data, key: c_int);

    pub fn mjw_obj_body() -> c_int;
    pub fn mjw_obj_geom() -> c_int;
    pub fn mjw_obj_site() -> c_int;
    pub fn mjw_obj_joint() -> c_int;
    pub fn mjw_obj_actuator() -> c_int;
    pub fn mjw_obj_key() -> c_int;
    pub fn mjw_jnt_free() -> c_int;
}
