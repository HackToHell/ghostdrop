// Links libghostty's shared library (built by scripts/build-linux.sh).
// FFI bindings are checked into src/ffi.rs so building needs no clang/libclang.
//
// GHOSTTY_DIR points at a libghostty install prefix (include/ghostty.h,
// lib/ghostty-internal.so); defaults to ../build/ghostty-linux.
use std::{env, path::PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let dir = env::var("GHOSTTY_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest.join("../build/ghostty-linux"));
    let lib = dir.join("lib");
    let so = lib.join("ghostty-internal.so");
    println!("cargo:rerun-if-env-changed=GHOSTTY_DIR");
    println!("cargo:rerun-if-changed={}", so.display());

    if !so.exists() {
        panic!(
            "missing {} — run `scripts/build-linux.sh build` (or `make`) from the repo root first",
            so.display()
        );
    }

    // Link the shared libghostty (soname libghostty.so). Unlike the static
    // archive it carries its own libc++/libxml2/freetype/harfbuzz/fontconfig
    // and only needs libc + libm. It's shipped next to the binary ($ORIGIN).
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    // OUT_DIR = target/<profile>/build/.../out → target/<profile>
    let profile_dir = out_dir
        .ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == "build"))
        .and_then(|p| p.parent())
        .expect("locate target profile dir")
        .to_path_buf();
    for dst in [out_dir.join("libghostty.so"), profile_dir.join("libghostty.so")] {
        std::fs::copy(&so, &dst).expect("copy libghostty.so");
    }
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=dylib=ghostty");
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN:$ORIGIN/../lib/ghostdrop");
}
