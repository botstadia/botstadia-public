# Vendored: libccd v2.1 (+pinned fix)

Upstream: https://github.com/danfis/libccd, commit `7931e76`
("Fixed division by zero in ccdMPRPenetration", the commit MuJoCo 3.11.0's
CMake pins). BSD 3-clause — see `LICENSE`.

What is here: `src/` — the five translation units MuJoCo's convex collision
needs (`ccd.c mpr.c polytope.c support.c vec3.c`) and their headers.
`src/ccd/config.h` is the generated configuration header (`CCD_DOUBLE`:
MuJoCo builds libccd in double precision).
