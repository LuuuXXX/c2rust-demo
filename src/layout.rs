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
}

impl FeatureLayout {
    pub fn new(project_root: PathBuf, feature_name: &str) -> Self {
        let feature_root = project_root.join(".c2rust").join(feature_name);
        Self {
            c_dir: feature_root.join("c"),
            rust_dir: feature_root.join("rust"),
            meta_dir: feature_root.join("meta"),
            feature_root,
            project_root,
            feature_name: feature_name.to_string(),
        }
    }

    /// Create all required directories.
    pub fn create_dirs(&self) -> Result<()> {
        for dir in [&self.c_dir, &self.rust_dir, &self.meta_dir] {
            std::fs::create_dir_all(dir)
                .map_err(|e| anyhow!("create dir {}: {}", dir.display(), e))?;
        }
        Ok(())
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
}
