// Generates Rust bindings for libghostty's embedding API and links the static library.
//
// GHOSTTY_DIR points at a libghostty install prefix (include/ghostty.h,
// lib/libghostty.a); defaults to ../build/ghostty-linux.
use std::{env, path::PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let dir = env::var("GHOSTTY_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest.join("../build/ghostty-linux"));
    let header = dir.join("include/ghostty.h");
    let lib = dir.join("lib");
    println!("cargo:rerun-if-env-changed=GHOSTTY_DIR");
    println!("cargo:rerun-if-changed={}", header.display());
    println!("cargo:rerun-if-changed={}", lib.join("libghostty.a").display());

    let bindings = bindgen::Builder::default()
        .header(header.to_str().unwrap())
        .clang_arg("-DGHOSTTY_STATIC")
        .allowlist_function("ghostty_.*")
        .allowlist_type("ghostty_.*")
        .allowlist_var("GHOSTTY_.*")
        .prepend_enum_name(false)
        .default_enum_style(bindgen::EnumVariation::Consts)
        .derive_default(true)
        .layout_tests(false)
        .generate()
        .expect("bindgen failed on ghostty.h");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("ghostty.rs");
    bindings.write_to_file(out).unwrap();

    // Link the shared libghostty (soname libghostty.so). Unlike the static
    // archive it carries its own libc++/libxml2/freetype/harfbuzz/fontconfig
    // and only needs libc + libm. It's shipped next to the binary ($ORIGIN).
    let so = lib.join("ghostty-internal.so");
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
    println!("cargo:rerun-if-changed={}", so.display());
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=dylib=ghostty");
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN:$ORIGIN/../lib/ghostdrop");
}
