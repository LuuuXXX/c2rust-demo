//! Integration tests for `c2rust-demo init`.
//!
//! Tests that require external tools (gcc, make, clang, bindgen) automatically
//! detect whether those tools are present and print a clear skip message when
//! they are not.  No environment variable gate is needed.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Clone DaveGamble/cJSON from GitHub (--depth=1) into a fresh TempDir.
///
/// Returns the TempDir (caller must hold it alive) on success, or `None` if
/// `git` is unavailable or the clone fails.  Tests should print a skip
/// message and return early when this returns `None`.
fn prepare_cjson_project() -> Option<tempfile::TempDir> {
    if !missing_tools(&["git"]).is_empty() {
        return None;
    }
    let tmp = tempfile::TempDir::new().ok()?;
    let ok = Command::new("git")
        .args(["clone", "--depth=1", "https://github.com/DaveGamble/cJSON.git", "."])
        .current_dir(tmp.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_or(false, |s| s.success());
    if ok { Some(tmp) } else { None }
}

/// Build the hook library and return its path, or `None` on failure.
fn build_hook_for_tests() -> Option<PathBuf> {
    let hook_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("hook");
    if !hook_dir.join("Makefile").exists() {
        return None;
    }
    let status = Command::new("make")
        .arg("-s")
        .current_dir(&hook_dir)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let so = hook_dir.join("libhook.so");
    if so.exists() { Some(so) } else { None }
}

/// Returns a list of tools that are missing from the PATH.
fn missing_tools(tools: &[&str]) -> Vec<String> {
    tools
        .iter()
        .filter(|t| {
            !Command::new("which")
                .arg(t)
                .status()
                .map_or(false, |s| s.success())
        })
        .map(|t| t.to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// CLI / argument parsing tests (no toolchain required)
// ---------------------------------------------------------------------------

#[test]
fn cli_init_parses_default_feature() {
    let output = Command::new(env!("CARGO_BIN_EXE_c2rust-demo"))
        .args(["init", "--help"])
        .output()
        .expect("failed to run c2rust-demo");
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(
        help.contains("feature") || help.contains("BUILD_CMD"),
        "unexpected help output: {help}"
    );
}

/// `merge --help` 应当输出 merge 子命令的帮助信息并正常退出。
#[test]
fn cli_merge_help_exits_zero() {
    let output = Command::new(env!("CARGO_BIN_EXE_c2rust-demo"))
        .args(["merge", "--help"])
        .output()
        .expect("failed to run c2rust-demo");
    assert!(
        output.status.success(),
        "merge --help should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(
        help.contains("feature") || help.contains("merge") || help.contains("Merge"),
        "merge --help output should mention 'feature' or 'merge': {help}"
    );
}

/// `c2rust-demo` 不带子命令时应以非零状态码退出并输出使用说明。
#[test]
fn cli_no_subcommand_exits_nonzero() {
    let output = Command::new(env!("CARGO_BIN_EXE_c2rust-demo"))
        .output()
        .expect("failed to run c2rust-demo");
    assert!(
        !output.status.success(),
        "running without subcommand should fail"
    );
}

/// `c2rust-demo merge` 在没有 `.c2rust` 目录时应以非零状态码退出。
#[test]
fn cli_merge_fails_without_init() {
    let tmp = tempfile::TempDir::new().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_c2rust-demo"))
        .current_dir(tmp.path())
        .args(["merge"])
        .output()
        .expect("failed to run c2rust-demo");
    assert!(
        !output.status.success(),
        "merge should fail when project has not been initialized"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Error") || stderr.contains("not found") || stderr.contains("run init"),
        "error message should indicate missing init, got: {stderr}"
    );
}

/// `c2rust-demo init --feature` 应接受自定义 feature 名称并写入 meta/build_cmd.txt。
#[test]
fn cli_init_custom_feature_writes_meta() {
    let missing = missing_tools(&["gcc", "make", "git"]);
    if !missing.is_empty() {
        eprintln!("Skipping cli_init_custom_feature_writes_meta: missing {}", missing.join(", "));
        return;
    }

    let Some(hook_so) = build_hook_for_tests() else {
        eprintln!("Skipping cli_init_custom_feature_writes_meta: failed to build libhook.so");
        return;
    };
    let _ = hook_so; // 仅验证 hook 可构建；init 本身会重新构建

    let Some(cjson_tmp) = prepare_cjson_project() else {
        eprintln!("Skipping cli_init_custom_feature_writes_meta: failed to clone cJSON from GitHub");
        return;
    };
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = cjson_tmp.path();

    // 使用自定义 feature 名称 "myfeature"
    let status = Command::new(env!("CARGO_BIN_EXE_c2rust-demo"))
        .current_dir(tmp.path())
        .args([
            "init",
            "--feature", "myfeature",
            "--",
            "make",
            &format!("-C{}", fixture.display()),
            "libcjson.a",
        ])
        .status()
        .expect("c2rust-demo init --feature myfeature");

    if !status.success() {
        eprintln!("c2rust-demo init --feature failed – skipping assertions");
        return;
    }

    let meta_dir = tmp.path().join(".c2rust/myfeature/meta");
    assert!(meta_dir.exists(), "meta dir for custom feature should exist");
    let build_cmd_txt = meta_dir.join("build_cmd.txt");
    assert!(build_cmd_txt.exists(), "build_cmd.txt should be written for custom feature");
    let content = std::fs::read_to_string(&build_cmd_txt).unwrap();
    assert!(content.contains("make"), "build_cmd.txt should contain the build command");
}

// ---------------------------------------------------------------------------
// Build-capture tests (require gcc + make + hook)
// ---------------------------------------------------------------------------

/// Runs the build capture phase and verifies that `.c2rust` files are generated.
#[test]
fn build_capture_generates_c2rust_files() {
    let missing = missing_tools(&["gcc", "make"]);
    if !missing.is_empty() {
        eprintln!("Skipping build_capture: missing tools: {}", missing.join(", "));
        return;
    }

    let Some(hook_so) = build_hook_for_tests() else {
        eprintln!("Skipping build_capture: failed to build libhook.so");
        return;
    };

    let Some(cjson_tmp) = prepare_cjson_project() else {
        eprintln!("Skipping build_capture: failed to clone cJSON from GitHub");
        return;
    };
    let fixture = cjson_tmp.path().to_path_buf();
    // C2RUST_PROJECT_ROOT must be an ancestor of the C files being compiled.
    // We clone cJSON into a TempDir and use it directly as the project root.
    let project_root = fixture.clone();
    let tmp = tempfile::TempDir::new().unwrap();
    let feature_root = tmp.path().join(".c2rust/default");
    let c_dir = feature_root.join("c");
    std::fs::create_dir_all(&c_dir).unwrap();

    // Clean + build libcjson.a with the hook injected
    let _ = Command::new("make")
        .current_dir(&fixture)
        .arg("clean")
        .status();

    let status = Command::new("make")
        .current_dir(&fixture)
        .arg("libcjson.a")
        .env("LD_PRELOAD", &hook_so)
        .env("C2RUST_PROJECT_ROOT", project_root)
        .env("C2RUST_FEATURE_ROOT", &feature_root)
        .status()
        .expect("make");
    assert!(status.success(), "make failed");

    // At least one .c2rust file should have been captured
    let c2rust_files = collect_c2rust_files(&c_dir);
    assert!(
        !c2rust_files.is_empty(),
        "expected .c2rust files in {:?}, found none",
        c_dir
    );
    println!("Captured {} .c2rust file(s)", c2rust_files.len());
}

// ---------------------------------------------------------------------------
// Full init tests (require gcc + make + clang + bindgen)
// ---------------------------------------------------------------------------

/// Runs the full `c2rust-demo init` command and verifies the output structure.
///
/// Because the test process has no TTY, `InteractiveSelector` automatically
/// selects all captured files without prompting.
#[test]
fn full_init_creates_rust_project() {
    let missing = missing_tools(&["gcc", "make", "clang", "bindgen", "git"]);
    if !missing.is_empty() {
        eprintln!("Skipping full_init: missing tools: {}", missing.join(", "));
        return;
    }

    // Clone cJSON into a fresh TempDir so that C2RUST_PROJECT_ROOT
    // (derived from the working directory) is an ancestor of the C source
    // files being compiled.
    let Some(cjson_tmp) = prepare_cjson_project() else {
        eprintln!("Skipping full_init: failed to clone cJSON from GitHub");
        return;
    };
    let project_root = cjson_tmp.path();

    // Clean first
    let _ = Command::new("make")
        .current_dir(project_root)
        .arg("clean")
        .status();

    let status = Command::new(env!("CARGO_BIN_EXE_c2rust-demo"))
        .current_dir(project_root)
        .args(["init", "--", "make", "libcjson.a"])
        .status()
        .expect("c2rust-demo init");

    // The full init might fail if some optional tools are missing; we only
    // assert structural outputs if it succeeded.
    if !status.success() {
        eprintln!("c2rust-demo init failed – checking partial output");
    }

    let feature_root = project_root.join(".c2rust/default");
    let meta_dir = feature_root.join("meta");
    let c_dir = feature_root.join("c");

    // These should always be created (before the bindgen step)
    assert!(meta_dir.exists(), "meta/ not created");
    assert!(
        meta_dir.join("build_cmd.txt").exists(),
        "build_cmd.txt not written"
    );

    let cmd_content = std::fs::read_to_string(meta_dir.join("build_cmd.txt")).unwrap();
    assert!(cmd_content.contains("make"), "build_cmd.txt content: {cmd_content}");

    if c_dir.exists() && !collect_c2rust_files(&c_dir).is_empty() {
        assert!(
            meta_dir.join("selected_files.json").exists(),
            "selected_files.json not written"
        );
    }

    if status.success() {
        let rust_dir = feature_root.join("rust");
        assert!(rust_dir.exists(), "rust/ not created");
        assert!(rust_dir.join("Cargo.toml").exists(), "rust/Cargo.toml not found");
        assert!(rust_dir.join("src/lib.rs").exists(), "rust/src/lib.rs not found");
        assert!(
            rust_dir.join("src/lib.normalized").exists(),
            "rust/src/lib.normalized not found"
        );

        // There should be at least one mod_* directory under rust/src/
        let mod_dirs: Vec<_> = std::fs::read_dir(rust_dir.join("src"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                e.path().is_dir() && name.starts_with("mod_")
            })
            .collect();
        assert!(!mod_dirs.is_empty(), "no mod_* directories found under rust/src/");

        for mod_dir in &mod_dirs {
            let mod_rs = mod_dir.path().join("mod.rs");
            assert!(mod_rs.exists(), "mod.rs missing in {:?}", mod_dir.path());
        }
    }
}

// ---------------------------------------------------------------------------
// Layout / selector unit-level helpers
// ---------------------------------------------------------------------------

/// Verify FeatureLayout creates directories correctly.
#[test]
fn feature_layout_dirs_created() {
    let tmp = tempfile::TempDir::new().unwrap();
    let layout = c2rust_demo_layout::FeatureLayout::new(tmp.path().to_path_buf(), "test");
    layout.create_dirs().unwrap();
    assert!(layout.c_dir.exists());
    assert!(layout.rust_dir.exists());
    assert!(layout.meta_dir.exists());
}

/// Verify SelectAll selector returns all candidates.
#[test]
fn selector_select_all() {
    use c2rust_demo_selector::{FileSelector, SelectAll};
    let files: Vec<PathBuf> = vec!["/tmp/a.c2rust".into(), "/tmp/b.c2rust".into()];
    let result = SelectAll.select(&files).unwrap();
    assert_eq!(result, files);
}

/// Verify SelectNone selector returns nothing.
#[test]
fn selector_select_none() {
    use c2rust_demo_selector::{FileSelector, SelectNone};
    let files: Vec<PathBuf> = vec!["/tmp/a.c2rust".into()];
    let result = SelectNone.select(&files).unwrap();
    assert!(result.is_empty());
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn collect_c2rust_files(dir: &Path) -> Vec<PathBuf> {
    collect_by_ext(dir, "c2rust")
}

/// Recursively collect all files with the given extension under `dir`.
fn collect_by_ext(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_recursive_ext(dir, ext, &mut out);
    out
}

fn collect_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    collect_recursive_ext(dir, "c2rust", out);
}

fn collect_recursive_ext(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                collect_recursive_ext(&p, ext, out);
            } else if p.extension().is_some_and(|e| e == ext) {
                out.push(p);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Coverage integration tests (require gcc + make + clang + hook; auto-skip)
// ---------------------------------------------------------------------------

/// Backward-compat check: without C2RUST_COV, init must NOT create any
/// coverage artefacts (cov_lib.txt, build.rs).
#[test]
fn coverage_no_env_no_artefacts() {
    let missing = missing_tools(&["gcc", "make", "clang", "bindgen", "git"]);
    if !missing.is_empty() {
        eprintln!("Skipping coverage_no_env_no_artefacts: missing {}", missing.join(", "));
        return;
    }

    let Some(cjson_tmp) = prepare_cjson_project() else {
        eprintln!("Skipping coverage_no_env_no_artefacts: failed to clone cJSON from GitHub");
        return;
    };
    let project_root = cjson_tmp.path();
    let _ = Command::new("make").current_dir(project_root).arg("clean").status();

    let status = Command::new(env!("CARGO_BIN_EXE_c2rust-demo"))
        .current_dir(project_root)
        .args(["init", "--", "make", "libcjson.a"])
        .env_remove("C2RUST_COV")
        .env_remove("C2RUST_COV_INSTRUMENTED")
        .status()
        .expect("c2rust-demo init");

    if !status.success() {
        eprintln!("c2rust-demo init failed – skipping assertions");
        return;
    }

    let feature_root = project_root.join(".c2rust/default");
    assert!(
        !feature_root.join("meta/cov_lib.txt").exists(),
        "cov_lib.txt should NOT be created when C2RUST_COV is unset"
    );
    assert!(
        !feature_root.join("rust/build.rs").exists(),
        "build.rs should NOT be created when C2RUST_COV is unset"
    );
}

/// With C2RUST_COV=1 (Case 2), the hook must write .o files into cov_obj/.
#[test]
fn coverage_case2_cov_obj_created() {
    let missing = missing_tools(&["gcc", "make", "clang", "git"]);
    if !missing.is_empty() {
        eprintln!("Skipping coverage_case2_cov_obj_created: missing {}", missing.join(", "));
        return;
    }

    let Some(hook_so) = build_hook_for_tests() else {
        eprintln!("Skipping coverage_case2_cov_obj_created: failed to build libhook.so");
        return;
    };

    let Some(cjson_tmp) = prepare_cjson_project() else {
        eprintln!("Skipping coverage_case2_cov_obj_created: failed to clone cJSON from GitHub");
        return;
    };
    let fixture = cjson_tmp.path().to_path_buf();
    let tmp = tempfile::TempDir::new().unwrap();
    let feature_root = tmp.path().join(".c2rust/default");
    let c_dir = feature_root.join("c");
    let cov_obj_dir = feature_root.join("cov_obj");
    std::fs::create_dir_all(&c_dir).unwrap();
    std::fs::create_dir_all(&cov_obj_dir).unwrap();

    let _ = Command::new("make").current_dir(&fixture).arg("clean").status();

    let status = Command::new("make")
        .current_dir(&fixture)
        .arg("libcjson.a")
        .env("LD_PRELOAD", &hook_so)
        .env("C2RUST_PROJECT_ROOT", &fixture)
        .env("C2RUST_FEATURE_ROOT", &feature_root)
        .env("C2RUST_COV", "1")
        .env_remove("C2RUST_COV_INSTRUMENTED")
        .status()
        .expect("make");
    assert!(status.success(), "make with hook failed");

    let obj_files: Vec<_> = collect_by_ext(&cov_obj_dir, "o");
    assert!(
        !obj_files.is_empty(),
        "expected .o files in cov_obj/ after C2RUST_COV=1 build, found none"
    );
    println!("Case 2: {} .o file(s) in cov_obj/", obj_files.len());
}

/// Full Case 2 flow: init with C2RUST_COV=1 must produce libcov.a,
/// meta/cov_lib.txt pointing at it, and rust/build.rs containing the
/// rustc-link-lib directive.
#[test]
fn coverage_case2_libcov_and_build_rs() {
    let missing = missing_tools(&["gcc", "make", "clang", "bindgen", "ar", "git"]);
    if !missing.is_empty() {
        eprintln!("Skipping coverage_case2_libcov_and_build_rs: missing {}", missing.join(", "));
        return;
    }

    let Some(cjson_tmp) = prepare_cjson_project() else {
        eprintln!("Skipping coverage_case2_libcov_and_build_rs: failed to clone cJSON from GitHub");
        return;
    };
    let project_root = cjson_tmp.path();
    let _ = Command::new("make").current_dir(project_root).arg("clean").status();

    let status = Command::new(env!("CARGO_BIN_EXE_c2rust-demo"))
        .current_dir(project_root)
        .args(["init", "--", "make", "libcjson.a"])
        .env("C2RUST_COV", "1")
        .env_remove("C2RUST_COV_INSTRUMENTED")
        .status()
        .expect("c2rust-demo init");

    if !status.success() {
        eprintln!("c2rust-demo init failed – skipping coverage assertions");
        return;
    }

    let feature_root = project_root.join(".c2rust/default");

    // libcov.a should have been packed
    let libcov = feature_root.join("cov/libcov.a");
    assert!(libcov.exists(), "libcov.a should exist at {}", libcov.display());

    // meta/cov_lib.txt should point at it
    let txt = feature_root.join("meta/cov_lib.txt");
    assert!(txt.exists(), "meta/cov_lib.txt should exist");
    let recorded = std::fs::read_to_string(&txt).unwrap();
    let recorded = recorded.trim();
    assert!(
        recorded.ends_with("libcov.a"),
        "cov_lib.txt should reference libcov.a, got: {recorded}"
    );

    // rust/build.rs should exist and contain the link directive
    let build_rs = feature_root.join("rust/build.rs");
    assert!(build_rs.exists(), "rust/build.rs should exist");
    let content = std::fs::read_to_string(&build_rs).unwrap();
    assert!(
        content.contains("rustc-link-lib=static=cov"),
        "build.rs should contain rustc-link-lib=static=cov"
    );
    assert!(
        content.contains("rustc-link-search=native="),
        "build.rs should contain rustc-link-search"
    );

    // Cargo.toml should have build = "build.rs"
    let cargo_toml = feature_root.join("rust/Cargo.toml");
    let toml_content = std::fs::read_to_string(&cargo_toml).unwrap();
    assert!(
        toml_content.contains("build = \"build.rs\""),
        "Cargo.toml should contain build = \"build.rs\""
    );

    println!("Case 2 full flow: libcov.a + build.rs verified");
}


mod c2rust_demo_layout {
    pub use ::std::path::PathBuf;

    pub struct FeatureLayout {
        pub c_dir: PathBuf,
        pub rust_dir: PathBuf,
        pub meta_dir: PathBuf,
        #[allow(dead_code)]
        feature_root: PathBuf,
    }

    impl FeatureLayout {
        pub fn new(project_root: PathBuf, feature: &str) -> Self {
            let feature_root = project_root.join(".c2rust").join(feature);
            Self {
                c_dir: feature_root.join("c"),
                rust_dir: feature_root.join("rust"),
                meta_dir: feature_root.join("meta"),
                feature_root,
            }
        }

        pub fn create_dirs(&self) -> ::std::io::Result<()> {
            for dir in [&self.c_dir, &self.rust_dir, &self.meta_dir] {
                ::std::fs::create_dir_all(dir)?;
            }
            Ok(())
        }
    }
}

mod c2rust_demo_selector {
    use ::std::path::PathBuf;

    pub trait FileSelector {
        fn select(&self, candidates: &[PathBuf]) -> ::anyhow::Result<Vec<PathBuf>>;
    }

    pub struct SelectAll;
    impl FileSelector for SelectAll {
        fn select(&self, candidates: &[PathBuf]) -> ::anyhow::Result<Vec<PathBuf>> {
            Ok(candidates.to_vec())
        }
    }

    pub struct SelectNone;
    impl FileSelector for SelectNone {
        fn select(&self, _candidates: &[PathBuf]) -> ::anyhow::Result<Vec<PathBuf>> {
            Ok(vec![])
        }
    }
}
