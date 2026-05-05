//! Parsing of `.c2rust` preprocessed C files into an AST.
//!
//! This module is a minimal port of `c2rust-code-analyse/src/file.rs`,
//! retaining only the functionality required for the `init` flow.

use crate::error::{Result, ToError};
use clang_ast::{BareSourceLocation, SourceLocation, SourceRange};
use serde_derive::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub type Node = clang_ast::Node<Kind>;

// ---------------------------------------------------------------------------
// Source-range helpers
// ---------------------------------------------------------------------------

fn read_file_range(path: &Path, start: usize, end: usize) -> Result<String> {
    let file = fs::File::open(path).ctx(&format!("open {}", path.display()))?;
    let mmap = unsafe { memmap2::Mmap::map(&file).map_err(|e| anyhow::anyhow!("mmap: {}", e))? };
    if start >= mmap.len() || end <= start {
        return Ok(String::new());
    }
    let bytes = &mmap[start..end.min(mmap.len())];
    Ok(String::from_utf8_lossy(bytes).to_string())
}

fn read_c_code(root: &Path, range: &SourceRange) -> Result<String> {
    let Some(ref beg) = range.begin.expansion_loc else {
        return Ok(String::new());
    };
    let Some(ref end) = range.end.expansion_loc else {
        return Ok(String::new());
    };
    read_file_range(&root.join(&*beg.file), beg.offset, end.offset + end.tok_len)
}

fn read_before(root: &Path, pos: &SourceRange) -> Result<String> {
    let Some(ref pos) = pos.begin.expansion_loc else {
        return Ok(String::new());
    };
    read_file_range(&root.join(&*pos.file), 0, pos.offset)
}

fn read_after(root: &Path, pos: &SourceRange) -> Result<String> {
    let Some(ref pos) = pos.end.expansion_loc else {
        return Ok(String::new());
    };
    read_file_range(
        &root.join(&*pos.file),
        pos.offset + pos.tok_len,
        usize::MAX,
    )
}

fn read_between(root: &Path, beg: &SourceRange, end: &SourceRange) -> Result<String> {
    let Some(ref beg_loc) = beg.end.expansion_loc else {
        return read_before(root, end);
    };
    let Some(ref end_loc) = end.begin.expansion_loc else {
        return read_after(root, beg);
    };
    if end_loc.file == beg_loc.file {
        read_file_range(
            &root.join(&*beg_loc.file),
            beg_loc.offset + beg_loc.tok_len,
            end_loc.offset,
        )
    } else {
        Ok(read_after(root, beg)? + &read_before(root, end)?)
    }
}

fn read_code_between(
    root: &Path,
    beg: Option<&SourceRange>,
    end: Option<&SourceRange>,
) -> Result<String> {
    match (beg, end) {
        (Some(beg), Some(end)) => read_between(root, beg, end),
        (None, Some(end)) => read_before(root, end),
        (Some(beg), None) => read_after(root, beg),
        _ => Ok(String::new()),
    }
}

fn range_include(range: &SourceRange, included: &SourceRange) -> bool {
    let (Some(beg1), Some(beg2)) = (
        range.begin.expansion_loc.as_ref(),
        included.begin.expansion_loc.as_ref(),
    ) else {
        return false;
    };
    let (Some(end1), Some(end2)) = (
        range.end.expansion_loc.as_ref(),
        included.end.expansion_loc.as_ref(),
    ) else {
        return false;
    };
    beg1.file == beg2.file && beg1.offset <= beg2.offset && end1.offset >= end2.offset
}

fn remove_unused_attrs(code: &mut String) {
    let mut off = 0;
    let re = regex::Regex::new(r"__attribute__\s*\(\s*\(\s*always_inline\s*\)\s*\)").unwrap();
    while let Some(m) = re.find(&code[off..]) {
        off += m.start();
        code.replace_range(off..off + m.len(), "");
    }
    off = 0;
    let re =
        regex::Regex::new(r"__attribute__\s*\(\s*\(\s*__malloc__\s*(\(.+\))?\)\s*\)").unwrap();
    while let Some(m) = re.find(&code[off..]) {
        off += m.start();
        code.replace_range(off..off + m.len(), "");
    }
    let mut off = 0;
    let re =
        regex::Regex::new(r"__attribute__\s*\(\s*\(\s*[^)]*inline[^)]*\)\s*\)").unwrap();
    while let Some(m) = re.find(&code[off..]) {
        off += m.start();
        code.replace_range(off..off + m.len(), "");
    }
}

// ---------------------------------------------------------------------------
// AST node kind definitions
// ---------------------------------------------------------------------------

