use crate::error::Result;
use crate::layout::FeatureLayout;
use anyhow::anyhow;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The hook C source is embedded in the binary at compile time.
const HOOK_SRC: &str = include_str!("../hook/hook.c");

/// A compiled `libhook.so` living in a temporary directory.
///
/// The temporary directory is kept alive as long as this value is held.
/// Drop it only after the build command has finished.
pub struct BuiltHook {
    pub path: PathBuf,
    _dir: tempfile::TempDir,
}

/// Compile `libhook.so` from the embedded C source into a fresh temporary
/// directory.  Only `gcc` is required; `make` and the `hook/` source tree are
/// not needed at runtime.
pub fn build_hook() -> Result<BuiltHook> {
    let dir = tempfile::TempDir::new()
        .map_err(|e| anyhow!("failed to create temp dir for hook: {}", e))?;

    let src_path = dir.path().join("hook.c");
    std::fs::write(&src_path, HOOK_SRC)
        .map_err(|e| anyhow!("failed to write hook.c to {}: {}", src_path.display(), e))?;

    let so_path = dir.path().join("libhook.so");

    println!("Compiling hook library...");
    let status = Command::new("gcc")
        .args(["-Wall", "-fPIC", "-shared", "-o"])
        .arg(&so_path)
        .arg(&src_path)
        .arg("-ldl")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| anyhow!("failed to run gcc: {}", e))?;

    if !status.success() {
        return Err(anyhow!("gcc failed to compile libhook.so"));
    }

    if !so_path.exists() {
        return Err(anyhow!("libhook.so not found after compilation at {}", so_path.display()));
    }

    println!("Hook library compiled: {}", so_path.display());
    Ok(BuiltHook { path: so_path, _dir: dir })
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

/// After the build completes, optionally pack a coverage library.
///
/// - If `C2RUST_COV` is not set, returns `Ok(None)`.
/// - **Case 1** (`C2RUST_COV_INSTRUMENTED` is set): reads the absolute paths
///   of already-instrumented `.a` files from `c/cov_targets.list` and returns
///   the first one that exists on disk.
/// - **Case 2** (`C2RUST_COV_INSTRUMENTED` not set): collects all `.o` files
///   under `cov_obj/`, packs them with `ar rcs` into `cov/libcov.a`, and
///   returns that path.
///
/// Returns `Ok(None)` if coverage is disabled.
pub fn post_build_cov(lo: &FeatureLayout) -> Result<Option<PathBuf>> {
    let cov_enabled = std::env::var_os("C2RUST_COV").is_some_and(|v| !v.is_empty());
    if !cov_enabled {
        return Ok(None);
    }

    let already_instrumented =
        std::env::var_os("C2RUST_COV_INSTRUMENTED").is_some_and(|v| !v.is_empty());

    if already_instrumented {
        // Case 1: read cov_targets.list written by the hook
        let list_path = lo.c_dir.join("cov_targets.list");
        let content = std::fs::read_to_string(&list_path).map_err(|e| {
            anyhow!(
                "C2RUST_COV_INSTRUMENTED is set but {} could not be read: {}.\n\
                 Make sure the C build ran under the hook so that instrumented \
                 library paths were recorded.",
                list_path.display(),
                e
            )
        })?;
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let p = PathBuf::from(line);
            if p.exists() {
                return Ok(Some(p));
            }
        }
        return Err(anyhow!(
            "C2RUST_COV_INSTRUMENTED is set but no valid .a path was found in {}.\n\
             Please ensure the C build produced an instrumented static library.",
            list_path.display()
        ));
    }

    // Case 2: pack .o files from cov_obj/ into cov/libcov.a
    let obj_dir = &lo.cov_obj_dir;
    let obj_files: Vec<PathBuf> = walkdir::WalkDir::new(obj_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "o"))
        .map(|e| e.path().to_path_buf())
        .collect();

    if obj_files.is_empty() {
        return Err(anyhow!(
            "C2RUST_COV=1 is set but no .o files found in {}.\n\
             Make sure the C build ran under the hook with C2RUST_COV=1.",
            obj_dir.display()
        ));
    }

    std::fs::create_dir_all(&lo.cov_dir)
        .map_err(|e| anyhow!("create dir {}: {}", lo.cov_dir.display(), e))?;

    let cov_lib = lo.cov_lib_path();
    let mut ar_args = vec!["rcs".to_string(), cov_lib.display().to_string()];
    ar_args.extend(obj_files.iter().map(|p| p.display().to_string()));

    let status = Command::new("ar")
        .args(&ar_args)
        .status()
        .map_err(|e| anyhow!("failed to run ar: {}", e))?;

    if !status.success() {
        return Err(anyhow!("ar failed when creating {}", cov_lib.display()));
    }

    Ok(Some(cov_lib))
}


