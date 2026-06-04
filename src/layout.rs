use crate::error::Result;
use anyhow::anyhow;
use std::path::{Path, PathBuf};

/// Locate the project root by searching for `.c2rust/` upward from `start`.
/// Falls back to `start` itself if not found.
pub fn find_project_root(start: &Path) -> PathBuf {
    let mut cur = start.to_path_buf();
    loop {
        if cur.join(".c2rust").is_dir() {
            return cur;
        }
        match cur.parent() {
            Some(p) => cur = p.to_path_buf(),
            None => return start.to_path_buf(),
        }
    }
}

/// Layout of a single feature directory under `.c2rust/<feature>/`.
pub struct FeatureLayout {
    #[allow(dead_code)]
    pub project_root: PathBuf,
    #[allow(dead_code)]
    pub feature_name: String,
    /// `.c2rust/<feature>/`
    pub feature_root: PathBuf,
    /// `.c2rust/<feature>/c/`
    pub c_dir: PathBuf,
    /// `.c2rust/<feature>/rust/`
    pub rust_dir: PathBuf,
    /// `.c2rust/<feature>/meta/`
    pub meta_dir: PathBuf,
    /// `.c2rust/<feature>/cov_obj/`  (used when C2RUST_COV=1 without C2RUST_COV_INSTRUMENTED)
    pub cov_obj_dir: PathBuf,
    /// `.c2rust/<feature>/cov/`  (contains libcov.a packed from cov_obj/)
    pub cov_dir: PathBuf,
}

impl FeatureLayout {
    pub fn new(project_root: PathBuf, feature_name: &str) -> Self {
        let feature_root = project_root.join(".c2rust").join(feature_name);
        Self {
            c_dir: feature_root.join("c"),
            rust_dir: feature_root.join("rust"),
            meta_dir: feature_root.join("meta"),
            cov_obj_dir: feature_root.join("cov_obj"),
            cov_dir: feature_root.join("cov"),
            feature_root,
            project_root,
            feature_name: feature_name.to_string(),
        }
    }

    /// Create all required directories.
    /// When `C2RUST_COV` is set and `C2RUST_COV_INSTRUMENTED` is not set,
    /// also creates `cov_obj/` for the hook to write instrumented objects into.
    pub fn create_dirs(&self) -> Result<()> {
        for dir in [&self.c_dir, &self.rust_dir, &self.meta_dir] {
            std::fs::create_dir_all(dir)
                .map_err(|e| anyhow!("create dir {}: {}", dir.display(), e))?;
        }
        let cov_enabled = std::env::var_os("C2RUST_COV").is_some_and(|v| !v.is_empty());
        let already_instrumented =
            std::env::var_os("C2RUST_COV_INSTRUMENTED").is_some_and(|v| !v.is_empty());
        if cov_enabled && !already_instrumented {
            std::fs::create_dir_all(&self.cov_obj_dir)
                .map_err(|e| anyhow!("create dir {}: {}", self.cov_obj_dir.display(), e))?;
        }
        Ok(())
    }

    /// Absolute path to the packed coverage library: `.c2rust/<feature>/cov/libcov.a`.
    pub fn cov_lib_path(&self) -> PathBuf {
        self.cov_dir.join("libcov.a")
    }

    /// Write `meta/cov_lib.txt` with the absolute path of the coverage library.
    pub fn save_cov_lib_path(&self, lib: &Path) -> Result<()> {
        let path = self.meta_dir.join("cov_lib.txt");
        std::fs::write(&path, lib.display().to_string())
            .map_err(|e| anyhow!("write {}: {}", path.display(), e))
    }

    /// Read `meta/cov_lib.txt` and return the coverage library path, if it exists.
    pub fn read_cov_lib_path(&self) -> Option<PathBuf> {
        let path = self.meta_dir.join("cov_lib.txt");
        std::fs::read_to_string(&path)
            .ok()
            .map(|s| PathBuf::from(s.trim()))
            .filter(|p| !p.as_os_str().is_empty())
    }

    /// Write `meta/build_cmd.txt`.
    pub fn save_build_cmd(&self, cmd: &[String]) -> Result<()> {
        let path = self.meta_dir.join("build_cmd.txt");
        std::fs::write(&path, cmd.join(" "))
            .map_err(|e| anyhow!("write {}: {}", path.display(), e))
    }

    /// Write `meta/selected_files.json`.
    pub fn save_selected_files(&self, files: &[PathBuf]) -> Result<()> {
        let list: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
        let json = serde_json::to_string_pretty(&list)
            .map_err(|e| anyhow!("serialize selected_files: {}", e))?;
        let path = self.meta_dir.join("selected_files.json");
        std::fs::write(&path, json).map_err(|e| anyhow!("write {}: {}", path.display(), e))
    }
}

