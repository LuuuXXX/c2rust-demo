use crate::error::Result;
use anyhow::anyhow;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Build the `libhook.so` from `hook/Makefile` adjacent to the binary.
///
/// Returns the path to the built `libhook.so`.
pub fn build_hook() -> Result<PathBuf> {
    let hook_dir = hook_dir()?;
    let so = hook_dir.join("libhook.so");

    println!("Building hook library from {}...", hook_dir.display());
    let status = Command::new("make")
        .current_dir(&hook_dir)
        .arg("-s")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| anyhow!("failed to run make in {}: {}", hook_dir.display(), e))?;

    if !status.success() {
        return Err(anyhow!("make failed in {}", hook_dir.display()));
    }

    if !so.exists() {
        return Err(anyhow!("libhook.so not found after build at {}", so.display()));
    }

    println!("Hook library built: {}", so.display());
    Ok(so)
}

/// Execute the user-supplied build command with LD_PRELOAD set to libhook.so.
pub fn run_with_hook(
    build_dir: &Path,
    cmd: &[String],
    project_root: &Path,
    feature_root: &Path,
    hook_so: &Path,
) -> Result<()> {
    if cmd.is_empty() {
        return Err(anyhow!("build command is empty"));
    }

    let abs_project_root = project_root
        .canonicalize()
        .map_err(|e| anyhow!("canonicalize {}: {}", project_root.display(), e))?;
    let abs_feature_root = feature_root
        .canonicalize()
        .map_err(|e| anyhow!("canonicalize {}: {}", feature_root.display(), e))?;
    let abs_hook = hook_so
        .canonicalize()
        .map_err(|e| anyhow!("canonicalize {}: {}", hook_so.display(), e))?;

    println!("Running build command: {}", cmd.join(" "));
    println!("  C2RUST_PROJECT_ROOT = {}", abs_project_root.display());
    println!("  C2RUST_FEATURE_ROOT = {}", abs_feature_root.display());
    println!("  LD_PRELOAD          = {}", abs_hook.display());
    println!();

    let status = Command::new(&cmd[0])
        .args(&cmd[1..])
        .current_dir(build_dir)
        .env("LD_PRELOAD", &abs_hook)
        .env("C2RUST_PROJECT_ROOT", &abs_project_root)
        .env("C2RUST_FEATURE_ROOT", &abs_feature_root)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| anyhow!("failed to spawn '{}': {}", cmd[0], e))?;

    if !status.success() {
        return Err(anyhow!(
            "build command failed with exit code {}",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

/// Locate the `hook/` directory, starting from the directory of the running
/// binary and searching upward.  Falls back to a path relative to the manifest.
fn hook_dir() -> Result<PathBuf> {
    // 1. Try next to the running binary (installed or `cargo run`)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let candidate = parent.join("hook");
            if candidate.join("Makefile").exists() {
                return Ok(candidate);
            }
            // When built with `cargo`, the binary lives in target/debug or target/release.
            // Walk up two more levels to find the workspace root.
            if let Some(workspace) = parent.parent().and_then(|p| p.parent()) {
                let candidate = workspace.join("hook");
                if candidate.join("Makefile").exists() {
                    return Ok(candidate);
                }
            }
        }
    }

    // 2. Relative to the current working directory (development)
    let cwd_candidate = std::env::current_dir()
        .map_err(|e| anyhow!("current_dir: {}", e))?
        .join("hook");
    if cwd_candidate.join("Makefile").exists() {
        return Ok(cwd_candidate);
    }

    Err(anyhow!(
        "hook/ directory with Makefile not found (searched near binary and cwd)"
    ))
}
