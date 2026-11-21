//! Links the generated game library (development C++ backend).
//!
//! `FEPOR_GENERATED_DIR` is the build directory produced by `fepor-recomp prepare`
//! (it holds libfepor_generated.a). Without it the binary builds but cannot link, so cargo
//! reports a clear error instead of a wall of undefined symbols.

fn main() {
    println!("cargo:rerun-if-env-changed=FEPOR_GENERATED_DIR");
    let Ok(dir) = std::env::var("FEPOR_GENERATED_DIR") else {
        println!("cargo:warning=FEPOR_GENERATED_DIR is not set: run `tools/dev-run.sh <iso>` or set it to the build dir of `fepor-recomp prepare`");
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let lib = dir.join("libfepor_generated.a");
    println!("cargo:rerun-if-changed={}", lib.display());
    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-link-lib=static=fepor_generated");
    println!("cargo:rustc-link-lib=stdc++");
}
