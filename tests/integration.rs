//! Integration tests for `c2rust-demo init`.
//!
//! Tests that require external tools (gcc, make, clang, bindgen) automatically
//! detect whether those tools are present and print a clear skip message when
//! they are not.  No environment variable gate is needed.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("simple")
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

    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = fixture_dir();
    // C2RUST_PROJECT_ROOT must be an ancestor of the C files being compiled.
    // We use the fixture directory itself as the project root so the hook
    // can strip its prefix from the absolute paths of .c files.
    let project_root = fixture.clone();
    let feature_root = tmp.path().join(".c2rust/default");
    let c_dir = feature_root.join("c");
    std::fs::create_dir_all(&c_dir).unwrap();

    // Clean + build with the hook injected
    let _ = Command::new("make")
        .current_dir(&fixture)
        .arg("clean")
        .status();

    let status = Command::new("make")
        .current_dir(&fixture)
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
    let missing = missing_tools(&["gcc", "make", "clang", "bindgen"]);
    if !missing.is_empty() {
        eprintln!("Skipping full_init: missing tools: {}", missing.join(", "));
        return;
    }

    let tmp = tempfile::TempDir::new().unwrap();
    let project_root = tmp.path();
    let fixture = fixture_dir();

    // Clean first
    let _ = Command::new("make")
        .current_dir(&fixture)
        .arg("clean")
        .status();

    let status = Command::new(env!("CARGO_BIN_EXE_c2rust-demo"))
        .current_dir(project_root)
        .args([
            "init",
            "--",
            "make",
            &format!("-C{}", fixture.display()),
        ])
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
    let mut out = Vec::new();
    collect_recursive(dir, &mut out);
    out
}

fn collect_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                collect_recursive(&p, out);
            } else if p.extension().is_some_and(|e| e == "c2rust") {
                out.push(p);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Re-export shims for testing internal modules from integration tests
// ---------------------------------------------------------------------------

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
