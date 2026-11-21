//! Compiles the C++ helpers shared with the generated code (fres/frsqrte, ADR 0006).

fn main() {
    let runtime = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime");
    let compiler = std::env::var("CXX").unwrap_or_else(|_| "clang++".into());
    cc::Build::new()
        .cpp(true)
        .compiler(compiler)
        .std("c++20")
        .include(runtime.join("include"))
        .file(runtime.join("src/fp.cpp"))
        .warnings(false)
        .compile("fepor_fp");
    println!("cargo:rerun-if-changed=../../runtime/src/fp.cpp");
    println!("cargo:rerun-if-changed=../../runtime/include/fepor/ppc.h");
}
