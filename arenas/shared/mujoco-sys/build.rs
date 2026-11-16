//! Compile the vendored MuJoCo runtime engine (C only), libccd, the
//! single-threaded, no-plugin stubs and the mjw_* accessor shim.
//!
//! - `wasm32-wasip2`: wasi-sdk's clang and llvm-ar, from `WASI_SDK_PATH` or
//!   ARENAS's `.toolchain/wasi-sdk-33` (`task toolchain:wasi-sdk` fetches it).
//! - native: the host C toolchain, for native tests of the kits and engines,
//!   with floating-point contraction off: clang fuses `a*b+c` into one FMA on
//!   aarch64 by default, and wasm (which has no FMA) never does, so without
//!   it a native run computes slightly different physics from the arena that
//!   ships. The wasm build is left exactly as it is: the flag changes most of
//!   its objects, and so every arena's digest.

use std::env;
use std::path::PathBuf;

fn main() {
    let target = env::var("TARGET").expect("cargo sets TARGET");
    let manifest =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let mj = manifest.join("vendor/mujoco");
    let ccd = manifest.join("vendor/libccd/src");

    let mut build = cc::Build::new();
    if target.starts_with("wasm32-wasi") {
        let sdk = env::var("WASI_SDK_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                manifest
                    .ancestors()
                    .nth(2)
                    .expect("shared/mujoco-sys sits two levels under ARENAS")
                    .join(".toolchain/wasi-sdk-33")
            });
        assert!(
            sdk.join("bin/clang").exists(),
            "wasi-sdk not found at {}: run `task toolchain:wasi-sdk` in ARENAS, or set WASI_SDK_PATH",
            sdk.display()
        );
        build
            .compiler(sdk.join("bin/clang"))
            .archiver(sdk.join("bin/llvm-ar"))
            .flag(format!(
                "--sysroot={}",
                sdk.join("share/wasi-sysroot").display()
            ))
            // No path of this machine reaches the engine (MuJoCo's error
            // macros name their source file): the same source gives the
            // same arena wherever it's checked out.
            .flag(format!(
                "-ffile-prefix-map={}=mujoco-sys",
                manifest.display()
            ))
            .flag(format!("-ffile-prefix-map={}=wasi-sdk", sdk.display()));
    }

    build
        .std("c11")
        .opt_level(2)
        .flag("-fvisibility=hidden")
        .define("_GNU_SOURCE", None)
        .define("_POSIX_C_SOURCE", "200809L")
        .define("CCD_STATIC_DEFINE", None)
        .define("MUJOCO_DLL_EXPORTS", None)
        .include(mj.join("include"))
        .include(mj.join("src"))
        .include(&ccd)
        .warnings(false);

    let mut sources = 0;
    for entry in std::fs::read_dir(mj.join("src/engine")).expect("vendored engine sources") {
        let path = entry.expect("readable entry").path();
        if path.extension().is_some_and(|ext| ext == "c") {
            build.file(&path);
            sources += 1;
        }
    }
    assert_eq!(
        sources, 36,
        "expected MuJoCo 3.11.0's 36 engine .c files in vendor/mujoco"
    );

    for file in ["ccd", "mpr", "polytope", "support", "vec3"] {
        build.file(ccd.join(format!("{file}.c")));
    }
    if !target.starts_with("wasm32") {
        build.flag_if_supported("-ffp-contract=off");
    }
    build.file(manifest.join("csrc/wasi_stubs.c"));
    build.file(manifest.join("csrc/shim.c"));
    build.compile("mujoco_engine");

    println!("cargo:rerun-if-changed=csrc");
    println!("cargo:rerun-if-changed=vendor");
    println!("cargo:rerun-if-env-changed=WASI_SDK_PATH");
}