/// Scan `.c2rust/<feature>/c/` for all `*.c2rust` files.
pub fn scan_c2rust_files(c_dir: &Path) -> Result<Vec<PathBuf>> {
    if !c_dir.exists() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    visit_dir(c_dir, &mut out)?;
    out.sort();
    Ok(out)
}

fn visit_dir(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).map_err(|e| anyhow!("read_dir {}: {}", dir.display(), e))? {
        let entry = entry.map_err(|e| anyhow!("read entry: {}", e))?;
        let path = entry.path();
        if path.is_dir() {
            visit_dir(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "c2rust") {
            out.push(path);
        }
    }
    Ok(())
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
    fn find_project_root_in_current_dir() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join(".c2rust")).unwrap();
        assert_eq!(find_project_root(tmp.path()), tmp.path());
    }

    #[test]
    fn find_project_root_in_parent() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join(".c2rust")).unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        assert_eq!(find_project_root(&sub), tmp.path());
    }

    #[test]
    fn find_project_root_fallback() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        // No .c2rust anywhere in tmp chain – fallback to start
        assert_eq!(find_project_root(&sub), sub);
    }

    #[test]
    fn feature_layout_create_dirs() {
        let tmp = TempDir::new().unwrap();
        let layout = FeatureLayout::new(tmp.path().to_path_buf(), "default");
        layout.create_dirs().unwrap();
        assert!(layout.c_dir.exists());
        assert!(layout.rust_dir.exists());
        assert!(layout.meta_dir.exists());
    }

    #[test]
    fn save_build_cmd_writes_file() {
        let tmp = TempDir::new().unwrap();
        let layout = FeatureLayout::new(tmp.path().to_path_buf(), "default");
        layout.create_dirs().unwrap();
        layout.save_build_cmd(&["make".into(), "-j4".into()]).unwrap();
        let content = std::fs::read_to_string(layout.meta_dir.join("build_cmd.txt")).unwrap();
        assert_eq!(content, "make -j4");
    }

    #[test]
    fn save_selected_files_writes_json() {
        let tmp = TempDir::new().unwrap();
        let layout = FeatureLayout::new(tmp.path().to_path_buf(), "default");
        layout.create_dirs().unwrap();
        let files = vec![PathBuf::from("/foo/bar.c2rust")];
        layout.save_selected_files(&files).unwrap();
        let content = std::fs::read_to_string(layout.meta_dir.join("selected_files.json")).unwrap();
        assert!(content.contains("bar.c2rust"));
    }

    #[test]
    fn scan_c2rust_files_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let files = scan_c2rust_files(tmp.path()).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn scan_c2rust_files_finds_files() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.c2rust"), "").unwrap();
        std::fs::write(tmp.path().join("b.c"), "").unwrap();
        let files = scan_c2rust_files(tmp.path()).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("a.c2rust"));
    }

    #[test]
    fn cov_obj_dir_path_is_correct() {
        let tmp = TempDir::new().unwrap();
        let layout = FeatureLayout::new(tmp.path().to_path_buf(), "myfeature");
        assert_eq!(
            layout.cov_obj_dir,
            tmp.path().join(".c2rust/myfeature/cov_obj")
        );
    }

    #[test]
    fn cov_lib_path_is_correct() {
        let tmp = TempDir::new().unwrap();
        let layout = FeatureLayout::new(tmp.path().to_path_buf(), "myfeature");
        assert_eq!(
            layout.cov_lib_path(),
            tmp.path().join(".c2rust/myfeature/cov/libcov.a")
        );
    }

    #[test]
    fn save_and_read_cov_lib_path_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let layout = FeatureLayout::new(tmp.path().to_path_buf(), "default");
        layout.create_dirs().unwrap();
        let lib = PathBuf::from("/some/path/libcov.a");
        layout.save_cov_lib_path(&lib).unwrap();
        let roundtrip = layout.read_cov_lib_path().expect("should have cov_lib.txt");
        assert_eq!(roundtrip, lib);
    }

    #[test]
    fn read_cov_lib_path_returns_none_when_missing() {
        let tmp = TempDir::new().unwrap();
        let layout = FeatureLayout::new(tmp.path().to_path_buf(), "default");
        layout.create_dirs().unwrap();
        assert!(layout.read_cov_lib_path().is_none());
    }

    #[test]
    fn create_dirs_with_cov_creates_cov_obj() {
        let _g = env_lock();
        let tmp = TempDir::new().unwrap();
        let layout = FeatureLayout::new(tmp.path().to_path_buf(), "default");
        std::env::set_var("C2RUST_COV", "1");
        std::env::remove_var("C2RUST_COV_INSTRUMENTED");
        let result = layout.create_dirs();
        std::env::remove_var("C2RUST_COV");
        result.unwrap();
        assert!(layout.cov_obj_dir.exists());
    }

    #[test]
    fn create_dirs_without_cov_no_cov_obj() {
        let _g = env_lock();
        let tmp = TempDir::new().unwrap();
        let layout = FeatureLayout::new(tmp.path().to_path_buf(), "default");
        std::env::remove_var("C2RUST_COV");
        layout.create_dirs().unwrap();
        assert!(!layout.cov_obj_dir.exists());
    }

    /// scan_c2rust_files 应当递归地在子目录中找到 .c2rust 文件。
    #[test]
    fn scan_c2rust_files_recursive() {
        let tmp = TempDir::new().unwrap();
        let sub = tmp.path().join("subdir");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(tmp.path().join("top.c2rust"), "").unwrap();
        std::fs::write(sub.join("nested.c2rust"), "").unwrap();
        std::fs::write(sub.join("ignored.c"), "").unwrap();
        let mut files = scan_c2rust_files(tmp.path()).unwrap();
        files.sort();
        assert_eq!(files.len(), 2, "should find 2 .c2rust files recursively");
        assert!(files.iter().any(|p| p.ends_with("top.c2rust")));
        assert!(files.iter().any(|p| p.ends_with("nested.c2rust")));
    }

    /// scan_c2rust_files 在目录不存在时返回空列表而不报错。
    #[test]
    fn scan_c2rust_files_nonexistent_dir_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let absent = tmp.path().join("does_not_exist");
        let files = scan_c2rust_files(&absent).unwrap();
        assert!(files.is_empty());
    }

    /// scan_c2rust_files 返回的路径应按字典序排序以保证确定性。
    #[test]
    fn scan_c2rust_files_sorted() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("z.c2rust"), "").unwrap();
        std::fs::write(tmp.path().join("a.c2rust"), "").unwrap();
        std::fs::write(tmp.path().join("m.c2rust"), "").unwrap();
        let files = scan_c2rust_files(tmp.path()).unwrap();
        assert_eq!(files.len(), 3);
        let names: Vec<_> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "files should be returned in sorted order");
    }

    /// save_selected_files 写入空列表时，JSON 文件内容为空数组。
    #[test]
    fn save_selected_files_empty_list() {
        let tmp = TempDir::new().unwrap();
        let layout = FeatureLayout::new(tmp.path().to_path_buf(), "default");
        layout.create_dirs().unwrap();
        layout.save_selected_files(&[]).unwrap();
        let content =
            std::fs::read_to_string(layout.meta_dir.join("selected_files.json")).unwrap();
        assert!(
            content.trim() == "[]",
            "empty selection should write '[]', got: {content}"
        );
    }

    /// 自定义 feature 名称时，各目录路径应反映该名称。
    #[test]
    fn feature_layout_custom_feature_name_paths() {
        let tmp = TempDir::new().unwrap();
        let layout = FeatureLayout::new(tmp.path().to_path_buf(), "v2");
        assert_eq!(layout.c_dir, tmp.path().join(".c2rust/v2/c"));
        assert_eq!(layout.rust_dir, tmp.path().join(".c2rust/v2/rust"));
        assert_eq!(layout.meta_dir, tmp.path().join(".c2rust/v2/meta"));
        assert_eq!(layout.cov_obj_dir, tmp.path().join(".c2rust/v2/cov_obj"));
        assert_eq!(layout.cov_dir, tmp.path().join(".c2rust/v2/cov"));
    }

    /// save_build_cmd 写入单个词的构建命令时，文件中不含多余空格。
    #[test]
    fn save_build_cmd_single_arg() {
        let tmp = TempDir::new().unwrap();
        let layout = FeatureLayout::new(tmp.path().to_path_buf(), "default");
        layout.create_dirs().unwrap();
        layout.save_build_cmd(&["cmake".into()]).unwrap();
        let content =
            std::fs::read_to_string(layout.meta_dir.join("build_cmd.txt")).unwrap();
        assert_eq!(content, "cmake");
    }

    /// find_project_root 在深层嵌套子目录中仍能找到含 .c2rust 的祖先。
    #[test]
    fn find_project_root_deep_nesting() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join(".c2rust")).unwrap();
        let deep = tmp.path().join("a/b/c/d");
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(find_project_root(&deep), tmp.path());
    }

    /// create_dirs_with_cov_instrumented_does_not_create_cov_obj 验证
    /// C2RUST_COV_INSTRUMENTED 被设置时不会创建 cov_obj 目录（Case 1）。
    #[test]
    fn create_dirs_with_cov_instrumented_does_not_create_cov_obj() {
        let _g = env_lock();
        let tmp = TempDir::new().unwrap();
        let layout = FeatureLayout::new(tmp.path().to_path_buf(), "default");
        std::env::set_var("C2RUST_COV", "1");
        std::env::set_var("C2RUST_COV_INSTRUMENTED", "1");
        let result = layout.create_dirs();
        std::env::remove_var("C2RUST_COV");
        std::env::remove_var("C2RUST_COV_INSTRUMENTED");
        result.unwrap();
        assert!(
            !layout.cov_obj_dir.exists(),
            "cov_obj should NOT be created when C2RUST_COV_INSTRUMENTED is set"
        );
    }
}
