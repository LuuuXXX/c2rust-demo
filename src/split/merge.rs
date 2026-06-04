//! Merge logic for c2rust-demo.
//!
//! Minimal implementation: consolidates per-symbol Rust files into
//! per-module files and deduplicates shared FFI declarations into lib.rs.
//!
//! Key adaptation: module discovery uses directory scanning for `fun_*.rs`
//! and `var_*.rs` files instead of parsing `mod.rs` for `mod fun_*;`
//! declarations, because the c2rust-demo init flow does NOT inject those
//! declarations.
//!
//! The merged output is first written to `rust/src.2/`.  After a successful
//! merge, the original `rust/src` is moved to `rust/src.1` (preserving the
//! init output as a backup), and `rust/src` becomes a symlink to `rust/src.2`.

use crate::error::{Result, ToError};
use crate::split::feature::Feature;
use quote::quote;
use quote::ToTokens;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

// ---------------------------------------------------------------------------
// Report data structures
// ---------------------------------------------------------------------------

/// Per-module summary collected during merge, used to write the merge report.
pub(crate) struct MergeModSummary {
    mod_name: String,
    /// Names of Rust functions (derived from `fun_*.rs` symbol files).
    fn_names: Vec<String>,
    /// Names of Rust variables/statics (derived from `var_*.rs` symbol files).
    var_names: Vec<String>,
    /// FFI item names still local to this module after deduplication.
    /// Populated after `deduplicate_into_lib_rs` removes shared items.
    ffi_names: Vec<String>,
    /// Source files that contributed to the merged module.
    source_files: Vec<String>,
}

// ---------------------------------------------------------------------------
// impl Feature – merge methods
// ---------------------------------------------------------------------------

impl Feature {
    /// Merge the init output under `.c2rust/<feature>/rust/src/` into
    /// `.c2rust/<feature>/rust/src.2/`.
    ///
    /// After a successful merge the original `rust/src` is moved to
    /// `rust/src.1` (backup of the init output) and `rust/src` becomes a
    /// symlink pointing to `rust/src.2`.
    pub fn merge(&self) -> Result<()> {
        println!("Starting merge for feature '{}'", self.name);

        let src_dir = self.root.join("rust/src");
        if !src_dir.exists() {
            return Err(anyhow::anyhow!(
                "source directory {} does not exist; run init first",
                src_dir.display()
            ));
        }

        let mod_names = Self::scan_src_mod_dirs(&src_dir)?;
        if mod_names.is_empty() {
            println!(
                "No mod_* directories found under {}; nothing to merge.",
                src_dir.display()
            );
            return Ok(());
        }

        let mut mod_summaries: Vec<MergeModSummary> = Vec::new();
        for mod_name in &mod_names {
            if let Some(summary) = self.merge_mod_dir(mod_name)? {
                mod_summaries.push(summary);
            }
        }

        let shared_ffi_names = self.deduplicate_into_lib_rs(&mod_names)?;

        // Remove shared FFI from each module's local FFI list
        let shared_ffi_set: HashSet<&str> =
            shared_ffi_names.iter().map(String::as_str).collect();
        for summary in &mut mod_summaries {
            summary.ffi_names.retain(|n| !shared_ffi_set.contains(n.as_str()));
        }

        self.link_src()?;
        self.write_merge_report(&mod_summaries, &shared_ffi_names)?;

        println!("Feature '{}' merged successfully", self.name);
        Ok(())
    }

