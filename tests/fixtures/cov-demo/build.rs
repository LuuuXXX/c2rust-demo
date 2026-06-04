// build.rs – mirrors what `c2rust-demo init --cov` produces:
//   1. Compile each C source file with clang's LLVM coverage flags.
//   2. Pack the instrumented objects into libcov.a.
//   3. Tell Cargo to link libcov.a into the test binary.
//
// When `cargo llvm-cov` runs the tests, both the Rust code and the
// linked C code appear in the coverage report.

use std::path::Path;
use std::process::Command;

fn main() {
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let src_dir = Path::new(&manifest_dir).join("src");

    let c_files = ["math.c", "counter.c"];
    let mut obj_paths: Vec<String> = Vec::new();

    for c_file in c_files {
        let c_path = src_dir.join(c_file);
        let obj_name = c_file.replace(".c", ".o");
        let obj_path = Path::new(&out_dir).join(&obj_name);

        // Compile with LLVM source-based coverage instrumentation, matching
        // what the c2rust-demo hook does when C2RUST_COV=1 is set.
        let status = Command::new("clang")
            .args(["-fprofile-instr-generate", "-fcoverage-mapping", "-c"])
            .arg(&c_path)
            .arg("-o")
            .arg(&obj_path)
            .status()
            .expect("failed to run clang; make sure clang is installed");

        assert!(status.success(), "clang failed to compile {c_file}");
        obj_paths.push(obj_path.to_str().unwrap().to_string());
        println!("cargo:rerun-if-changed=src/{c_file}");
    }

    // Pack instrumented objects into libcov.a (same layout as c2rust-demo init).
    let lib_path = Path::new(&out_dir).join("libcov.a");
    let mut ar = Command::new("ar");
    ar.arg("rcs").arg(&lib_path);
    for obj in &obj_paths {
        ar.arg(obj);
    }
    assert!(ar.status().expect("failed to run ar").success(), "ar failed");

    println!("cargo:rustc-link-search=native={out_dir}");
    println!("cargo:rustc-link-lib=static=cov");
}
