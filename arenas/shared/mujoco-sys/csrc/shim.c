// Accessor shim so the Rust side never needs bindgen over mjModel/mjData —
// two enormous structs whose layout varies with MuJoCo's build options. Every
// field the `mujoco` wrapper reads or writes crosses through one of
// these tiny functions instead, which the C compiler checks against the real
// headers. Pure C, no allocation beyond what MuJoCo itself does.
#include <mujoco/mujoco.h>
#include <string.h>

// ── Model facts ─────────────────────────────────────────────────────────
int mjw_nq(const mjModel* m) { return m->nq; }
int mjw_nv(const mjModel* m) { return m->nv; }
int mjw_nu(const mjModel* m) { return m->nu; }
int mjw_nbody(const mjModel* m) { return m->nbody; }
int mjw_ngeom(const mjModel* m) { return m->ngeom; }
int mjw_nsite(const mjModel* m) { return m->nsite; }
int mjw_njnt(const mjModel* m) { return m->njnt; }
int mjw_nkey(const mjModel* m) { return m->nkey; }
double mjw_timestep(const mjModel* m) { return m->opt.timestep; }

int mjw_name2id(const mjModel* m, int type, const char* name) {
  return mj_name2id(m, type, name);
}
const char* mjw_id2name(const mjModel* m, int type, int id) {
  return mj_id2name(m, type, id);
}

int mjw_body_jntadr(const mjModel* m, int body) { return m->body_jntadr[body]; }
int mjw_body_jntnum(const mjModel* m, int body) { return m->body_jntnum[body]; }
double mjw_body_mass(const mjModel* m, int body) { return m->body_mass[body]; }
void mjw_set_body_mass(mjModel* m, int body, double mass) { m->body_mass[body] = mass; }
void mjw_set_body_pos(mjModel* m, int body, const double pos[3]) {
  memcpy(m->body_pos + 3 * body, pos, 3 * sizeof(double));
}
void mjw_set_body_quat(mjModel* m, int body, const double quat[4]) {
  memcpy(m->body_quat + 4 * body, quat, 4 * sizeof(double));
}
void mjw_set_site_pos(mjModel* m, int site, const double pos[3]) {
  memcpy(m->site_pos + 3 * site, pos, 3 * sizeof(double));
}
void mjw_set_geom_friction(mjModel* m, int geom, const double friction[3]) {
  memcpy(m->geom_friction + 3 * geom, friction, 3 * sizeof(double));
}
int mjw_geom_bodyid(const mjModel* m, int geom) { return m->geom_bodyid[geom]; }
int mjw_body_parentid(const mjModel* m, int body) { return m->body_parentid[body]; }
int mjw_geom_contype(const mjModel* m, int geom) { return m->geom_contype[geom]; }
int mjw_geom_conaffinity(const mjModel* m, int geom) { return m->geom_conaffinity[geom]; }

int mjw_jnt_type(const mjModel* m, int jnt) { return m->jnt_type[jnt]; }
int mjw_jnt_qposadr(const mjModel* m, int jnt) { return m->jnt_qposadr[jnt]; }
int mjw_jnt_dofadr(const mjModel* m, int jnt) { return m->jnt_dofadr[jnt]; }
int mjw_jnt_limited(const mjModel* m, int jnt) { return m->jnt_limited[jnt]; }
void mjw_jnt_range(const mjModel* m, int jnt, double out[2]) {
  out[0] = m->jnt_range[2 * jnt];
  out[1] = m->jnt_range[2 * jnt + 1];
}
int mjw_jnt_actfrclimited(const mjModel* m, int jnt) { return m->jnt_actfrclimited[jnt]; }
void mjw_jnt_actfrcrange(const mjModel* m, int jnt, double out[2]) {
  out[0] = m->jnt_actfrcrange[2 * jnt];
  out[1] = m->jnt_actfrcrange[2 * jnt + 1];
}
void mjw_set_jnt_actfrcrange(mjModel* m, int jnt, const double range[2]) {
  m->jnt_actfrcrange[2 * jnt] = range[0];
  m->jnt_actfrcrange[2 * jnt + 1] = range[1];
}
int mjw_actuator_ctrllimited(const mjModel* m, int act) { return m->actuator_ctrllimited[act]; }
void mjw_actuator_ctrlrange(const mjModel* m, int act, double out[2]) {
  out[0] = m->actuator_ctrlrange[2 * act];
  out[1] = m->actuator_ctrlrange[2 * act + 1];
}
int mjw_actuator_trnid(const mjModel* m, int act) { return m->actuator_trnid[2 * act]; }