#[derive(Deserialize, Serialize, Debug, Clone)]
pub enum Kind {
    EnumDecl(EnumDecl),
    RecordDecl(RecordDecl),
    FunctionDecl(FunctionDecl),
    VarDecl(VarDecl),
    TypedefDecl(TypedefDecl),
    TranslationUnitDecl(TranslationUnitDecl),
    CompoundStmt,
    Other(OtherDecl),
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct OtherDecl {
    kind: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct TranslationUnitDecl {
    md5: Option<String>,
    #[serde(default)]
    git_commit: bool,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct MyClangType {
    #[serde(rename = "qualType")]
    qual_type: String,
    #[serde(rename = "desugaredQualType")]
    desugared_qual_type: Option<String>,
}

impl MyClangType {
    fn typedef(&self) -> &str {
        self.desugared_qual_type
            .as_deref()
            .unwrap_or(&self.qual_type)
    }

    pub fn fill_array_size(&self, c_code: &mut String) -> Result<()> {
        let ty = Self::ignore_fn(self.typedef());
        let (off, end) = Self::ignore_fn_range(ty);
        let ty_without_fn = &ty[off..end];

        let array_re = regex::Regex::new(r"(\[\s*\d*\s*\]\s*)+$").unwrap();

        let c_code_match = if let Some(cap) = array_re.captures(c_code) {
            cap[0].to_string()
        } else {
            return Ok(());
        };

        let typedef_match = if let Some(cap) = array_re.captures(ty_without_fn) {
            cap[0].to_string()
        } else {
            return Err(anyhow::anyhow!("fill_array_size: typedef has no array dims"));
        };

        let typedef_count = typedef_match.chars().filter(|&c| c == '[').count();
        let c_code_count = c_code_match.chars().filter(|&c| c == '[').count();

        if c_code_count != typedef_count {
            return Err(anyhow::anyhow!("fill_array_size: dimension count mismatch"));
        }

        let cap = array_re.captures(c_code).unwrap();
        let (start, end) = (cap.get(0).unwrap().start(), cap.get(0).unwrap().end());
        c_code.replace_range(start..end, &typedef_match);

        Ok(())
    }

    pub fn is_const(&self) -> bool {
        Self::is_const_ty(self.typedef())
    }

    fn is_const_ty(ty: &str) -> bool {
        let re = regex::Regex::new(r"\bconst\b[^\*]*$").unwrap();
        re.is_match(Self::ignore_fn(ty))
    }

    fn ignore_fn(ty: &str) -> &str {
        let (off, end) = Self::ignore_fn_range(ty);
        &ty[off..end]
    }

    fn ignore_fn_range(ty: &str) -> (usize, usize) {
        let re = regex::Regex::new(r"^[^\(]*\(\s*[^\s]").unwrap();
        let mut off = 0;
        let mut end = ty.len();
        while let Some(m) = re.find(&ty[off..end]) {
            if ty.as_bytes()[off + m.len() - 1] != b'*' {
                break;
            }
            off += m.len();
            let mut cnt = 1;
            for n in off..end {
                match ty.as_bytes()[n] {
                    b'(' => cnt += 1,
                    b')' => {
                        cnt -= 1;
                        if cnt == 0 {
                            end = n;
                            break;
                        }
                    }
                    _ => continue,
                }
            }
        }
        (off, end)
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct TypedefDecl {
    name: String,
    loc: SourceLocation,
    range: SourceRange,
    #[serde(rename = "type")]
    ty: MyClangType,
    #[serde(default, rename = "isImplicit")]
    is_implicit: bool,
    #[serde(default)]
    skip: bool,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct EnumDecl {
    name: Option<String>,
    loc: SourceLocation,
    range: SourceRange,
    #[serde(default, rename = "completeDefinition")]
    is_definition: bool,
    #[serde(default, rename = "isImplicit")]
    is_implicit: bool,
    #[serde(default)]
    skip: bool,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct RecordDecl {
    name: Option<String>,
    loc: SourceLocation,
    range: SourceRange,
    #[serde(rename = "tagUsed")]
    tag_used: String,
    #[serde(default, rename = "completeDefinition")]
    is_definition: bool,
    #[serde(default, rename = "isImplicit")]
    is_implicit: bool,
    #[serde(default)]
    skip: bool,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct VarDecl {
    pub name: String,
    loc: SourceLocation,
    range: SourceRange,
    #[serde(rename = "type")]
    pub ty: MyClangType,
    #[serde(rename = "storageClass")]
    storage_class: Option<String>,
    init: Option<String>,
    #[serde(default)]
    fake_init: bool,
    #[serde(default, rename = "isImplicit")]
    is_implicit: bool,
    #[serde(default)]
    git_commit: bool,
    global_name: Option<String>,
    #[serde(default)]
    skip: bool,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct FunctionDecl {
    pub name: String,
    loc: SourceLocation,
    range: SourceRange,
    #[serde(default, rename = "completeDefinition")]
    is_definition: bool,
    #[serde(default, rename = "isImplicit")]
    is_implicit: bool,
    #[serde(default)]
    inline: bool,
    #[serde(rename = "type")]
    pub ty: MyClangType,
    #[serde(rename = "storageClass")]
    storage_class: Option<String>,
    #[serde(default)]
    git_commit: bool,
    global_name: Option<String>,
    #[serde(default)]
    skip: bool,
}

// ---------------------------------------------------------------------------
// Kind impl
// ---------------------------------------------------------------------------

impl Kind {
    fn loc(&self) -> Option<&SourceLocation> {
        match self {
            Kind::EnumDecl(i) => Some(&i.loc),
            Kind::RecordDecl(i) => Some(&i.loc),
            Kind::FunctionDecl(i) => Some(&i.loc),
            Kind::VarDecl(i) => Some(&i.loc),
            Kind::TypedefDecl(i) => Some(&i.loc),
            _ => None,
        }
    }

    fn loc_mut(&mut self) -> Option<&mut SourceLocation> {
        match self {
            Kind::EnumDecl(i) => Some(&mut i.loc),
            Kind::RecordDecl(i) => Some(&mut i.loc),
            Kind::FunctionDecl(i) => Some(&mut i.loc),
            Kind::VarDecl(i) => Some(&mut i.loc),
            Kind::TypedefDecl(i) => Some(&mut i.loc),
            _ => None,
        }
    }

    fn range(&self) -> Option<&SourceRange> {
        match self {
            Kind::EnumDecl(i) => Some(&i.range),
            Kind::RecordDecl(i) => Some(&i.range),
            Kind::FunctionDecl(i) => Some(&i.range),
            Kind::VarDecl(i) => Some(&i.range),
            Kind::TypedefDecl(i) => Some(&i.range),
            _ => None,
        }
    }

    pub fn set_skip(&mut self) {
        match self {
            Kind::EnumDecl(i) => i.skip = true,
            Kind::RecordDecl(i) => i.skip = true,
            Kind::FunctionDecl(i) => i.skip = true,
            Kind::VarDecl(i) => i.skip = true,
            Kind::TypedefDecl(i) => i.skip = true,
            _ => {}
        }
    }

    pub fn skip(&self) -> bool {
        match self {
            Kind::EnumDecl(i) => i.skip,
            Kind::RecordDecl(i) => i.skip,
            Kind::FunctionDecl(i) => i.skip,
            Kind::VarDecl(i) => i.skip,
            Kind::TypedefDecl(i) => i.skip,
            _ => true,
        }
    }

    pub fn name(&self) -> Option<&str> {
        match self {
            Kind::EnumDecl(i) => i.name.as_deref(),
            Kind::RecordDecl(i) => i.name.as_deref(),
            Kind::FunctionDecl(i) => Some(i.name.as_str()),
            Kind::VarDecl(i) => Some(i.name.as_str()),
            Kind::TypedefDecl(i) => Some(i.name.as_str()),
            _ => None,
        }
    }

    pub fn is_fun_declare(&self, inner: &[Node]) -> bool {
        !inner.iter().any(|e| matches!(e.kind, Kind::CompoundStmt))
    }

    pub fn is_weak_fn(&self, inner: &[Node]) -> bool {
        if !matches!(self, Kind::FunctionDecl(_)) {
            return false;
        }
        inner
            .iter()
            .any(|n| matches!(&n.kind, Kind::Other(o) if o.kind.as_deref() == Some("WeakAttr")))
    }

    pub fn is_const_var(&self) -> bool {
        let Kind::VarDecl(var) = self else {
            return false;
        };
        var.ty.is_const()
    }

    pub fn is_inline(&self) -> bool {
        let Kind::FunctionDecl(ref item) = self else {
            return false;
        };
        item.inline
    }

    pub fn is_static(&self) -> bool {
        let storage_class = match self {
            Kind::FunctionDecl(ref item) => &item.storage_class,
            Kind::VarDecl(ref item) => &item.storage_class,
            _ => return false,
        };
        matches!(storage_class.as_deref(), Some("static"))
    }

    pub fn global_name(&self) -> Option<&str> {
        match self {
            Kind::FunctionDecl(ref item) => item.global_name.as_deref(),
            Kind::VarDecl(ref item) => item.global_name.as_deref(),
            _ => None,
        }
    }

    pub fn set_global_name(&mut self, global_name: String) {
        match self {
            Kind::FunctionDecl(ref mut item) => item.global_name = Some(global_name),
            Kind::VarDecl(ref mut item) => item.global_name = Some(global_name),
            _ => {}
        }
    }

    pub fn is_extern(&self) -> bool {
        let storage_class = match self {
            Kind::VarDecl(ref item) => &item.storage_class,
            _ => return false,
        };
        matches!(storage_class.as_ref().map(|s| s.as_str()), Some("extern"))
    }

    pub fn has_committed(&self) -> bool {
        match self {
            Kind::FunctionDecl(ref item) => item.git_commit,
            Kind::VarDecl(ref item) => item.git_commit,
            _ => false,
        }
    }

    #[allow(dead_code)]
    pub fn set_git_commit(&mut self, committed: bool) {
        match self {
            Kind::FunctionDecl(ref mut item) => item.git_commit = committed,
            Kind::VarDecl(ref mut item) => item.git_commit = committed,
            _ => {}
        };
    }

    pub fn is_inited(&self) -> bool {
        match self {
            Kind::VarDecl(ref item) => item.init.is_some() || item.fake_init,
            _ => false,
        }
    }

    pub fn is_fake_inited(&self) -> bool {
        match self {
            Kind::VarDecl(ref item) => item.fake_init,
            _ => false,
        }
    }

    pub fn is_variadic(&self) -> bool {
        let Kind::FunctionDecl(ref item) = self else {
            return false;
        };
        item.ty.typedef().contains("...")
            || item.ty.typedef().contains("struct __va_list_tag")
    }

    fn tail_code(&self, root: &Path, end: Option<&SourceRange>) -> Result<String> {
        let code = read_code_between(root, self.range(), end)?;
        Ok(code)
    }

    fn rename_macro(&self) -> Option<String> {
        if !self.is_static() && !self.is_inline() {
            return None;
        }
        let Some(global_name) = self.global_name() else {
            eprintln!("empty global name: {:?}", self.name());
            return None;
        };
        let name = self.name()?;
        Some(format!(
            r##"
#if !defined({name})
    #define {name} {global_name}
#endif
"##
        ))
    }

    // Produce the C source text for this node.
    pub fn c_code(&self, root: &Path) -> Result<String> {
        if self.skip() {
            return Ok(String::new());
        }
        let Some(range) = self.range() else {
            return Err(anyhow::anyhow!("no source range"));
        };

        let mut code = read_c_code(root, range)?;

        if code.is_empty() {
            return Ok(code);
        }

        if !self.is_static() && !self.is_inline() {
            return Ok(code);
        }

        let Some(global_name) = self.global_name() else {
            // publicizing was not performed (e.g. C2RUST_REMOVE_STATIC is
            // unset); return the original code as-is.
            return Ok(code);
        };

        let (beg, end) = name_range(self.loc().unwrap(), self.range().unwrap());
        code.replace_range(beg..end, global_name);

        strip_visibility_attrs(&mut code)?;
        code.insert_str(0, "__attribute__((visibility(\"default\"))) ");

        let re = regex::Regex::new(r"^static\s|\sstatic\s").map_err(|e| anyhow::anyhow!("{}", e))?;
        if let Some(m) = re.find(&code) {
            code.replace_range(m.start()..m.start() + m.len(), " ");
        }
        if !self.is_inline() {
            return Ok(code);
        }
        let re = regex::Regex::new(r"\s_*inline_*\s").map_err(|e| anyhow::anyhow!("{}", e))?;
        if let Some(m) = re.find(&code) {
            code.replace_range(m.start()..m.start() + m.len(), " ");
        }
        Ok(code)
    }
}

fn name_range(loc: &SourceLocation, range: &SourceRange) -> (usize, usize) {
    let (Some(name_loc), Some(beg_loc)) = (
        loc.expansion_loc.as_ref(),
        range.begin.expansion_loc.as_ref(),
    ) else {
        return (0, 0);
    };
    let offset = name_loc.offset - beg_loc.offset;
    (offset, offset + name_loc.tok_len)
}

fn strip_visibility_attrs(code: &mut String) -> Result<()> {
    let re = regex::Regex::new(
        r#"__attribute__\s*\(\s*\(\s*__visibility__\s*\(\s*"[^"]*"\s*\)\s*\)\s*\)\s*"#,
    )
    .map_err(|e| anyhow::anyhow!("{}", e))?;
    while let Some(m) = re.find(code) {
        code.replace_range(m.start()..m.end(), "");
    }
    let re = regex::Regex::new(
        r#"__attribute__\s*\(\s*\(\s*visibility\s*\(\s*"[^"]*"\s*\)\s*\)\s*\)\s*"#,
    )
    .map_err(|e| anyhow::anyhow!("{}", e))?;
    while let Some(m) = re.find(code) {
        code.replace_range(m.start()..m.end(), "");
    }
    Ok(())
}

fn ensure_extern_decl(code: &mut String) -> Result<()> {
    let has_extern = regex::Regex::new(r"(^|[^\w])extern([^\w]|$)")
        .map_err(|e| anyhow::anyhow!("{}", e))?
        .is_match(code);
    if !has_extern {
        code.insert_str(0, "extern ");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Location helpers
// ---------------------------------------------------------------------------

fn init_base_location(target: &mut BareSourceLocation, src: &BareSourceLocation) {
    if let (None, Some(_), Some(line)) = (
        target.presumed_file.as_ref(),
        src.presumed_file.as_ref(),
        src.presumed_line,
    ) {
        target.presumed_file = src.presumed_file.clone();
        target.presumed_line = Some(line + (target.line - src.line))
    }
}

fn init_loc(target: &mut SourceLocation, src: &SourceLocation) {
    if let (Some(ref mut target), Some(src)) =
        (target.spelling_loc.as_mut(), src.spelling_loc.as_ref())
    {
        init_base_location(target, src);
    }
    if let (Some(ref mut target), Some(src)) =
        (target.expansion_loc.as_mut(), src.expansion_loc.as_ref())
    {
        init_base_location(target, src);
    }
}

// ---------------------------------------------------------------------------
// File struct
// ---------------------------------------------------------------------------

pub struct File {
    node: Node,
    path: PathBuf,
    #[allow(dead_code)]
    loaded_from_json: bool,
}

impl File {
    /// Load or parse a `.c2rust` file.
    ///
    /// `root` is `<feature_root>/c`.
    /// `path` is the absolute path to the `.c2rust` file.
    pub fn new(root: &Path, path: &Path) -> Result<Self> {
        let json_file = path.with_extension("json");
        if json_file.exists() {
            if let Ok(node) = Self::load_from_json(&json_file) {
                return Ok(Self {
                    node,
                    path: path.to_path_buf(),
                    loaded_from_json: true,
                });
            }
        }
        Self::with_c_file(root, path)
    }

    fn load_from_json(json_file: &Path) -> Result<Node> {
        let content =
            fs::read_to_string(json_file).ctx(&format!("read {}", json_file.display()))?;
        let node: Node =
            serde_json::from_str(&content).ctx(&format!("parse {}", json_file.display()))?;
        Ok(node)
    }

    fn with_c_file(root: &Path, path: &Path) -> Result<Self> {
        let mut node = Self::load_by_c_file(root, path)?;
        let Kind::TranslationUnitDecl(ref mut unit) = node.kind else {
            return Err(anyhow::anyhow!("expected TranslationUnitDecl"));
        };
        unit.md5 = Some(Self::md5_file(path)?);
        Self::save_to(&node, path)?;
        Ok(Self {
            node,
            path: path.to_path_buf(),
            loaded_from_json: false,
        })
    }

    fn remove_static(root: &Path, path: &Path, mut node: Node) -> Result<Node> {
        let md5 = Self::md5_file(&path.with_extension("c2rust"))?;

        if !Self::rename_static_symbols(&mut node, &md5) {
            return Ok(node);
        }

        let new_c_file = path.with_extension("c2rust_without_static");
        Self::preprocess_c_code(root, &node.inner, path, &new_c_file)?;

        let mut new_node = Self::load_by_c_file(root, &new_c_file)?;
        let Kind::TranslationUnitDecl(ref mut unit) = new_node.kind else {
            return Err(anyhow::anyhow!("expected TranslationUnitDecl"));
        };
        unit.md5 = Some(Self::md5_file(path)?);
        Ok(new_node)
    }

    pub fn rename_static_symbols(node: &mut Node, md5: &str) -> bool {
        let mut has_static = false;
        for child in &mut node.inner {
            if !child.kind.is_static() && !child.kind.is_inline() {
                continue;
            }
            let Some(name) = child.kind.name() else {
                continue;
            };
            child
                .kind
                .set_global_name(format!("_c2rust_private_{md5}_{name}"));
            has_static = true;
        }
        has_static
    }

    fn preprocess_c_code(root: &Path, nodes: &[Node], path: &Path, output: &Path) -> Result<()> {
        let new_c2rust_file = path.with_extension("c2rust_global");
        let mut fix = false;
        loop {
            let code = Self::generate_c_code(root, nodes, false, fix)?;
            fs::write(&new_c2rust_file, code.as_bytes())
                .ctx(&format!("write {}", new_c2rust_file.display()))?;

            let output_result = Command::new(get_clang())
                .arg("-xc")
                .arg("-E")
                .arg("-C")
                .arg("-P")
                .arg("-fno-builtin")
                .arg(&new_c2rust_file)
                .arg("-o")
                .arg(output)
                .output()
                .ctx("clang preprocess")?;

            if output_result.status.success() {
                return Ok(());
            }
            if !fix {
                fix = true;
                continue;
            }
            eprintln!("{}", String::from_utf8_lossy(&output_result.stderr));
            return Err(anyhow::anyhow!("clang preprocessing failed"));
        }
    }

    fn load_by_c_file(root: &Path, path: &Path) -> Result<Node> {
        let rel_path = path
            .strip_prefix(root)
            .map_err(|_| anyhow::anyhow!("path {} not under root {}", path.display(), root.display()))?;

        let output = Command::new(get_clang())
            .current_dir(root)
            .arg("-xc")
            .arg("-Xclang")
            .arg("-ast-dump=json")
            .arg("-fsyntax-only")
            .arg(rel_path)
            .output()
            .ctx("clang ast-dump")?;

        let json = String::from_utf8_lossy(&output.stdout);
        let mut node: Node = serde_json::from_str(&json).ctx(&format!(
            "parse ast-dump json for {}",
            path.display()
        ))?;
        Self::init_line_info(&mut node);
        Self::init_vars(&mut node);
        if remove_static_enabled() {
            Self::remove_static(root, path, node)
        } else {
            Ok(node)
        }
    }

    fn md5_file(path: &Path) -> Result<String> {
        let content = fs::read_to_string(path).ctx(&format!("read {}", path.display()))?;
        let digest = md5::compute(content.as_bytes());
        Ok(format!("{:x}", digest))
    }

    fn normalize_inline_flags(node: &mut Node) {
        let mut any_inline: HashMap<String, bool> = HashMap::new();
        for child in &node.inner {
            let Kind::FunctionDecl(ref f) = child.kind else {
                continue;
            };
            let e = any_inline.entry(f.name.clone()).or_insert(false);
            if f.inline {
                *e = true;
            }
        }
        for child in &mut node.inner {
            let Kind::FunctionDecl(ref mut f) = child.kind else {
                continue;
            };
            if any_inline.get(&f.name) == Some(&true) {
                f.inline = true;
            }
        }
    }

    fn init_line_info(node: &mut Node) {
        if node.inner.is_empty() {
            return;
        }
        for n in 1..node.inner.len() {
            let src = node.inner[n - 1].kind.loc().cloned();
            let target = node.inner[n].kind.loc_mut();
            if let (Some(target), Some(src)) = (target, src) {
                init_loc(target, &src);
            }
        }

        // typedef struct Foo { } Foo – earlier node contained by later
        for n in 0..node.inner.len() {
            if node.inner[n].kind.skip() {
                continue;
            }
            let Some(range1) = node.inner[n].kind.range() else {
                continue;
            };
            for m in (n + 1)..node.inner.len() {
                let Some(range2) = node.inner[m].kind.range() else {
                    continue;
                };
                if !range_include(range2, range1) {
                    continue;
                }
                for k in n..m {
                    node.inner[k].kind.set_skip();
                }
                break;
            }
        }

        // void foo(int, ...) – later node contained by earlier
        for n in (0..node.inner.len()).rev() {
            if node.inner[n].kind.skip() {
                continue;
            }
            let Some(range1) = node.inner[n].kind.range() else {
                continue;
            };
            for m in (0..n).rev() {
                let Some(range2) = node.inner[m].kind.range() else {
                    continue;
                };
                if !range_include(range2, range1) {
                    continue;
                }
                for k in m + 1..n + 1 {
                    node.inner[k].kind.set_skip();
                }
                break;
            }
        }
        node.inner.retain(|n| !n.kind.skip());
        Self::normalize_inline_flags(node);
    }

    fn init_vars(node: &mut Node) {
        let mut inited_vars: HashMap<String, &mut VarDecl> = HashMap::new();
        for n in &mut node.inner {
            let Kind::VarDecl(ref mut var) = n.kind else {
                continue;
            };
            if var.init.is_some() {
                inited_vars.insert(var.name.clone(), var);
            } else if inited_vars.contains_key(&var.name) {
                var.is_implicit = true;
            } else {
                inited_vars.insert(var.name.clone(), var);
            }
        }
        for (_, var) in inited_vars {
            if var.storage_class.as_deref() != Some("extern") && var.init.is_none() {
                var.fake_init = true;
            }
        }
    }

    fn save_to(node: &Node, path: &Path) -> Result<()> {
        let json = serde_json::to_string(node).ctx("serialize node")?;
        let json_path = path.with_extension("json");
        fs::write(&json_path, json.as_bytes()).ctx(&format!("write {}", json_path.display()))
    }

    /// Test-only constructor.
    #[cfg(test)]
    pub fn new_for_test(node: Node, path: PathBuf) -> Self {
        Self {
            node,
            path,
            loaded_from_json: false,
        }
    }

    pub fn save_json(&self) -> Result<()> {
        Self::save_to(&self.node, &self.path)
    }

    pub fn iter(&self) -> &[Node] {
        &self.node.inner
    }

    pub fn iter_mut(&mut self) -> &mut [Node] {
        &mut self.node.inner
    }

    pub fn remove_skipped(&mut self) {
        self.node.inner.retain(|n| !n.kind.skip());
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    #[allow(dead_code)]
    pub fn loaded_from_json(&self) -> bool {
        self.loaded_from_json
    }

    #[allow(dead_code)]
    pub fn export_c_code(&self, root: &Path) -> Result<String> {
        Self::generate_c_code(root, &self.node.inner, false, false)
    }

    pub fn export_header(&self, root: &Path) -> Result<String> {
        let header = Self::generate_c_code(root, &self.node.inner, true, false)?;
        let mut new_header = "typedef float _Float32;\n".to_string();
        new_header.push_str("typedef double _Float64;\n");
        new_header.push_str("typedef double _Float32x;\n");
        new_header.push_str("typedef long double _Float64x;\n");
        new_header.push_str("typedef __float128 _Float128;\n");
        new_header.push_str(&header);
        Ok(new_header)
    }

    fn generate_c_code(
        root: &Path,
        nodes: &[Node],
        is_header: bool,
        fix: bool,
    ) -> Result<String> {
        let mut content = String::new();
        let mut last: Option<&Node> = None;
        for node in nodes {
            let end = node.kind.range();
            let code = if let Some(last) = last {
                last.kind.tail_code(root, end)?
            } else {
                read_code_between(root, None, end)?
            };
            content.push_str(&code);
            last = Some(node);

            let mut code = node.kind.c_code(root)?;
            if code.is_empty() {
                continue;
            }

            if is_header || node.kind.has_committed() {
                if let Kind::FunctionDecl(_) = node.kind {
                    if let Some(pos) = code.find('{') {
                        code.drain(pos..);
                        code.push(';');
                    }
                    ensure_extern_decl(&mut code)?;
                }
                if let Kind::VarDecl(ref var) = node.kind {
                    if let Some(pos) = code.find('=') {
                        code.drain(pos..);
                    }
                    ensure_extern_decl(&mut code)?;
                    var.ty.fill_array_size(&mut code)?;
                }
            }
            if let Some(rename) = node.kind.rename_macro() {
                if !fix {
                    content.push_str(&format!("\n{rename}\n"));
                } else {
                    content.insert_str(0, &format!("\n{rename}\n"));
                }
            }
            content.push_str(&code);
        }
        if let Some(last) = last {
            let code = last.kind.tail_code(root, None)?;
            content.push_str(&code);
        }
        remove_unused_attrs(&mut content);
        Ok(content)
    }
}

/// Return the clang binary name, honouring `C2RUST_CLANG` env var.
pub fn get_clang() -> String {
    std::env::var("C2RUST_CLANG").unwrap_or_else(|_| "clang".to_string())
}

/// Return whether the static/inline symbol publicizing step is enabled.
///
/// Disabled by default.  Set the `C2RUST_REMOVE_STATIC` environment variable
/// to any non-empty value to enable it.
pub fn remove_static_enabled() -> bool {
    std::env::var("C2RUST_REMOVE_STATIC")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod test_helpers {
    use super::*;

    pub(crate) fn make_fn_definition_node(name: &str, is_weak: bool) -> Node {
        let mut inner = vec![];
        if is_weak {
            inner.push(Node {
                id: clang_ast::Id::NULL,
                kind: Kind::Other(OtherDecl {
                    kind: Some("WeakAttr".to_string()),
                }),
                inner: vec![],
            });
        }
        inner.push(Node {
            id: clang_ast::Id::NULL,
            kind: Kind::CompoundStmt,
            inner: vec![],
        });
        Node {
            id: clang_ast::Id::NULL,
            kind: Kind::FunctionDecl(FunctionDecl {
                name: name.to_string(),
                loc: SourceLocation::default(),
                range: SourceRange::default(),
                is_definition: true,
                is_implicit: false,
                inline: false,
                ty: MyClangType {
                    qual_type: "void ()".to_string(),
                    desugared_qual_type: None,
                },
                storage_class: None,
                git_commit: false,
                global_name: None,
                skip: false,
            }),
            inner,
        }
    }

    pub(crate) fn make_translation_unit(children: Vec<Node>) -> Node {
        Node {
            id: clang_ast::Id::NULL,
            kind: Kind::TranslationUnitDecl(TranslationUnitDecl {
                md5: None,
                git_commit: false,
            }),
            inner: children,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialise tests that mutate `C2RUST_REMOVE_STATIC` so they cannot race
    /// with each other.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    fn make_var(qual_type: &str) -> Node {
        Node {
            id: clang_ast::Id::NULL,
            kind: Kind::VarDecl(VarDecl {
                name: "test_var".to_string(),
                loc: SourceLocation::default(),
                range: SourceRange::default(),
                ty: MyClangType {
                    qual_type: qual_type.to_string(),
                    desugared_qual_type: None,
                },
                storage_class: None,
                init: None,
                is_implicit: false,
                git_commit: false,
                global_name: None,
                skip: false,
                fake_init: false,
            }),
            inner: vec![],
        }
    }

    #[test]
    fn is_const_var_basic() {
        assert!(make_var("const int").kind.is_const_var());
        assert!(make_var("int const").kind.is_const_var());
        assert!(!make_var("int").kind.is_const_var());
        assert!(!make_var("const int *").kind.is_const_var());
        assert!(make_var("int * const").kind.is_const_var());
    }

    #[test]
    fn is_weak_fn_detects_attr() {
        let weak_attr = Node {
            id: clang_ast::Id::NULL,
            kind: Kind::Other(OtherDecl {
                kind: Some("WeakAttr".to_string()),
            }),
            inner: vec![],
        };
        let fn_node = Node {
            id: clang_ast::Id::NULL,
            kind: Kind::FunctionDecl(FunctionDecl {
                name: "foo".to_string(),
                loc: SourceLocation::default(),
                range: SourceRange::default(),
                is_definition: true,
                is_implicit: false,
                inline: false,
                ty: MyClangType {
                    qual_type: "void ()".to_string(),
                    desugared_qual_type: None,
                },
                storage_class: None,
                git_commit: false,
                global_name: None,
                skip: false,
            }),
            inner: vec![weak_attr],
        };
        assert!(fn_node.kind.is_weak_fn(&fn_node.inner));
    }

    #[test]
    fn remove_static_enabled_respects_env_var() {
        let _guard = ENV_MUTEX.lock().unwrap();

        // The env var is not set in the normal test environment, so the
        // default should be disabled.
        std::env::remove_var("C2RUST_REMOVE_STATIC");
        assert!(!remove_static_enabled(), "should be disabled when env var is unset");

        std::env::set_var("C2RUST_REMOVE_STATIC", "1");
        assert!(remove_static_enabled(), "should be enabled when env var is '1'");

        std::env::set_var("C2RUST_REMOVE_STATIC", "true");
        assert!(remove_static_enabled(), "should be enabled when env var is 'true'");

        std::env::set_var("C2RUST_REMOVE_STATIC", "");
        assert!(!remove_static_enabled(), "should be disabled when env var is empty string");

        // Clean up so other tests are not affected.
        std::env::remove_var("C2RUST_REMOVE_STATIC");
    }

    #[test]
    fn rename_static_symbols_works() {
        let mut root = Node {
            id: clang_ast::Id::NULL,
            kind: Kind::TranslationUnitDecl(TranslationUnitDecl {
                md5: None,
                git_commit: false,
            }),
            inner: vec![Node {
                id: clang_ast::Id::NULL,
                kind: Kind::FunctionDecl(FunctionDecl {
                    name: "priv".to_string(),
                    loc: SourceLocation::default(),
                    range: SourceRange::default(),
                    is_definition: true,
                    is_implicit: false,
                    inline: false,
                    ty: MyClangType {
                        qual_type: "void ()".to_string(),
                        desugared_qual_type: None,
                    },
                    storage_class: Some("static".to_string()),
                    git_commit: false,
                    global_name: None,
                    skip: false,
                }),
                inner: vec![],
            }],
        };
        assert!(File::rename_static_symbols(&mut root, "abc"));
        assert_eq!(
            root.inner[0].kind.global_name(),
            Some("_c2rust_private_abc_priv")
        );
    }
}
