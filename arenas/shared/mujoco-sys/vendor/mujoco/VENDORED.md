# Vendored: MuJoCo 3.11.0 — runtime engine only

Upstream: https://github.com/google-deepmind/mujoco, tag `3.11.0`
(commit b85fdca, Apache-2.0 — see `LICENSE`).

What is here, verbatim and unpatched:

- `include/mujoco/` — the public headers.
- `src/engine/` — the runtime engine: every `.c`/`.h` in that directory.

What is NOT here: the model compiler (`src/user`, `src/xml`), renderer
(`src/render`), UI, plugins, and the three C++ files in `src/engine`
(`engine_thread.cc`, `engine_plugin.cc`, `engine_crossplatform.cc`), whose
single-threaded / no-plugin replacements live in
`shared/mujoco-sys/csrc/wasi_stubs.c`. Models are compiled to `.mjb` at bake time with the real MuJoCo (the authoring tools'
`bake-model`, Python `mujoco==3.11.0`) and loaded here with
`mj_loadModelBuffer`: pure C, no VFS, no XML.

Why vendored: the engine is compiled into each arena's WASM component for
wasm32-wasip2, so the physics is part of the published arena, covered by its
digest, and a MuJoCo error traps the guest rather than the host. Update by
copying the same two directories from the new tag, bumping the bake's pinned
wheel to the same version, and re-running the suite; keep this file's tag line
honest. A `.mjb` is locked to its MuJoCo version, so moving the pin means a
new arena version for every arena.