#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::test_utils::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn post_build_cov_returns_none_when_not_set() {
        let _g = env_lock();
        std::env::remove_var("C2RUST_COV");
        std::env::remove_var("C2RUST_COV_INSTRUMENTED");
        let tmp = TempDir::new().unwrap();
        let lo = FeatureLayout::new(tmp.path().to_path_buf(), "default");
        lo.create_dirs().unwrap();
        let result = post_build_cov(&lo).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn post_build_cov_case1_returns_path_from_targets_list() {
        let _g = env_lock();
        let tmp = TempDir::new().unwrap();
        let lo = FeatureLayout::new(tmp.path().to_path_buf(), "default");
        std::env::set_var("C2RUST_COV", "1");
        std::env::set_var("C2RUST_COV_INSTRUMENTED", "1");
        lo.create_dirs().unwrap();

        // Create a fake static library
        let fake_a = tmp.path().join("libfoo.a");
        std::fs::write(&fake_a, "").unwrap();

        // Write cov_targets.list
        std::fs::write(
            lo.c_dir.join("cov_targets.list"),
            format!("{}\n", fake_a.display()),
        )
        .unwrap();

        let result = post_build_cov(&lo);
        std::env::remove_var("C2RUST_COV");
        std::env::remove_var("C2RUST_COV_INSTRUMENTED");

        let path = result.unwrap().expect("should return Some");
        assert_eq!(path, fake_a);
    }

    #[test]
    fn post_build_cov_case1_errors_when_targets_list_missing() {
        let _g = env_lock();
        let tmp = TempDir::new().unwrap();
        let lo = FeatureLayout::new(tmp.path().to_path_buf(), "default");
        std::env::set_var("C2RUST_COV", "1");
        std::env::set_var("C2RUST_COV_INSTRUMENTED", "1");
        lo.create_dirs().unwrap();

        let result = post_build_cov(&lo);
        std::env::remove_var("C2RUST_COV");
        std::env::remove_var("C2RUST_COV_INSTRUMENTED");
        assert!(result.is_err());
    }

    /// Case 2：cov_obj 目录存在但没有 .o 文件时应返回错误。
    #[test]
    fn post_build_cov_case2_errors_when_cov_obj_empty() {
        let _g = env_lock();
        let tmp = TempDir::new().unwrap();
        let lo = FeatureLayout::new(tmp.path().to_path_buf(), "default");
        std::env::set_var("C2RUST_COV", "1");
        std::env::remove_var("C2RUST_COV_INSTRUMENTED");
        lo.create_dirs().unwrap();
        // cov_obj 目录已由 create_dirs 创建，但为空（无 .o 文件）

        let result = post_build_cov(&lo);
        std::env::remove_var("C2RUST_COV");
        assert!(
            result.is_err(),
            "should error when cov_obj is empty"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("no .o files found") || msg.contains("cov_obj"),
            "error message should mention missing .o files, got: {msg}"
        );
    }

    /// Case 1：targets list 存在但所有路径均无效时应返回错误。
    #[test]
    fn post_build_cov_case1_errors_when_all_paths_invalid() {
        let _g = env_lock();
        let tmp = TempDir::new().unwrap();
        let lo = FeatureLayout::new(tmp.path().to_path_buf(), "default");
        std::env::set_var("C2RUST_COV", "1");
        std::env::set_var("C2RUST_COV_INSTRUMENTED", "1");
        lo.create_dirs().unwrap();

        // 写入一个不存在的路径
        std::fs::write(
            lo.c_dir.join("cov_targets.list"),
            "/nonexistent/path/libfoo.a\n",
        )
        .unwrap();

        let result = post_build_cov(&lo);
        std::env::remove_var("C2RUST_COV");
        std::env::remove_var("C2RUST_COV_INSTRUMENTED");
        assert!(result.is_err(), "should error when no valid .a path exists");
    }

    /// Case 1：targets list 中有空行和注释行时应跳过，仅使用有效路径。
    #[test]
    fn post_build_cov_case1_skips_blank_lines() {
        let _g = env_lock();
        let tmp = TempDir::new().unwrap();
        let lo = FeatureLayout::new(tmp.path().to_path_buf(), "default");
        std::env::set_var("C2RUST_COV", "1");
        std::env::set_var("C2RUST_COV_INSTRUMENTED", "1");
        lo.create_dirs().unwrap();

        let fake_a = tmp.path().join("real.a");
        std::fs::write(&fake_a, b"").unwrap();

        // 写入含有空行的列表，最后一行才是真实路径
        std::fs::write(
            lo.c_dir.join("cov_targets.list"),
            format!("\n   \n{}\n", fake_a.display()),
        )
        .unwrap();

        let result = post_build_cov(&lo);
        std::env::remove_var("C2RUST_COV");
        std::env::remove_var("C2RUST_COV_INSTRUMENTED");

        let path = result.unwrap().expect("should return Some path");
        assert_eq!(path, fake_a);
    }
}