    /// After a successful merge, back up the original `rust/src` as
    /// `rust/src.1` and make `rust/src` a symlink to `rust/src.2`.
    ///
    /// If `rust/src` is already a symlink (e.g. from a previous merge run),
    /// the old symlink is simply removed; `rust/src.1` is left in place.
    fn link_src(&self) -> Result<()> {
        let src = self.root.join("rust/src");

        if src.is_symlink() {
            fs::remove_file(&src)
                .ctx(&format!("remove symlink {}", src.display()))?;
        } else {
            let old_src = self.root.join("rust/src.1");
            let _ = fs::remove_dir_all(&old_src);
            fs::rename(&src, &old_src).ctx(&format!(
                "rename {} -> {}",
                src.display(),
                old_src.display()
            ))?;
        }

        let new_src = self.root.join("rust/src.2");
        std::os::unix::fs::symlink(&new_src, &src).ctx(&format!(
            "symlink {} -> {}",
            src.display(),
            new_src.display()
        ))?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Directory discovery
    // -----------------------------------------------------------------------

    /// Scan `rust/src/` for `mod_*` subdirectories and return their names
    /// in sorted order.
    pub(crate) fn scan_src_mod_dirs(src_dir: &Path) -> Result<Vec<String>> {
        let mut names: Vec<String> = WalkDir::new(src_dir)
            .min_depth(1)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path().is_dir()
                    && e.path()
                        .file_name()
                        .map(|n| n.to_string_lossy().starts_with("mod_"))
                        .unwrap_or(false)
            })
            .map(|e| {
                e.path()
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        Ok(names)
    }

    /// Discover symbol modules by scanning a `mod_xxx/` directory for
    /// `fun_*.rs` and `var_*.rs` files.
    ///
    /// **Key adaptation from `c2rust-code-analyse`**: uses directory scanning
    /// instead of parsing `mod.rs` for `mod fun_*;` declarations, because
    /// the c2rust-demo `init` output does NOT write those declarations.
    pub(crate) fn discover_symbol_modules(mod_dir: &Path) -> Result<Vec<String>> {
        let mut modules: Vec<String> = WalkDir::new(mod_dir)
            .min_depth(1)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                if !e.path().is_file() {
                    return false;
                }
                if e.path().extension().map(|x| x != "rs").unwrap_or(true) {
                    return false;
                }
                let stem = e
                    .path()
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                stem.starts_with("fun_") || stem.starts_with("var_")
            })
            .map(|e| {
                e.path()
                    .file_stem()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        modules.sort();
        Ok(modules)
    }

    // -----------------------------------------------------------------------
    // Per-module merge
    // -----------------------------------------------------------------------

    /// Merge all symbol files in a `mod_xxx` directory into
    /// `rust/src.2/mod_xxx.rs`.
    ///
    /// The merged file contains:
    /// - `use super::*;`
    /// - All items from `mod.rs` (types, FFI declarations)
    /// - All items from each `fun_*.rs` / `var_*.rs` symbol file
    fn merge_mod_dir(&self, mod_name: &str) -> Result<Option<MergeModSummary>> {
        let src_dir = self.root.join("rust/src");
        let mod_dir = src_dir.join(mod_name);

        if !mod_dir.exists() {
            return Ok(None);
        }

        println!("Processing mod for merge: {}", mod_name);

        let module_names = Self::discover_symbol_modules(&mod_dir)?;

        println!(
            "Merging {} symbol file(s) for: {} ...",
            module_names.len(),
            mod_name
        );

        let mut merged_items: Vec<syn::Item> = Vec::new();

        // Always start with `use super::*;`
        merged_items.push(syn::parse2(quote! { use super::*; }).unwrap());

        // Include all items from mod.rs (type definitions, FFI blocks, etc.)
        let mod_rs = mod_dir.join("mod.rs");
        if mod_rs.exists() {
            let content =
                fs::read_to_string(&mod_rs).ctx(&format!("read {}", mod_rs.display()))?;
            if !content.trim().is_empty() {
                let ast =
                    syn::parse_file(&content).ctx(&format!("parse {}", mod_rs.display()))?;
                for item in ast.items {
                    // Skip `use super::*;` — we already added it above
                    if let syn::Item::Use(ref u) = item {
                        if Self::is_use_super(u) {
                            continue;
                        }
                    }
                    merged_items.push(item);
                }
            }
        }

        // Append all items from each symbol file
        for module_name in &module_names {
            let rs_file = mod_dir.join(module_name).with_extension("rs");
            Self::collect_symbol_items(&rs_file, &mut merged_items)?;
        }

        // Collect FFI names from merged items (before deduplication)
        let mut ffi_names: Vec<String> = Vec::new();
        for item in &merged_items {
            if let syn::Item::ForeignMod(fm) = item {
                for ffi_item in &fm.items {
                    let name = Self::ffi_name(ffi_item);
                    if !name.is_empty() {
                        ffi_names.push(name);
                    }
                }
            }
        }

        let merged_file = syn::File {
            shebang: None,
            attrs: Vec::new(),
            items: merged_items,
        };
        let formatted = prettyplease::unparse(&merged_file);

        let src2_dir = self.root.join("rust/src.2");
        fs::create_dir_all(&src2_dir).ctx(&format!("create {}", src2_dir.display()))?;
        let merged_rs = src2_dir.join(mod_name).with_extension("rs");
        fs::write(&merged_rs, formatted.as_bytes())
            .ctx(&format!("write {}", merged_rs.display()))?;

        println!("File merged successfully: {}", merged_rs.display());

        // Build the summary for the report
        let fn_names: Vec<String> = module_names
            .iter()
            .filter(|n| n.starts_with("fun_"))
            .map(|n| n[4..].to_string())
            .collect();
        let var_names: Vec<String> = module_names
            .iter()
            .filter(|n| n.starts_with("var_"))
            .map(|n| n[4..].to_string())
            .collect();
        let mut source_files = vec!["mod.rs".to_string()];
        source_files.extend(module_names.iter().map(|n| format!("{n}.rs")));

        Ok(Some(MergeModSummary {
            mod_name: mod_name.to_string(),
            fn_names,
            var_names,
            ffi_names,
            source_files,
        }))
    }

    /// Read all items from a symbol file (`fun_*.rs` / `var_*.rs`) and
    /// append them to `items`, skipping `use super::*;` imports.
    ///
    /// Empty files are silently skipped.
    fn collect_symbol_items(rs_file: &Path, items: &mut Vec<syn::Item>) -> Result<()> {
        let content =
            fs::read_to_string(rs_file).ctx(&format!("read {}", rs_file.display()))?;
        if content.trim().is_empty() {
            return Ok(());
        }
        let ast = syn::parse_file(&content).ctx(&format!("parse {}", rs_file.display()))?;
        for item in ast.items {
            if let syn::Item::Use(ref u) = item {
                if Self::is_use_super(u) {
                    continue;
                }
            }
            items.push(item);
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // FFI deduplication across modules → lib.rs
    // -----------------------------------------------------------------------

    /// Collect FFI declarations from all merged `mod_xxx.rs` files,
    /// extract any that appear in more than one file into `src.2/lib.rs`,
    /// and remove them from the individual module files.
    ///
    /// Returns the names of FFI items that were extracted (shared across modules).
    fn deduplicate_into_lib_rs(&self, mod_names: &[String]) -> Result<Vec<String>> {
        let src_2 = self.root.join("rust/src.2");
        if !src_2.exists() {
            return Ok(Vec::new());
        }

        let mod_files = Self::collect_mod_files(&src_2)?;
        if mod_files.is_empty() {
            return Ok(Vec::new());
        }

        // Collect all FFI items across mod files
        let mut ffi_by_name: HashMap<String, Vec<syn::ForeignItem>> = HashMap::new();
        let mut foreign_mod_template: Option<syn::ItemForeignMod> = None;

        for mod_file in &mod_files {
            let (ffi_items, template) = Self::ffi_items_from_file(mod_file)?;
            if foreign_mod_template.is_none() {
                foreign_mod_template = template;
            }
            for (name, item) in ffi_items {
                ffi_by_name.entry(name).or_default().push(item);
            }
        }

        // Identify duplicates (same FFI name in multiple modules)
        let mut ffi_to_extract: Vec<syn::ForeignItem> = Vec::new();
        let mut ffi_remove_set: HashSet<String> = HashSet::new();
        for (name, items) in &ffi_by_name {
            if items.len() > 1 {
                ffi_to_extract.push(items[0].clone());
                ffi_remove_set.insert(name.clone());
            }
        }

        // Write src.2/lib.rs
        Self::write_merged_lib_rs(
            &src_2,
            mod_names,
            &ffi_to_extract,
            &foreign_mod_template,
        )?;

        // Remove extracted FFI from individual module files
        if !ffi_remove_set.is_empty() {
            Self::remove_ffi_from_files(&mod_files, &ffi_remove_set)?;
        }

        println!(
            "Deduplicated {} FFI declaration(s) to lib.rs",
            ffi_remove_set.len()
        );

        let mut shared: Vec<String> = ffi_remove_set.into_iter().collect();
        shared.sort();
        Ok(shared)
    }

    /// Collect all mod_*.rs files from a directory.
    fn collect_mod_files(src_2: &Path) -> Result<Vec<PathBuf>> {
        let mut files: Vec<PathBuf> = WalkDir::new(src_2)
            .min_depth(1)
            .max_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path().is_file()
                    && e.path().extension().map(|ext| ext == "rs").unwrap_or(false)
                    && e.path()
                        .file_name()
                        .map(|n| n.to_string_lossy().starts_with("mod_"))
                        .unwrap_or(false)
            })
            .map(|e| e.path().to_path_buf())
            .collect();
        files.sort();
        Ok(files)
    }

    /// Read all `extern "C"` items from a file, returning them as
    /// `(canonical_name, item)` pairs and an optional template block.
    fn ffi_items_from_file(
        path: &Path,
    ) -> Result<(Vec<(String, syn::ForeignItem)>, Option<syn::ItemForeignMod>)> {
        let content = fs::read_to_string(path).ctx(&format!("read {}", path.display()))?;
        let ast = syn::parse_file(&content).ctx(&format!("parse {}", path.display()))?;

        let mut result = Vec::new();
        let mut template: Option<syn::ItemForeignMod> = None;

        for item in ast.items {
            if let syn::Item::ForeignMod(fm) = item {
                if template.is_none() {
                    let mut t = fm.clone();
                    t.items.clear();
                    template = Some(t);
                }
                for ffi_item in fm.items {
                    let name = Self::ffi_name(&ffi_item);
                    if !name.is_empty() {
                        result.push((name, ffi_item));
                    }
                }
            }
        }
        Ok((result, template))
    }

    /// Write `src.2/lib.rs` with module declarations for every merged module
    /// and the deduplicated FFI block (if any).
    fn write_merged_lib_rs(
        src_2: &Path,
        mod_names: &[String],
        ffi_to_extract: &[syn::ForeignItem],
        foreign_mod_template: &Option<syn::ItemForeignMod>,
    ) -> Result<()> {
        // Base: read original lib.rs (from rust/src/) for the lib-level attrs
        let lib_rs_file = src_2.parent().unwrap().join("src/lib.rs");
        let content =
            fs::read_to_string(&lib_rs_file).ctx(&format!("read {}", lib_rs_file.display()))?;
        let mut lib_rs =
            syn::parse_file(&content).ctx(&format!("parse {}", lib_rs_file.display()))?;
        let lib_items = &mut lib_rs.items;

        // Strip old `mod <name>;` declarations – we'll regenerate them
        lib_items.retain(|item| {
            if let syn::Item::Mod(m) = item {
                return m.content.is_some(); // keep inline mods, strip bare decls
            }
            true
        });

        // Ensure `use ::core::ffi::*;` is present
        if !lib_items.iter().any(Self::is_ffi_glob_import) {
            let use_ffi: syn::Item = syn::parse_str("use ::core::ffi::*;")
                .ctx("parse use ::core::ffi::*")?;
            lib_items.push(use_ffi);
        }

        // Add a `mod <name>;` declaration for each merged module
        for mod_name in mod_names {
            let mod_decl: syn::Item = syn::parse_str(&format!("pub mod {mod_name};"))
                .ctx(&format!("parse mod {mod_name}"))?;
            lib_items.push(mod_decl);
        }

        // Append the deduplicated FFI block (if any)
        if !ffi_to_extract.is_empty() {
            if let Some(mut fm) = foreign_mod_template.clone() {
                fm.items = ffi_to_extract.to_vec();
                lib_items.push(syn::Item::ForeignMod(fm));
            }
        }

        let lib_content = prettyplease::unparse(&lib_rs);
        let lib_rs_path = src_2.join("lib.rs");
        fs::write(&lib_rs_path, lib_content.as_bytes())
            .ctx(&format!("write {}", lib_rs_path.display()))?;

        Ok(())
    }

    /// Remove FFI items whose canonical names are in `ffi_remove_set` from
    /// each of the given module files.
    fn remove_ffi_from_files(mod_files: &[PathBuf], ffi_remove_set: &HashSet<String>) -> Result<()> {
        for mod_file in mod_files {
            let content =
                fs::read_to_string(mod_file).ctx(&format!("read {}", mod_file.display()))?;
            let mut ast =
                syn::parse_file(&content).ctx(&format!("parse {}", mod_file.display()))?;

            ast.items.retain_mut(|item| {
                if let syn::Item::ForeignMod(fm) = item {
                    fm.items
                        .retain(|ffi| !ffi_remove_set.contains(&Self::ffi_name(ffi)));
                    return !fm.items.is_empty();
                }
                true
            });

            let formatted = prettyplease::unparse(&ast);
            fs::write(mod_file, formatted.as_bytes())
                .ctx(&format!("write {}", mod_file.display()))?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Small item helpers
    // -----------------------------------------------------------------------

    #[cfg(test)]
    pub(crate) fn item_name(item: &syn::Item) -> Option<String> {
        match item {
            syn::Item::Struct(item) => Some(item.ident.to_string()),
            syn::Item::Union(item) => Some(item.ident.to_string()),
            syn::Item::Const(item) => Some(item.ident.to_string()),
            syn::Item::Type(item) => Some(item.ident.to_string()),
            syn::Item::Fn(item) => Some(item.sig.ident.to_string()),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn foreign_item_name(item: &syn::ForeignItem) -> Option<String> {
        match item {
            syn::ForeignItem::Fn(item) => Some(item.sig.ident.to_string()),
            syn::ForeignItem::Static(item) => Some(item.ident.to_string()),
            _ => None,
        }
    }

    fn ffi_name(item: &syn::ForeignItem) -> String {
        match item {
            syn::ForeignItem::Fn(f) => {
                Self::extract_link_name(&f.attrs).unwrap_or_else(|| f.sig.ident.to_string())
            }
            syn::ForeignItem::Static(s) => {
                Self::extract_link_name(&s.attrs).unwrap_or_else(|| s.ident.to_string())
            }
            _ => String::new(),
        }
    }

    fn extract_link_name(attrs: &[syn::Attribute]) -> Option<String> {
        for attr in attrs {
            let attr_str = attr.to_token_stream().to_string();
            if attr_str.contains("link_name") {
                if let Some(start) = attr_str.find("link_name") {
                    let rest = &attr_str[start..];
                    if let Some(quote_start) = rest.find('"') {
                        let rest = &rest[quote_start + 1..];
                        if let Some(quote_end) = rest.find('"') {
                            return Some(rest[..quote_end].to_string());
                        }
                    }
                }
            }
        }
        None
    }

    fn is_use_super(item_use: &syn::ItemUse) -> bool {
        if item_use.leading_colon.is_some() {
            return false;
        }
        if let syn::UseTree::Path(ref path) = item_use.tree {
            return path.ident == "super" && matches!(&*path.tree, syn::UseTree::Glob(_));
        }
        false
    }

    /// Returns true if `item` is a glob import of `core::ffi`, `::core::ffi`,
    /// or `std::ffi` (e.g. `use core::ffi::*;`).
    fn is_ffi_glob_import(item: &syn::Item) -> bool {
        let syn::Item::Use(item_use) = item else {
            return false;
        };
        let syn::UseTree::Path(root) = &item_use.tree else {
            return false;
        };
        let crate_name = root.ident.to_string();
        if crate_name != "core" && crate_name != "std" {
            return false;
        }
        let syn::UseTree::Path(ffi_seg) = root.tree.as_ref() else {
            return false;
        };
        if ffi_seg.ident != "ffi" {
            return false;
        }
        matches!(ffi_seg.tree.as_ref(), syn::UseTree::Glob(_))
    }

    // -----------------------------------------------------------------------
    // Report generation
    // -----------------------------------------------------------------------

    /// Write `.c2rust/<feature>/meta/merge-interface-report.md`.
    ///
    /// This is the primary user-facing artifact produced by `merge`.  It
    /// summarises the final merged layout: shared FFI hoisted to `lib.rs`,
    /// per-module functions, variables, local FFI, and source files.
    pub(crate) fn write_merge_report(
        &self,
        mod_summaries: &[MergeModSummary],
        shared_ffi_names: &[String],
    ) -> Result<()> {
        let meta_dir = self.root.join("meta");
        fs::create_dir_all(&meta_dir).ctx(&format!("create {}", meta_dir.display()))?;

        let mut out = String::new();
        out.push_str(&format!(
            "# Merge Interface Report — feature `{}`\n\n",
            self.name
        ));
        out.push_str(
            "Generated by **c2rust-demo merge**.  \
             This is the primary interface checklist for the final merged output.\n\n---\n\n",
        );

        // Summary table
        let total_fns: usize = mod_summaries.iter().map(|s| s.fn_names.len()).sum();
        let total_vars: usize = mod_summaries.iter().map(|s| s.var_names.len()).sum();
        let total_local_ffi: usize = mod_summaries.iter().map(|s| s.ffi_names.len()).sum();
        out.push_str("## Summary\n\n");
        out.push_str(&format!(
            "| Item | Count |\n|------|-------|\n\
             | Merged modules | {} |\n\
             | Total functions | {} |\n\
             | Total variables | {} |\n\
             | Module-local FFI items | {} |\n\
             | Shared FFI moved to `lib.rs` | {} |\n\n---\n",
            mod_summaries.len(),
            total_fns,
            total_vars,
            total_local_ffi,
            shared_ffi_names.len(),
        ));

        // Shared FFI section
        out.push_str("\n## `lib.rs` — Shared FFI\n\n");
        if shared_ffi_names.is_empty() {
            out.push_str("*(no shared FFI; all declarations are module-local)*\n");
        } else {
            out.push_str("The following FFI declarations appeared in more than one module and were deduplicated into `lib.rs`:\n\n");
            for name in shared_ffi_names {
                out.push_str(&format!("- `{name}`\n"));
            }
        }
        out.push_str("\n---\n");

        // Per-module sections
        for summary in mod_summaries {
            out.push_str(&format!("\n## {}\n\n", summary.mod_name));

            out.push_str("### Final Rust functions\n\n");
            if summary.fn_names.is_empty() {
                out.push_str("*(none)*\n");
            } else {
                for name in &summary.fn_names {
                    out.push_str(&format!("- `{name}`\n"));
                }
            }

            out.push_str("\n### Final Rust variables\n\n");
            if summary.var_names.is_empty() {
                out.push_str("*(none)*\n");
            } else {
                for name in &summary.var_names {
                    out.push_str(&format!("- `{name}`\n"));
                }
            }

            out.push_str("\n### Module-local FFI\n\n");
            if summary.ffi_names.is_empty() {
                out.push_str("*(none)*\n");
            } else {
                for name in &summary.ffi_names {
                    out.push_str(&format!("- `{name}`\n"));
                }
            }

            out.push_str("\n### Source files merged\n\n");
            for src_file in &summary.source_files {
                out.push_str(&format!("- `{}/{}`\n", summary.mod_name, src_file));
            }
            out.push('\n');
        }

        let report_path = meta_dir.join("merge-interface-report.md");
        fs::write(&report_path, out.as_bytes())
            .ctx(&format!("write {}", report_path.display()))?;
        println!(
            "Merge interface report: {}",
            report_path.display()
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::split::feature::Feature;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_merge_feature(tmp: &TempDir) -> Feature {
        Feature {
            root: tmp.path().join(".c2rust/default"),
            name: "default".to_string(),
            prefix: PathBuf::new(),
            files: vec![],
        }
    }

    // -----------------------------------------------------------------------
    // discover_symbol_modules
    // -----------------------------------------------------------------------

    #[test]
    fn discover_symbol_modules_finds_fun_and_var() {
        let tmp = TempDir::new().unwrap();
        let mod_dir = tmp.path().join("mod_foo");
        fs::create_dir_all(&mod_dir).unwrap();

        fs::write(mod_dir.join("fun_add.rs"), "").unwrap();
        fs::write(mod_dir.join("fun_sub.rs"), "").unwrap();
        fs::write(mod_dir.join("var_counter.rs"), "").unwrap();

        // Non-symbol files should be ignored
        fs::write(mod_dir.join("mod.rs"), "").unwrap();
        fs::write(mod_dir.join("decl_add.rs"), "").unwrap();
        fs::write(mod_dir.join("types.h"), "").unwrap();
        fs::write(mod_dir.join("fun_add.c"), "").unwrap();

        let mut modules = Feature::discover_symbol_modules(&mod_dir).unwrap();
        modules.sort();

        assert_eq!(modules, vec!["fun_add", "fun_sub", "var_counter"]);
    }

    #[test]
    fn discover_symbol_modules_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let mod_dir = tmp.path().join("mod_empty");
        fs::create_dir_all(&mod_dir).unwrap();
        fs::write(mod_dir.join("mod.rs"), "").unwrap();

        let modules = Feature::discover_symbol_modules(&mod_dir).unwrap();
        assert!(modules.is_empty());
    }

    /// Verifies the key adaptation: files are discovered without any `mod fun_*;`
    /// declaration in mod.rs.
    #[test]
    fn discover_symbol_modules_no_mod_rs_entry_needed() {
        let tmp = TempDir::new().unwrap();
        let mod_dir = tmp.path().join("mod_bar");
        fs::create_dir_all(&mod_dir).unwrap();

        // mod.rs has NO `mod fun_baz;` declaration
        fs::write(mod_dir.join("mod.rs"), "// empty mod.rs without submod decls").unwrap();
        // But the .rs file exists on disk
        fs::write(mod_dir.join("fun_baz.rs"), "pub fn baz() {}").unwrap();

        let modules = Feature::discover_symbol_modules(&mod_dir).unwrap();
        assert_eq!(modules, vec!["fun_baz"]);
    }

    // -----------------------------------------------------------------------
    // scan_src_mod_dirs
    // -----------------------------------------------------------------------

    #[test]
    fn scan_src_mod_dirs_finds_mod_dirs() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(src.join("mod_foo")).unwrap();
        fs::create_dir_all(src.join("mod_bar")).unwrap();
        fs::create_dir_all(src.join("lib")).unwrap(); // not a mod_ prefix
        fs::write(src.join("lib.rs"), "").unwrap(); // plain file

        let names = Feature::scan_src_mod_dirs(&src).unwrap();
        assert_eq!(names, vec!["mod_bar", "mod_foo"]);
    }

    // -----------------------------------------------------------------------
    // is_use_super
    // -----------------------------------------------------------------------

    #[test]
    fn is_use_super_variants() {
        let yes: syn::ItemUse = syn::parse_str("use super::*;").unwrap();
        assert!(Feature::is_use_super(&yes));

        let no: syn::ItemUse = syn::parse_str("use super::SomeType;").unwrap();
        assert!(!Feature::is_use_super(&no));

        let no2: syn::ItemUse = syn::parse_str("use crate::foo::*;").unwrap();
        assert!(!Feature::is_use_super(&no2));
    }

    // -----------------------------------------------------------------------
    // Full merge flow – output structure correctness
    // -----------------------------------------------------------------------

    /// Build a minimal feature directory and run merge; verify that:
    /// - `src.2/mod_foo.rs` is created
    /// - `src.2/lib.rs` is created
    /// - The merged module file contains expected FFI
    /// - `rust/src.1` preserves the original init output
    /// - `rust/src` is a symlink to `rust/src.2`
    #[test]
    fn merge_produces_correct_output_structure() {
        let tmp = TempDir::new().unwrap();
        let feature_root = tmp.path().join(".c2rust/default");

        let src = feature_root.join("rust/src");
        let mod_dir = src.join("mod_foo");
        fs::create_dir_all(&mod_dir).unwrap();

        // mod.rs with type + FFI
        fs::write(
            mod_dir.join("mod.rs"),
            r#"
#[allow(unused_imports)]
use super::*;
unsafe extern "C" {
    pub fn add(a: ::core::ffi::c_int, b: ::core::ffi::c_int) -> ::core::ffi::c_int;
}
"#,
        )
        .unwrap();

        // fun_add.rs – stub implementation
        fs::write(
            mod_dir.join("fun_add.rs"),
            r#"
use super::*;
pub fn add(a: ::core::ffi::c_int, b: ::core::ffi::c_int) -> ::core::ffi::c_int {
    a + b
}
"#,
        )
        .unwrap();

        // lib.rs from init
        fs::write(
            src.join("lib.rs"),
            r#"
// generated by c2rust
#![allow(non_camel_case_types)]
mod mod_foo;
"#,
        )
        .unwrap();

        let feat = make_merge_feature(&tmp);
        feat.merge().unwrap();

        // src.2/mod_foo.rs should exist and contain the add function
        let merged = feature_root.join("rust/src.2/mod_foo.rs");
        assert!(merged.exists(), "merged file should be created");
        let merged_content = fs::read_to_string(&merged).unwrap();
        assert!(
            merged_content.contains("fn add"),
            "merged file should contain the add function"
        );

        // src.2/lib.rs should exist
        let lib = feature_root.join("rust/src.2/lib.rs");
        assert!(lib.exists(), "lib.rs should be created in src.2");
        let lib_content = fs::read_to_string(&lib).unwrap();
        assert!(
            lib_content.contains("mod mod_foo"),
            "lib.rs should declare mod_foo module"
        );

        // rust/src.1 should preserve the original init output
        let src1_path = feature_root.join("rust/src.1");
        assert!(
            src1_path.is_dir(),
            "rust/src.1 should preserve the original init output"
        );

        // rust/src should be a symlink pointing to rust/src.2
        let src_path = feature_root.join("rust/src");
        assert!(
            src_path.is_symlink(),
            "rust/src should become a symlink to rust/src.2"
        );
        let linked = fs::read_link(&src_path).unwrap();
        assert_eq!(
            linked,
            feature_root.join("rust/src.2"),
            "rust/src symlink should point to rust/src.2"
        );
    }

    /// Verify that FFI declarations present in only one module are preserved
    /// in that module file and NOT extracted to lib.rs.
    #[test]
    fn merge_preserves_unique_ffi_in_module() {
        let tmp = TempDir::new().unwrap();
        let feature_root = tmp.path().join(".c2rust/default");

        let src = feature_root.join("rust/src");
        let mod_dir = src.join("mod_foo");
        fs::create_dir_all(&mod_dir).unwrap();

        fs::write(
            mod_dir.join("mod.rs"),
            r#"
#[allow(unused_imports)]
use super::*;
unsafe extern "C" {
    pub fn unique_fn(x: ::core::ffi::c_int) -> ::core::ffi::c_int;
}
"#,
        )
        .unwrap();
        fs::write(src.join("lib.rs"), "// generated by c2rust\n#![allow(non_camel_case_types)]\nmod mod_foo;\n").unwrap();

        let feat = make_merge_feature(&tmp);
        feat.merge().unwrap();

        // The unique FFI should still be in mod_foo.rs
        let merged_content =
            fs::read_to_string(feature_root.join("rust/src.2/mod_foo.rs")).unwrap();
        assert!(
            merged_content.contains("unique_fn"),
            "unique FFI should remain in mod_foo.rs"
        );

        // lib.rs should NOT contain unique_fn (it's not a duplicate)
        let lib_content =
            fs::read_to_string(feature_root.join("rust/src.2/lib.rs")).unwrap();
        assert!(
            !lib_content.contains("unique_fn"),
            "unique FFI should not be extracted to lib.rs"
        );
    }

    /// Verify that FFI declarations shared across two modules are deduplicated
    /// into lib.rs and removed from the individual module files.
    #[test]
    fn merge_deduplicates_shared_ffi_to_lib_rs() {
        let tmp = TempDir::new().unwrap();
        let feature_root = tmp.path().join(".c2rust/default");

        let src = feature_root.join("rust/src");
        for mod_name in &["mod_a", "mod_b"] {
            let mod_dir = src.join(mod_name);
            fs::create_dir_all(&mod_dir).unwrap();
            // Both modules declare the same `shared_fn`
            fs::write(
                mod_dir.join("mod.rs"),
                r#"
#[allow(unused_imports)]
use super::*;
unsafe extern "C" {
    pub fn shared_fn(x: ::core::ffi::c_int) -> ::core::ffi::c_int;
}
"#,
            )
            .unwrap();
        }
        fs::write(
            src.join("lib.rs"),
            "// generated by c2rust\n#![allow(non_camel_case_types)]\nmod mod_a;\nmod mod_b;\n",
        )
        .unwrap();

        let feat = make_merge_feature(&tmp);
        feat.merge().unwrap();

        // lib.rs should contain shared_fn
        let lib_content =
            fs::read_to_string(feature_root.join("rust/src.2/lib.rs")).unwrap();
        assert!(
            lib_content.contains("shared_fn"),
            "shared FFI should be extracted to lib.rs"
        );

        // Neither individual module file should retain shared_fn
        for mod_name in &["mod_a", "mod_b"] {
            let content =
                fs::read_to_string(feature_root.join(format!("rust/src.2/{mod_name}.rs")))
                    .unwrap();
            assert!(
                !content.contains("shared_fn"),
                "{mod_name}.rs should not retain the deduplicated FFI"
            );
        }
    }

    /// Verify lib.rs always contains `use ::core::ffi::*;` after merge.
    #[test]
    fn merge_lib_rs_has_ffi_glob_import() {
        let tmp = TempDir::new().unwrap();
        let feature_root = tmp.path().join(".c2rust/default");

        let src = feature_root.join("rust/src");
        let mod_dir = src.join("mod_foo");
        fs::create_dir_all(&mod_dir).unwrap();
        fs::write(mod_dir.join("mod.rs"), "use super::*;\n").unwrap();
        fs::write(src.join("lib.rs"), "// generated\nmod mod_foo;\n").unwrap();

        let feat = make_merge_feature(&tmp);
        feat.merge().unwrap();

        let lib_content =
            fs::read_to_string(feature_root.join("rust/src.2/lib.rs")).unwrap();
        assert!(
            lib_content.contains("core::ffi::*") || lib_content.contains("std::ffi::*"),
            "lib.rs should contain a ffi glob import, got:\n{}",
            lib_content
        );
    }

    // -----------------------------------------------------------------------
    // item_name / foreign_item_name helpers
    // -----------------------------------------------------------------------

    #[test]
    fn item_name_various_kinds() {
        let item: syn::Item = syn::parse_str("struct MyStruct { x: i32 }").unwrap();
        assert_eq!(Feature::item_name(&item), Some("MyStruct".to_string()));

        let item: syn::Item = syn::parse_str("const MAX: usize = 100;").unwrap();
        assert_eq!(Feature::item_name(&item), Some("MAX".to_string()));

        let item: syn::Item = syn::parse_str("fn my_func() {}").unwrap();
        assert_eq!(Feature::item_name(&item), Some("my_func".to_string()));

        let item: syn::Item = syn::parse_str("use std::ffi::*;").unwrap();
        assert_eq!(Feature::item_name(&item), None);
    }

    #[test]
    fn foreign_item_name_fn_and_static() {
        let item: syn::ForeignItem = syn::parse_str("fn ext(x: i32) -> i32;").unwrap();
        assert_eq!(Feature::foreign_item_name(&item), Some("ext".to_string()));

        let item: syn::ForeignItem = syn::parse_str("static EXT_VAR: i32;").unwrap();
        assert_eq!(
            Feature::foreign_item_name(&item),
            Some("EXT_VAR".to_string())
        );
    }

    // -----------------------------------------------------------------------
    // Merge report generation
    // -----------------------------------------------------------------------

    /// Verify that merge produces the merge interface report file.
    #[test]
    fn merge_produces_merge_interface_report() {
        let tmp = TempDir::new().unwrap();
        let feature_root = tmp.path().join(".c2rust/default");

        let src = feature_root.join("rust/src");
        let mod_dir = src.join("mod_foo");
        fs::create_dir_all(&mod_dir).unwrap();

        fs::write(
            mod_dir.join("mod.rs"),
            "#[allow(unused_imports)]\nuse super::*;\nunsafe extern \"C\" { pub fn add(a: ::core::ffi::c_int) -> ::core::ffi::c_int; }\n",
        )
        .unwrap();
        fs::write(
            mod_dir.join("fun_add.rs"),
            "use super::*;\npub fn add(a: ::core::ffi::c_int) -> ::core::ffi::c_int { a }\n",
        )
        .unwrap();
        fs::write(
            src.join("lib.rs"),
            "// generated by c2rust\n#![allow(non_camel_case_types)]\nmod mod_foo;\n",
        )
        .unwrap();

        let feat = make_merge_feature(&tmp);
        feat.merge().unwrap();

        let report_path = feature_root.join("meta/merge-interface-report.md");
        assert!(
            report_path.exists(),
            "merge-interface-report.md should be created"
        );
        let content = fs::read_to_string(&report_path).unwrap();
        assert!(
            content.contains("# Merge Interface Report"),
            "report should have title"
        );
        assert!(
            content.contains("mod_foo"),
            "report should mention mod_foo"
        );
        assert!(
            content.contains("## Summary"),
            "report should have Summary section"
        );
        assert!(
            content.contains("lib.rs"),
            "report should mention lib.rs"
        );
    }

    /// Verify merge report lists functions, variables, and local FFI correctly.
    #[test]
    fn merge_report_content_functions_variables_ffi() {
        let tmp = TempDir::new().unwrap();
        let feature_root = tmp.path().join(".c2rust/default");

        let src = feature_root.join("rust/src");
        let mod_dir = src.join("mod_bar");
        fs::create_dir_all(&mod_dir).unwrap();

        fs::write(
            mod_dir.join("mod.rs"),
            r#"
#[allow(unused_imports)]
use super::*;
unsafe extern "C" {
    pub fn unique_api(x: ::core::ffi::c_int) -> ::core::ffi::c_int;
}
"#,
        )
        .unwrap();
        fs::write(
            mod_dir.join("fun_compute.rs"),
            "use super::*;\npub fn compute() -> ::core::ffi::c_int { 0 }\n",
        )
        .unwrap();
        fs::write(
            mod_dir.join("var_global.rs"),
            "use super::*;\npub static mut global: ::core::ffi::c_int = 0;\n",
        )
        .unwrap();
        fs::write(
            src.join("lib.rs"),
            "// generated\n#![allow(non_camel_case_types)]\nmod mod_bar;\n",
        )
        .unwrap();

        let feat = make_merge_feature(&tmp);
        feat.merge().unwrap();

        let content =
            fs::read_to_string(feature_root.join("meta/merge-interface-report.md")).unwrap();

        // Functions discovered from fun_*.rs stems
        assert!(
            content.contains("`compute`"),
            "report should list compute function, got:\n{content}"
        );
        // Variables discovered from var_*.rs stems
        assert!(
            content.contains("`global`"),
            "report should list global variable, got:\n{content}"
        );
        // Module-local FFI (not shared, so stays in module)
        assert!(
            content.contains("`unique_api`"),
            "report should list unique_api FFI, got:\n{content}"
        );
        // Source files section
        assert!(
            content.contains("fun_compute.rs"),
            "report should list source files, got:\n{content}"
        );
    }

    /// Verify merge report correctly identifies shared FFI moved to lib.rs.
    #[test]
    fn merge_report_shows_shared_ffi_in_lib_rs() {
        let tmp = TempDir::new().unwrap();
        let feature_root = tmp.path().join(".c2rust/default");

        let src = feature_root.join("rust/src");
        for mod_name in &["mod_x", "mod_y"] {
            let mod_dir = src.join(mod_name);
            fs::create_dir_all(&mod_dir).unwrap();
            fs::write(
                mod_dir.join("mod.rs"),
                r#"
#[allow(unused_imports)]
use super::*;
unsafe extern "C" {
    pub fn shared_helper(v: ::core::ffi::c_int) -> ::core::ffi::c_int;
}
"#,
            )
            .unwrap();
        }
        fs::write(
            src.join("lib.rs"),
            "// generated\n#![allow(non_camel_case_types)]\nmod mod_x;\nmod mod_y;\n",
        )
        .unwrap();

        let feat = make_merge_feature(&tmp);
        feat.merge().unwrap();

        let content =
            fs::read_to_string(feature_root.join("meta/merge-interface-report.md")).unwrap();

        assert!(
            content.contains("shared_helper"),
            "report should list shared_helper in the shared FFI section, got:\n{content}"
        );
        assert!(
            content.contains("lib.rs` — Shared FFI"),
            "report should have a shared FFI section, got:\n{content}"
        );
    }

    /// Verify write_merge_report directly with controlled data.
    #[test]
    fn write_merge_report_format() {
        let tmp = TempDir::new().unwrap();
        let feat = make_merge_feature(&tmp);
        // Ensure meta dir exists
        fs::create_dir_all(tmp.path().join(".c2rust/default/meta")).unwrap();

        let summaries = vec![MergeModSummary {
            mod_name: "mod_alpha".to_string(),
            fn_names: vec!["do_work".to_string()],
            var_names: vec!["state_var".to_string()],
            ffi_names: vec!["local_ffi_fn".to_string()],
            source_files: vec![
                "mod.rs".to_string(),
                "fun_do_work.rs".to_string(),
                "var_state_var.rs".to_string(),
            ],
        }];
        let shared = vec!["global_init".to_string()];

        feat.write_merge_report(&summaries, &shared).unwrap();

        let report_path = tmp.path().join(".c2rust/default/meta/merge-interface-report.md");
        assert!(report_path.exists());
        let content = fs::read_to_string(&report_path).unwrap();

        assert!(content.contains("# Merge Interface Report — feature `default`"));
        assert!(content.contains("## Summary"));
        assert!(content.contains("Merged modules | 1"));
        assert!(content.contains("Total functions | 1"));
        assert!(content.contains("Total variables | 1"));
        assert!(content.contains("Module-local FFI items | 1"));
        assert!(content.contains("Shared FFI moved to `lib.rs` | 1"));
        assert!(content.contains("`global_init`"));
        assert!(content.contains("## mod_alpha"));
        assert!(content.contains("`do_work`"));
        assert!(content.contains("`state_var`"));
        assert!(content.contains("`local_ffi_fn`"));
        assert!(content.contains("fun_do_work.rs"));
    }

    // -----------------------------------------------------------------------
    // Additional edge-case tests
    // -----------------------------------------------------------------------

    /// merge 在 rust/src 目录不存在时应返回错误。
    #[test]
    fn merge_errors_when_src_dir_missing() {
        let tmp = TempDir::new().unwrap();
        let feat = make_merge_feature(&tmp);
        // 不创建 rust/src 目录
        let result = feat.merge();
        assert!(result.is_err(), "merge should fail when rust/src is absent");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("run init first") || msg.contains("does not exist"),
            "error should hint to run init first, got: {msg}"
        );
    }

    /// merge 在没有 mod_* 子目录时应成功并提前返回，不创建 src.2。
    #[test]
    fn merge_no_op_when_no_mod_dirs() {
        let tmp = TempDir::new().unwrap();
        let feature_root = tmp.path().join(".c2rust/default");
        let src = feature_root.join("rust/src");
        fs::create_dir_all(&src).unwrap();
        // lib.rs 但没有 mod_* 子目录
        fs::write(src.join("lib.rs"), "// empty\n").unwrap();

        let feat = make_merge_feature(&tmp);
        feat.merge().unwrap(); // 应当正常返回

        // src.2 目录不应被创建
        assert!(
            !feature_root.join("rust/src.2").exists(),
            "src.2 should not be created when there are no mod_* dirs"
        );
    }

    /// 第二次执行 merge 时 rust/src 已是 symlink，应当成功替换而不报错。
    #[test]
    fn merge_second_run_replaces_existing_symlink() {
        let tmp = TempDir::new().unwrap();
        let feature_root = tmp.path().join(".c2rust/default");

        let src = feature_root.join("rust/src");
        let mod_dir = src.join("mod_foo");
        fs::create_dir_all(&mod_dir).unwrap();
        fs::write(mod_dir.join("mod.rs"), "use super::*;\n").unwrap();
        fs::write(src.join("lib.rs"), "// generated\nmod mod_foo;\n").unwrap();

        let feat = make_merge_feature(&tmp);
        feat.merge().unwrap();

        // 第一次 merge 后 src 是 symlink；再次运行 merge 需要先重建 src 目录
        // 模拟第二次 merge：将 src 恢复为真实目录（像用户重跑 init 那样）
        let src_path = feature_root.join("rust/src");
        fs::remove_file(&src_path).unwrap(); // 移除 symlink
        fs::create_dir_all(src_path.join("mod_foo")).unwrap();
        fs::write(src_path.join("mod_foo/mod.rs"), "use super::*;\n").unwrap();
        fs::write(src_path.join("lib.rs"), "// regenerated\nmod mod_foo;\n").unwrap();

        // 第二次 merge 应成功
        let feat2 = make_merge_feature(&tmp);
        feat2.merge().unwrap();

        assert!(
            feature_root.join("rust/src").is_symlink(),
            "src should be a symlink after second merge"
        );
    }

    /// is_ffi_glob_import 应当识别 core::ffi::* 和 std::ffi::* 的各种写法。
    #[test]
    fn is_ffi_glob_import_various_forms() {
        // 标准写法
        let item: syn::Item = syn::parse_str("use core::ffi::*;").unwrap();
        assert!(Feature::is_ffi_glob_import(&item), "core::ffi::*");

        let item: syn::Item = syn::parse_str("use ::core::ffi::*;").unwrap();
        assert!(Feature::is_ffi_glob_import(&item), "::core::ffi::*");

        let item: syn::Item = syn::parse_str("use std::ffi::*;").unwrap();
        assert!(Feature::is_ffi_glob_import(&item), "std::ffi::*");

        // 不应匹配的写法
        let item: syn::Item = syn::parse_str("use core::ffi::c_int;").unwrap();
        assert!(!Feature::is_ffi_glob_import(&item), "non-glob import");

        let item: syn::Item = syn::parse_str("use crate::ffi::*;").unwrap();
        assert!(!Feature::is_ffi_glob_import(&item), "crate::ffi::*");

        let item: syn::Item = syn::parse_str("fn foo() {}").unwrap();
        assert!(!Feature::is_ffi_glob_import(&item), "function item");
    }

    /// scan_src_mod_dirs 在空目录时应返回空列表。
    #[test]
    fn scan_src_mod_dirs_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        let names = Feature::scan_src_mod_dirs(&src).unwrap();
        assert!(names.is_empty());
    }

    /// collect_symbol_items 对空文件应静默跳过，不报错，不追加任何 item。
    #[test]
    fn collect_symbol_items_empty_file_is_noop() {
        let tmp = TempDir::new().unwrap();
        let empty = tmp.path().join("empty.rs");
        fs::write(&empty, "   \n  ").unwrap();
        let mut items: Vec<syn::Item> = Vec::new();
        Feature::collect_symbol_items(&empty, &mut items).unwrap();
        assert!(items.is_empty(), "empty file should not add any items");
    }

    /// collect_symbol_items 应当过滤掉 `use super::*;` 导入。
    #[test]
    fn collect_symbol_items_skips_use_super() {
        let tmp = TempDir::new().unwrap();
        let rs = tmp.path().join("fun_foo.rs");
        fs::write(
            &rs,
            "use super::*;\npub fn foo() -> i32 { 42 }\n",
        )
        .unwrap();
        let mut items: Vec<syn::Item> = Vec::new();
        Feature::collect_symbol_items(&rs, &mut items).unwrap();
        // 只有函数，use super::* 被过滤
        assert_eq!(items.len(), 1);
        assert!(
            Feature::item_name(&items[0]) == Some("foo".to_string()),
            "should only contain the function"
        );
    }

    /// 多个模块各自有独立 FFI 时，lib.rs 中不应出现任何 extern 块（无共享 FFI）。
    #[test]
    fn merge_no_shared_ffi_lib_rs_has_no_extern_block() {
        let tmp = TempDir::new().unwrap();
        let feature_root = tmp.path().join(".c2rust/default");

        let src = feature_root.join("rust/src");
        for (mod_name, fn_name) in &[("mod_a", "fn_a_only"), ("mod_b", "fn_b_only")] {
            let mod_dir = src.join(mod_name);
            fs::create_dir_all(&mod_dir).unwrap();
            fs::write(
                mod_dir.join("mod.rs"),
                format!(
                    "use super::*;\nunsafe extern \"C\" {{ pub fn {fn_name}(x: i32) -> i32; }}\n"
                ),
            )
            .unwrap();
        }
        fs::write(
            src.join("lib.rs"),
            "// generated\nmod mod_a;\nmod mod_b;\n",
        )
        .unwrap();

        let feat = make_merge_feature(&tmp);
        feat.merge().unwrap();

        let lib_content =
            fs::read_to_string(feature_root.join("rust/src.2/lib.rs")).unwrap();

        // 唯一 FFI 不应出现在 lib.rs 中
        assert!(
            !lib_content.contains("fn_a_only"),
            "unique FFI should not appear in lib.rs"
        );
        assert!(
            !lib_content.contains("fn_b_only"),
            "unique FFI should not appear in lib.rs"
        );
    }
}
