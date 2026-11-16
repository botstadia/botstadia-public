// C replacements for the three C++ files in src/engine when building MuJoCo's
// runtime engine for wasm32-wasip2 (single-threaded, no plugins):
//   - engine_thread.cc        -> mju_threadpool / mju_dispatch / mju_numThread (serial)
//   - engine_plugin.cc        -> empty plugin table (engine_io.c queries it when m->nplugin > 0)
//   - engine_crossplatform.cc -> nothing needed (only has Apple/AVX Rosetta check + ASAN helpers)
#include <stddef.h>
#include <mujoco/mujoco.h>
#include "engine/engine_thread.h"
#include "engine/engine_plugin.h"

// ---- engine_thread.cc replacements -------------------------------------------------------------
void mju_threadpool(mjData* d, int nthread) {
  (void)nthread;
  d->threadpool = 0;  // never create a pool: wasm32-wasip2 is single-threaded
}

int mju_numThread(const mjData* d) {
  (void)d;
  return 1;
}

void mju_dispatch(const mjModel* m, mjData* d, mjTaskFunc func, void* arg, int ntask) {
  for (int i = 0; i < ntask; i++) {
    func(m, d, arg, 0, i);
  }
}

// ---- engine_plugin.cc replacements (no plugins registered) --------------------------------------
int mjp_pluginCount(void) { return 0; }
const mjpPlugin* mjp_getPluginAtSlot(int slot) { (void)slot; return NULL; }
const mjpPlugin* mjp_getPluginAtSlotUnsafe(int slot, int nslot) { (void)slot; (void)nslot; return NULL; }
const mjpPlugin* mjp_getPlugin(const char* name, int* slot) { (void)name; if (slot) *slot = -1; return NULL; }
int mjp_registerPlugin(const mjpPlugin* plugin) { (void)plugin; return -1; }
void mjp_defaultPlugin(mjpPlugin* plugin) { if (plugin) { char* p = (char*)plugin; for (size_t i = 0; i < sizeof(*plugin); i++) p[i] = 0; } }
int mjp_resourceProviderCount(void) { return 0; }
const mjpResourceProvider* mjp_getResourceProvider(const char* name) { (void)name; return NULL; }
const mjpResourceProvider* mjp_getResourceProviderAtSlot(int slot) { (void)slot; return NULL; }
int mjp_registerResourceProvider(const mjpResourceProvider* provider) { (void)provider; return -1; }
void mj_loadPluginLibrary(const char* path) { (void)path; }
void mj_loadAllPluginLibraries(const char* directory, mjfPluginLibraryLoadCallback callback) { (void)directory; (void)callback; }

// ---- user_resource.cc replacement: mj_saveModel(filename=...) needs this; we only support the
// in-memory buffer path (filename == NULL), so writing to a named resource reports failure.
mjtSize mju_writeResource(const char* name, const void* buffer, mjtSize nbytes,
                          const mjVFS* vfs, char* error, size_t error_sz) {
  (void)name; (void)buffer; (void)nbytes; (void)vfs;
  if (error && error_sz) { error[0] = 0; }
  return 0;
}
