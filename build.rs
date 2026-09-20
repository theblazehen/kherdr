#[cfg(feature = "terminal")]
use std::{env, path::PathBuf};

#[cfg(not(feature = "terminal"))]
fn main() {}

#[cfg(feature = "terminal")]
fn main() {
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_TERMINAL");
    if env::var_os("CARGO_FEATURE_TERMINAL").is_none() {
        return;
    }
    println!("cargo:rerun-if-env-changed=GHOSTTY_SOURCE");
    println!("cargo:rerun-if-env-changed=GHOSTTY_PREFIX");
    let source = PathBuf::from(env::var_os("GHOSTTY_SOURCE")
        .expect("terminal feature requires GHOSTTY_SOURCE (the pinned Ghostty source tree)"));
    let prefix = PathBuf::from(env::var_os("GHOSTTY_PREFIX")
        .expect("terminal feature requires GHOSTTY_PREFIX (the target Ghostty installation)"));
    let include = source.join("include");
    let header = include.join("ghostty/vt.h");
    let lib = prefix.join("lib");
    assert!(header.is_file(), "missing pinned header: {}", header.display());
    assert!(lib.join("libghostty-vt.a").is_file(), "missing target libghostty-vt.a in {}", lib.display());

    // Bind the headers belonging to the actual archive build, never host-installed
    // Ghostty headers or hand-maintained struct/enum layouts. Cross builds supply
    // their sysroot through bindgen's BINDGEN_EXTRA_CLANG_ARGS[_<target>].
    let bindings = bindgen::Builder::default()
        .header(header.to_string_lossy())
        .clang_arg(format!("-I{}", include.display()))
        .clang_arg("-DGHOSTTY_STATIC")
        .clang_arg("-std=c11")
        .allowlist_function("ghostty_(terminal_(new|free|reset|resize|set|get|vt_write)|render_state_.*|cell_get|kitty_graphics_.*|sys_set|alloc|free|build_info)")
        .allowlist_type("Ghostty.*")
        .allowlist_var("GHOSTTY_.*")
        .prepend_enum_name(false)
        .layout_tests(false)
        .generate_comments(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("generating bindings from pinned Ghostty headers failed");
    bindings.write_to_file(PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("ghostty_vt.rs"))
        .expect("writing Ghostty bindings failed");
    println!("cargo:rerun-if-changed={}", lib.join("libghostty-vt.a").display());
    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-lib=static=ghostty-vt");
    // The pinned Linux archive's native link contract (also used by vt-probe).
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        for library in ["m", "dl", "pthread", "rt"] {
            println!("cargo:rustc-link-lib={library}");
        }
    }
}
