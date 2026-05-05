//! Feature-level init split logic.
//!
//! Migrated from `c2rust-code-analyse/src/feature.rs`, retaining only the
//! functionality required for `init` (no `update`, `reinit`, `merge`, `sync`).

use crate::error::{Result, ToError};
use crate::split::file::{File, Kind, Node};
use anyhow::anyhow;
use quote::quote;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use std::sync::OnceLock;
use syn::{
    spanned::Spanned,
    visit::{visit_file, Visit},
    visit_mut::{visit_file_mut, visit_foreign_item_fn_mut, visit_item_foreign_mod_mut, VisitMut},
};
use toml_edit::{Array, DocumentMut, Item, Table};
use walkdir::WalkDir;

/// Represents a single feature (`.c2rust/<name>/`).
pub struct Feature {
    /// Absolute path to `.c2rust/<name>/`
    pub root: PathBuf,
    /// The common path prefix shared by all `.c2rust` files
    pub prefix: PathBuf,
    pub name: String,
    pub files: Vec<File>,
}

impl Feature {
    /// Create a Feature by loading all `.c2rust` files from `.c2rust/<name>/c/`.
    ///
    /// `project_root` is the directory containing `.c2rust/`.
    #[allow(dead_code)]
    pub fn new(project_root: &Path, name: &str) -> Result<Self> {
        let root = project_root.join(".c2rust").join(name);
        let prefix = Self::get_file_prefix(&root.join("c"));
        let mut this = Self {
            root,
            name: name.to_string(),
            prefix,
            files: vec![],
        };
        this.get_files()?;
        Ok(this)
    }

    /// Create a lightweight Feature for the `merge` command.
    ///
    /// Does not load any `.c2rust` files – merge operates directly on the
    /// `rust/src/` directory produced by `init`.
    pub fn new_for_merge(project_root: &Path, name: &str) -> Result<Self> {
        let root = project_root.join(".c2rust").join(name);
        if !root.exists() {
            return Err(anyhow!(
                "feature '{}' not found at {}; run init first",
                name,
                root.display()
            ));
        }
        Ok(Self {
            root,
            name: name.to_string(),
            prefix: PathBuf::new(),
            files: vec![],
        })
    }

    /// Create a Feature with a pre-filtered subset of `.c2rust` files.
    ///
    /// Only the files whose paths appear in `selected` are loaded.
    pub fn new_with_selection(
        project_root: &Path,
        name: &str,
        selected: &[PathBuf],
    ) -> Result<Self> {
        let root = project_root.join(".c2rust").join(name);
        let c_root = root.join("c");
        let prefix = Self::get_file_prefix(&c_root);
        let selected_set: HashSet<PathBuf> = selected.iter().cloned().collect();
        let mut files = Vec::new();

        // Sort for determinism
        let mut sorted: Vec<PathBuf> = selected_set.into_iter().collect();
        sorted.sort();
        for path in &sorted {
            files.push(File::new(&c_root, path)?);
        }

        let mut this = Self {
            root,
            name: name.to_string(),
            prefix,
            files,
        };
        this.skip_duplicate_weak_fns()?;
        Ok(this)
    }

    #[allow(dead_code)]
    fn get_files(&mut self) -> Result<()> {
        let c_root = self.root.join("c");
        let mut paths: Vec<PathBuf> = WalkDir::new(&c_root)
            .min_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .map(|e| e.into_path())
            .filter(|p| p.is_file() && p.extension() == Some(OsStr::new("c2rust")))
            .collect();
        paths.sort();
        for path in &paths {
            self.files.push(File::new(&c_root, path)?);
        }
        self.skip_duplicate_weak_fns()?;
        Ok(())
    }