// ── Data state ──────────────────────────────────────────────────────────
double mjw_time(const mjData* d) { return d->time; }
double* mjw_qpos(mjData* d) { return d->qpos; }
double* mjw_qvel(mjData* d) { return d->qvel; }
double* mjw_ctrl(mjData* d) { return d->ctrl; }
double* mjw_xfrc_applied(mjData* d) { return d->xfrc_applied; }
double* mjw_actuator_force(mjData* d) { return d->actuator_force; }
double* mjw_qfrc_actuator(mjData* d) { return d->qfrc_actuator; }

void mjw_body_xpos(const mjData* d, int body, double out[3]) {
  memcpy(out, d->xpos + 3 * body, 3 * sizeof(double));
}
void mjw_body_xquat(const mjData* d, int body, double out[4]) {
  memcpy(out, d->xquat + 4 * body, 4 * sizeof(double));
}
void mjw_site_xpos(const mjData* d, int site, double out[3]) {
  memcpy(out, d->site_xpos + 3 * site, 3 * sizeof(double));
}
void mjw_geom_xpos(const mjData* d, int geom, double out[3]) {
  memcpy(out, d->geom_xpos + 3 * geom, 3 * sizeof(double));
}
// geom orientation is stored as a 9-element rotation matrix; convert.
void mjw_geom_xquat(const mjData* d, int geom, double out[4]) {
  mju_mat2Quat(out, d->geom_xmat + 9 * geom);
}
// World-frame velocity of a body frame: out[0..3] = angular, out[3..6] = linear.
void mjw_body_velocity(const mjModel* m, const mjData* d, int body, double out[6]) {
  mj_objectVelocity(m, d, mjOBJ_BODY, body, out, 0);
}

// The signed distance between two geoms (negative when they overlap), up to
// distmax; the current poses, so call after forward.
double mjw_geom_distance(const mjModel* m, mjData* d, int geom1, int geom2, double distmax) {
  mjtNum fromto[6];
  return mj_geomDistance(m, d, geom1, geom2, distmax, fromto);
}

// ── Contacts ────────────────────────────────────────────────────────────
int mjw_ncon(const mjData* d) { return d->ncon; }
void mjw_contact(const mjModel* m, const mjData* d, int i, int* geom1, int* geom2,
                 double pos[3], double* normal_force) {
  const mjContact* c = d->contact + i;
  *geom1 = c->geom[0];
  *geom2 = c->geom[1];
  memcpy(pos, c->pos, 3 * sizeof(double));
  // Contact force in the contact frame; [0] is the normal component.
  double f[6] = {0, 0, 0, 0, 0, 0};
  if (c->efc_address >= 0) {
    mj_contactForce(m, d, i, f);
  }
  *normal_force = f[0];
}

// ── Lifecycle / stepping ────────────────────────────────────────────────
const mjModel* mjw_load_model(const unsigned char* buffer, int len) {
  return mj_loadModelBuffer(buffer, len);
}
void mjw_delete_model(const mjModel* m) { mj_deleteModel((mjModel*)m); }
mjData* mjw_make_data(const mjModel* m) { return mj_makeData(m); }
void mjw_delete_data(mjData* d) { mj_deleteData(d); }
void mjw_step(const mjModel* m, mjData* d) { mj_step(m, d); }
void mjw_forward(const mjModel* m, mjData* d) { mj_forward(m, d); }
void mjw_set_const(mjModel* m, mjData* d) { mj_setConst(m, d); }
void mjw_reset_data(const mjModel* m, mjData* d) { mj_resetData(m, d); }
void mjw_reset_data_keyframe(const mjModel* m, mjData* d, int key) {
  mj_resetDataKeyframe(m, d, key);
}

// Object-type constants, exported as functions so the Rust side cannot
// drift from the header's enum.
int mjw_obj_body(void) { return mjOBJ_BODY; }
int mjw_obj_geom(void) { return mjOBJ_GEOM; }
int mjw_obj_site(void) { return mjOBJ_SITE; }
int mjw_obj_joint(void) { return mjOBJ_JOINT; }
int mjw_obj_actuator(void) { return mjOBJ_ACTUATOR; }
int mjw_obj_key(void) { return mjOBJ_KEY; }
int mjw_jnt_free(void) { return mjJNT_FREE; }