    fn skip_duplicate_weak_fns(&mut self) -> Result<()> {
        let mut fn_counts: HashMap<String, usize> = HashMap::new();
        let mut has_non_weak: HashSet<String> = HashSet::new();
        for file in self.files.iter() {
            for node in file.iter() {
                let Kind::FunctionDecl(_) = &node.kind else {
                    continue;
                };
                if node.kind.is_fun_declare(&node.inner) {
                    continue;
                }
                let Some(name) = node.kind.name() else {
                    continue;
                };
                *fn_counts.entry(name.to_string()).or_insert(0) += 1;
                if !node.kind.is_weak_fn(&node.inner) {
                    has_non_weak.insert(name.to_string());
                }
            }
        }

        let mut skip_nodes: HashSet<(usize, usize)> = HashSet::new();
        let mut weak_seen: HashSet<String> = HashSet::new();
        for (file_idx, file) in self.files.iter().enumerate() {
            for (node_idx, node) in file.iter().iter().enumerate() {
                let Kind::FunctionDecl(_) = &node.kind else {
                    continue;
                };
                if node.kind.is_fun_declare(&node.inner) {
                    continue;
                }
                let Some(name) = node.kind.name() else {
                    continue;
                };
                let count = fn_counts.get(name).copied().unwrap_or(0);
                if count <= 1 || !node.kind.is_weak_fn(&node.inner) {
                    continue;
                }
                if has_non_weak.contains(name) {
                    skip_nodes.insert((file_idx, node_idx));
                } else {
                    if weak_seen.contains(name) {
                        skip_nodes.insert((file_idx, node_idx));
                    } else {
                        weak_seen.insert(name.to_string());
                    }
                }
            }
        }

        let mut by_file: HashMap<usize, Vec<usize>> = HashMap::new();
        for (file_idx, node_idx) in skip_nodes {
            by_file.entry(file_idx).or_default().push(node_idx);
        }
        for (file_idx, indices) in by_file {
            let file = &mut self.files[file_idx];
            let nodes = file.iter_mut();
            for idx in &indices {
                nodes[*idx].kind.set_skip();
            }
            file.remove_skipped();
            file.save_json()?;
        }
        Ok(())
    }

    fn get_file_prefix(c_root: &Path) -> PathBuf {
        let mut prefix = c_root.to_path_buf();
        while let Some(child) = Self::get_single_subdir(&prefix) {
            prefix = child;
        }
        prefix
    }

    fn get_single_subdir(path: &Path) -> Option<PathBuf> {
        let mut child = None;
        for entry in WalkDir::new(path)
            .min_depth(1)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let p = entry.path();
            if child.is_some() || !p.is_dir() {
                return None;
            }
            child = Some(p.to_path_buf());
        }
        child
    }

    fn is_node_definition(node: &Node) -> bool {
        match node.kind {
            Kind::FunctionDecl(_) => !node.kind.is_fun_declare(&node.inner),
            Kind::VarDecl(_) => !node.kind.is_extern() || node.kind.is_inited(),
            _ => false,
        }
    }

    fn prefixed_filename(node: &Node) -> Result<String> {
        let name = node.kind.name().ok_or_else(|| anyhow!("node has no name"))?;
        let name = Self::normalize_name(name);
        match node.kind {
            Kind::VarDecl(_) => Ok(format!("var_{}", name)),
            Kind::FunctionDecl(_) => Ok(format!("fun_{}", name)),
            _ => Err(anyhow!("unexpected node kind for prefixed_filename")),
        }
    }

    pub fn decl_filename(name: &str) -> String {
        let name = Self::normalize_name(name);
        format!("decl_{name}.rs")
    }

    /// Run the full init flow:
    /// 1. Back up and recreate `rust/` directory
    /// 2. Run `cargo new --lib`
    /// 3. Configure `Cargo.toml` (set crate-type)
    /// 4. Generate per-file Rust scaffolding
    /// 5. Write `rust/src/lib.rs` and `lib.normalized`
    pub fn init(&self) -> Result<()> {
        println!("Starting initialization for feature '{}'", self.name);
        let rust = self.root.join("rust");
        let rust_old = self.root.join("rust_old");
        let _ = fs::remove_dir_all(&rust_old);
        let _ = fs::rename(&rust, &rust_old);
        let _ = fs::remove_dir_all(&rust);

        println!("Creating new Rust library project...");
        let output = Command::new("cargo")
            .current_dir(&self.root)
            .arg("new")
            .arg("--lib")
            .arg("--edition")
            .arg("2021")
            .arg("rust")
            .output()
            .ctx("cargo new")?;

        if !output.status.success() {
            eprintln!("{}", String::from_utf8_lossy(&output.stderr));
            return Err(anyhow!("cargo new failed"));
        }
        println!("Rust project created");

        self.set_staticlib()?;
        println!("Crate type configured");

        self.create_file_directories()?;
        println!("Directory structure created");

        let _ = fs::remove_dir_all(rust_old);

        let lib_rs = self.root.join("rust/src/lib.rs");
        let lib_normalized = lib_rs.with_extension("normalized");
        fs::copy(&lib_rs, &lib_normalized).ctx(&format!(
            "copy {} -> {}",
            lib_rs.display(),
            lib_normalized.display()
        ))?;

        println!("Feature '{}' initialized successfully", self.name);
        Ok(())
    }

    fn set_staticlib(&self) -> Result<()> {
        let toml_path = self.root.join("rust/Cargo.toml");
        let content =
            fs::read_to_string(&toml_path).ctx(&format!("read {}", toml_path.display()))?;
        let mut doc =
            DocumentMut::from_str(&content).ctx(&format!("parse {}", toml_path.display()))?;

        let lib = doc["lib"].or_insert(Item::Table(Table::new()));
        let lib = lib.as_table_mut().ok_or_else(|| anyhow!("[lib] is not a table"))?;
        let mut crate_type = Array::new();
        crate_type.push("staticlib");
        lib.insert("crate-type", Item::Value(crate_type.into()));

        fs::write(&toml_path, doc.to_string().as_bytes())
            .ctx(&format!("write {}", toml_path.display()))
    }

    pub fn lib_attrs() -> &'static str {
        r#"// 对应__attribute__((weak))弱链接符号.
// 构建环境需要设置变量RUSTC_BOOTSTRAP=1
#![feature(linkage)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(unsafe_op_in_unsafe_fn)]
// 会生成_Float128浮点数的API，先抑制这类告警.
#![allow(improper_ctypes)]
#![allow(unused_imports)]
#![allow(dead_code)]
"#
    }

    fn add_cstddef_items(items: Vec<syn::Item>) -> Vec<syn::Item> {
        let c_stddef = r"
        use ::core::ffi::*;
        type c_size_t = usize;
        type c_ssize_t = isize;
        type c_ptrdiff_t = isize;
        type c_int8_t = i8;
        type c_int16_t = i16;
        type c_int32_t = i32;
        type c_int64_t = i64;
        type c_uint8_t = u8;
        type c_uint16_t = u16;
        type c_uint32_t = u32;
        type c_uint64_t = u64;
        type uchar = c_uchar;
        type float = c_float;
        type double = c_double;
        type int = c_int;
        type short = c_short;
        type long = c_long;
        type longlong = c_longlong;
        type uint = c_uint;
        type ushort = c_ushort;
        type ulong = c_ulong;
        type ulonglong = c_ulonglong;
        type size_t = c_size_t;
        type ssize_t = c_ssize_t;
        type ptrdiff_t = c_ptrdiff_t;
        type int8_t = c_int8_t;
        type int16_t = c_int16_t;
        type int32_t = c_int32_t;
        type int64_t = c_int64_t;
        type uint8_t = c_uint8_t;
        type uint16_t = c_uint16_t;
        type uint32_t = c_uint32_t;
        type uint64_t = c_uint64_t;
        ";
        let c_items = syn::parse_file(c_stddef).unwrap().items;
        let cstddef_names: HashSet<proc_macro2::Ident> = c_items
            .iter()
            .filter_map(|item| {
                if let syn::Item::Type(t) = item {
                    Some(t.ident.clone())
                } else {
                    None
                }
            })
            .collect();
        let mut result = c_items;
        result.extend(items.into_iter().filter(|item| {
            if let syn::Item::Type(t) = item {
                !cstddef_names.contains(&t.ident)
            } else {
                true
            }
        }));
        result
    }

    fn create_file_directories(&self) -> Result<()> {
        let mut code = "// generated by c2rust\n\n".to_string();
        code.push_str(Self::lib_attrs());
        code.push('\n');
        for file in &self.files {
            let mod_name = self.create_file_mod(file)?;
            code.push_str("mod ");
            code.push_str(&mod_name);
            code.push_str(";\n");
        }
        let lib_rs = self.root.join("rust/src/lib.rs");
        fs::write(&lib_rs, code.as_bytes()).ctx(&format!("write {}", lib_rs.display()))?;
        let lib_normalized = self.root.join("rust/src/lib.normalized");
        fs::write(&lib_normalized, code.as_bytes())
            .ctx(&format!("write {}", lib_normalized.display()))?;
        Ok(())
    }

    fn create_file_mod(&self, file: &File) -> Result<String> {
        let mod_name = Self::get_mod_name_for_file(&self.prefix, file)?;
        let mod_dir = self.root.join("rust/src").join(&mod_name);
        fs::create_dir_all(&mod_dir).ctx(&format!("create {}", mod_dir.display()))?;

        let mut nodes: HashMap<&str, &Node> = HashMap::new();
        for node in file.iter() {
            if !Self::is_node_definition(node) || node.kind.is_variadic() {
                continue;
            }
            let Some(name) = node.kind.name() else {
                continue;
            };
            nodes
                .entry(name)
                .and_modify(|old| {
                    if node.kind.is_inited() {
                        *old = node;
                    }
                })
                .or_insert(node);
        }

        println!("{mod_name}: Generating type information with bindgen...");
        self.generate_mod_rs(file, &mod_dir, &nodes)?;
        println!("{mod_name}: Type information generated");

        let mut ffi_decl = Self::get_ffi_decl(&mod_dir)?;
        let c_root = self.root.join("c");
        for (name, node) in nodes {
            let normalized_name = Self::normalize_name(name);
            let Some(decl) = ffi_decl.remove(normalized_name) else {
                continue;
            };
            let prefixed_name = Self::prefixed_filename(node)?;
            let rs_file = mod_dir.join(&prefixed_name).with_extension("rs");
            if !rs_file.exists() {
                fs::File::create(&rs_file).ctx(&format!("create {}", rs_file.display()))?;
            }
            let c_code = Self::normalize_c_code(node, &c_root)?;
            let c_file = mod_dir.join(&prefixed_name).with_extension("c");
            fs::write(&c_file, c_code.as_bytes())
                .ctx(&format!("write {}", c_file.display()))?;
            let decl_file = mod_dir.join(Self::decl_filename(name));
            let decl = if decl.contains("link_name") {
                Self::postprocess_decl(&decl)
            } else {
                decl
            };
            fs::write(&decl_file, decl).ctx(&format!("write {}", decl_file.display()))?;
        }
        Ok(mod_name)
    }

    fn append_clang_options(cmd: &mut Command, file: &File) {
        const SHORT_ENUMS: &str = "-fshort-enums";
        let opts = file.path().with_extension("c2rust.opts");
        let opts = fs::read_to_string(opts).unwrap_or_default();
        if opts.contains(SHORT_ENUMS) {
            cmd.arg(SHORT_ENUMS);
        }
    }

    pub fn generate_mod_rs(
        &self,
        file: &File,
        mod_dir: &Path,
        nodes: &HashMap<&str, &Node>,
    ) -> Result<()> {
        let types_h = mod_dir.join("types.h");
        let header = file.export_header(&self.root.join("c"))?;
        fs::write(&types_h, header.as_bytes()).ctx(&format!("write {}", types_h.display()))?;

        let mut cmd = Command::new("bindgen");
        cmd.current_dir(mod_dir)
            .arg(&types_h)
            .arg("-o")
            .arg("mod.rs")
            .arg("--no-layout-tests")
            .arg("--default-enum-style")
            .arg("consts")
            .arg("--no-prepend-enum-name")
            .arg("--disable-nested-struct-naming")
            .arg("--ctypes-prefix")
            .arg("::core::ffi")
            .arg("--")
            .arg("-fno-builtin")
            .arg("-xc")
            .arg("-Wno-duplicate-decl-specifier")
            .arg("-Wno-attributes");

        Self::append_clang_options(&mut cmd, file);

        let output = cmd.output().ctx("bindgen")?;
        if !output.status.success() {
            eprintln!("{}", String::from_utf8_lossy(&output.stderr));
            return Err(anyhow!("bindgen failed"));
        }
        self.normalize_mod_rs(mod_dir, nodes)
    }

    fn normalize_mod_rs(&self, mod_dir: &Path, nodes: &HashMap<&str, &Node>) -> Result<()> {
        let mod_rs = mod_dir.join("mod.rs");
        let content =
            fs::read_to_string(&mod_rs).ctx(&format!("read {}", mod_rs.display()))?;
        let mut ast =
            syn::parse_file(&content).ctx(&format!("parse {}", mod_rs.display()))?;
        ast.items = Self::add_cstddef_items(ast.items);
        let use_super: syn::Item =
            syn::parse_str("#[allow(unused_imports)]\nuse super::*;").unwrap();
        ast.items.insert(0, use_super);

        struct Visitor<'a>(&'a HashMap<&'a str, &'a Node>);

        impl Visitor<'_> {
            fn foreign_item_attrs(name: &str) -> Vec<syn::Attribute> {
                use syn::parse::Parser as SynParser;
                syn::Attribute::parse_outer
                    .parse_str(&format!("#[allow(warnings)]\n#[link_name = \"{}\"]", name))
                    .unwrap()
            }

            fn normalize_item_const(&mut self, item: &mut syn::Item) {
                let syn::Item::Const(c) = item else {
                    return;
                };
                let name = c.ident.to_string();
                let Some(node) = self.0.get(name.as_str()) else {
                    self.visit_item_const_mut(c);
                    return;
                };
                if node.kind.is_const_var() {
                    self.visit_item_const_mut(c);
                    return;
                }
                let ty = &c.ty;
                let ident = &c.ident;
                *item =
                    syn::parse2(quote!(unsafe extern "C" { static mut #ident: #ty; })).unwrap();
                self.visit_item_mut(item);
            }
        }

        impl VisitMut for Visitor<'_> {
            fn visit_type_reference_mut(&mut self, refer: &mut syn::TypeReference) {
                refer.lifetime = Some(syn::parse2(quote!('static)).unwrap());
            }

            fn visit_item_mut(&mut self, item: &mut syn::Item) {
                match item {
                    syn::Item::ForeignMod(m) => visit_item_foreign_mod_mut(self, m),
                    syn::Item::Const(_) => self.normalize_item_const(item),
                    syn::Item::Use(item) => item.vis = syn::Visibility::Inherited,
                    _ => {}
                }
            }

            fn visit_item_const_mut(&mut self, item: &mut syn::ItemConst) {
                let name = item.ident.to_string();
                if name.starts_with("_c2rust_private_") {
                    let new_name = name.splitn(5, '_').last().unwrap();
                    item.ident = syn::Ident::new(new_name, item.ident.span());
                }
                self.visit_type_mut(&mut item.ty);
            }

            fn visit_foreign_item_static_mut(&mut self, item: &mut syn::ForeignItemStatic) {
                let name = item.ident.to_string();
                item.attrs = Self::foreign_item_attrs(&name);
                if name.starts_with("_c2rust_private_") {
                    let new_name = name.splitn(5, '_').last().unwrap();
                    item.ident = syn::Ident::new(new_name, item.ident.span());
                }
                self.visit_type_mut(&mut item.ty);
            }

            fn visit_foreign_item_fn_mut(&mut self, item: &mut syn::ForeignItemFn) {
                let name = item.sig.ident.to_string();
                item.attrs = Self::foreign_item_attrs(&name);
                if name.starts_with("_c2rust_private_") {
                    let new_name = name.splitn(5, '_').last().unwrap();
                    item.sig.ident = syn::Ident::new(new_name, item.sig.ident.span());
                }
                if self.0.contains_key(name.as_str()) {
                    visit_foreign_item_fn_mut(self, item);
                }
            }

            fn visit_fn_arg_mut(&mut self, arg: &mut syn::FnArg) {
                let syn::FnArg::Typed(arg) = arg else {
                    return;
                };
                Feature::normalize_type(&mut arg.ty);
            }
        }

        let mut visitor = Visitor(nodes);
        visit_file_mut(&mut visitor, &mut ast);

        let formatted = prettyplease::unparse(&ast);
        fs::write(&mod_rs, formatted.as_bytes())
            .ctx(&format!("write {}", mod_rs.display()))?;
        let normalized_rs = mod_rs.with_extension("normalized");
        fs::copy(&mod_rs, &normalized_rs).ctx(&format!(
            "copy {} -> {}",
            mod_rs.display(),
            normalized_rs.display()
        ))?;
        Ok(())
    }

    fn normalize_c_code(node: &Node, c_root: &Path) -> Result<String> {
        let mut code = node.kind.c_code(c_root)?;
        let regex = regex::Regex::new("_c2rust_private_[^_]+_").unwrap();
        let mut off = 0;
        while let Some(m) = regex.find(&code[off..]) {
            off += m.start();
            code.replace_range(off..off + m.len(), "");
        }
        if node.kind.is_fake_inited() {
            code.push_str(" = { 0 }");
        }
        Ok(code)
    }

    fn normalize_name(name: &str) -> &str {
        if name.starts_with("_c2rust_private_") {
            return name.splitn(5, '_').last().unwrap();
        }
        name
    }

    fn normalize_type(ty: &mut syn::Type) {
        let syn::Type::Ptr(ref mut ptr) = ty else {
            return;
        };
        let inner = &mut *ptr.elem;
        if ptr.const_token.is_some() {
            *ty = syn::parse2(quote!(Option<& #inner>)).unwrap();
        } else {
            *ty = syn::parse2(quote!(Option<&mut #inner>)).unwrap();
        }
    }

    fn get_ffi_decl(mod_dir: &Path) -> Result<HashMap<String, String>> {
        let normalized_rs = mod_dir.join("mod.normalized");
        let content = fs::read_to_string(&normalized_rs)
            .ctx(&format!("read {}", normalized_rs.display()))?;
        let ast =
            syn::parse_file(&content).ctx(&format!("parse {}", normalized_rs.display()))?;

        struct Visitor<'a>(HashMap<String, String>, &'a str);
        let mut visitor = Visitor(HashMap::new(), &content);

        impl Visit<'_> for Visitor<'_> {
            fn visit_item_foreign_mod(&mut self, m: &syn::ItemForeignMod) {
                for item in &m.items {
                    if let syn::ForeignItem::Fn(ref item) = item {
                        let name = item.sig.ident.to_string();
                        let range = item.sig.span().byte_range();
                        self.0.insert(name, self.1[range].to_string());
                    } else if let syn::ForeignItem::Static(ref item) = item {
                        let name = item.ident.to_string();
                        let range = item.span().byte_range();
                        self.0.insert(name, self.1[range].to_string());
                    }
                }
            }
        }
        visit_file(&mut visitor, &ast);
        Ok(visitor.0)
    }

    fn postprocess_decl(content: &str) -> String {
        static RE: OnceLock<Regex> = OnceLock::new();
        let re = RE.get_or_init(|| Regex::new(r"(#\[\s*)link_name\b(\s*=)").unwrap());
        re.replace_all(content, "${1}export_name$2").into_owned()
    }

    pub fn get_mod_name_for_file(prefix: &Path, file: &File) -> Result<String> {
        let rel_path = file
            .path()
            .strip_prefix(prefix)
            .map_err(|_| anyhow!("path {} not under prefix {}", file.path().display(), prefix.display()))?
            .with_extension("");
        Ok("mod_".to_string()
            + &rel_path
                .display()
                .to_string()
                .replace(|c: char| !matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '_'), "_"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::split::file::test_helpers::{make_fn_definition_node, make_translation_unit};
    use tempfile::TempDir;

    #[allow(dead_code)]
    fn make_feature_for_test(tmp: &TempDir, nodes: Vec<Node>) -> Feature {
        let root = tmp.path();
        let c_root = root.join(".c2rust/default/c");
        std::fs::create_dir_all(&c_root).unwrap();

        // Write a dummy .c2rust file
        let c2rust_path = c_root.join("test.c2rust");
        std::fs::write(&c2rust_path, "int foo(void) {}").unwrap();

        // Build a File from the translation unit
        let tu = make_translation_unit(nodes);
        let file = crate::split::file::File::new_for_test(tu, c2rust_path);

        let prefix = c_root.clone();
        Feature {
            root: root.join(".c2rust/default"),
            name: "default".to_string(),
            prefix,
            files: vec![file],
        }
    }

    #[test]
    fn get_mod_name_replaces_special_chars() {
        let tmp = TempDir::new().unwrap();
        let c_root = tmp.path().join("c");
        std::fs::create_dir_all(&c_root).unwrap();
        let path = c_root.join("src/foo.c2rust");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "").unwrap();
        let file = crate::split::file::File::new_for_test(
            make_translation_unit(vec![]),
            path,
        );
        let name = Feature::get_mod_name_for_file(&c_root, &file).unwrap();
        // "src/foo" -> "src_foo" -> "mod_src_foo"
        assert!(name.starts_with("mod_"));
        assert!(!name.contains('/'));
    }

    #[test]
    fn skip_duplicate_weak_fns_keeps_strong() {
        let tmp = TempDir::new().unwrap();
        let c_root = tmp.path().join(".c2rust/default/c");
        std::fs::create_dir_all(&c_root).unwrap();

        let strong = make_fn_definition_node("my_fn", false);
        let weak = make_fn_definition_node("my_fn", true);
        let tu = make_translation_unit(vec![strong, weak]);

        let path = c_root.join("test.c2rust");
        std::fs::write(&path, "").unwrap();
        let file = crate::split::file::File::new_for_test(tu, path);

        let mut feat = Feature {
            root: tmp.path().join(".c2rust/default"),
            name: "default".to_string(),
            prefix: c_root.clone(),
            files: vec![file],
        };
        feat.skip_duplicate_weak_fns().unwrap();

        // After dedup, both entries for my_fn should remain but weak should be
        // skipped; since remove_skipped is called, only the strong one remains
        let file = &feat.files[0];
        let fns: Vec<_> = file
            .iter()
            .iter()
            .filter(|n| n.kind.name() == Some("my_fn"))
            .collect();
        assert_eq!(fns.len(), 1);
        assert!(!fns[0].kind.is_weak_fn(&fns[0].inner));
    }
}
