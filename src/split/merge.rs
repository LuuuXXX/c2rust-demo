//! Merge logic for c2rust-demo.
//!
//! Adapted from `c2rust-code-analyse/src/merge.rs`.
//!
//! Key adaptation: module discovery uses directory scanning for `fun_*.rs` and
//! `var_*.rs` files instead of parsing `mod.rs` for `mod fun_*;` declarations,
//! because the c2rust-demo init flow does NOT inject those declarations.

use crate::error::{Result, ToError};
use crate::split::feature::Feature;
use quote::{quote, ToTokens};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::{visit_foreign_item, visit_item, visit_path, Visit};
use walkdir::WalkDir;

// ---------------------------------------------------------------------------
// Helper types
// ---------------------------------------------------------------------------

struct DepNames {
    used_names: HashMap<String, bool>,
    mac_tokens: String,
}

impl DepNames {
    fn new() -> Self {
        Self {
            used_names: HashMap::new(),
            mac_tokens: String::new(),
        }
    }

    fn contains(&self, name: &str) -> bool {
        if self.used_names.contains_key(name) {
            return true;
        }
        let regex = regex::Regex::new(&format!("[^a-zA-Z_]{name}[^a-zA-Z_]")).unwrap();
        regex.find(&self.mac_tokens).is_some()
    }

    fn mark_used(&mut self, name: String) {
        self.used_names.entry(name).or_insert(false);
    }

    fn mark_pub(&mut self, name: String) {
        self.used_names.insert(name, true);
    }

    fn is_pub(&self, name: &str) -> bool {
        self.used_names.get(name).copied().unwrap_or(false)
    }
}

impl Visit<'_> for DepNames {
    fn visit_path(&mut self, path: &syn::Path) {
        if let Some(ident) = path.segments.last() {
            let name = ident.ident.to_string();
            if !name.starts_with("_c2rust_private_") {
                self.mark_used(name);
            }
        }
        visit_path(self, path);
    }

    fn visit_use_name(&mut self, name: &syn::UseName) {
        self.mark_used(name.ident.to_string());
    }

    fn visit_use_rename(&mut self, rename: &syn::UseRename) {
        self.mark_used(rename.ident.to_string());
    }

    fn visit_macro(&mut self, mac: &syn::Macro) {
        self.mac_tokens.push_str(&mac.tokens.to_string());
    }
}

struct PubDepVisitor<'a>(&'a mut DepNames);

impl Visit<'_> for PubDepVisitor<'_> {
    fn visit_path(&mut self, path: &syn::Path) {
        if let Some(ident) = path.segments.last() {
            let name = ident.ident.to_string();
            if !name.starts_with("_c2rust_private_") {
                self.0.mark_pub(name);
            }
        }
        visit_path(self, path);
    }

    fn visit_block(&mut self, _: &syn::Block) {}
    fn visit_expr(&mut self, _: &syn::Expr) {}
    fn visit_stmt(&mut self, _: &syn::Stmt) {}
}

struct CollectedItems {
    named_items: HashMap<String, Vec<TypeItem>>,
    ffi_items: HashMap<String, Vec<syn::ForeignItem>>,
    foreign_mod_template: Option<syn::ItemForeignMod>,
}

pub(crate) struct TypeItem {
    pub(crate) type_def: syn::Item,
    pub(crate) impl_blocks: Vec<syn::ItemImpl>,
}

impl TypeItem {
    fn new(type_def: syn::Item) -> Self {
        Self {
            type_def,
            impl_blocks: Vec::new(),
        }
    }

    fn name(&self) -> Option<String> {
        Feature::item_name(&self.type_def)
    }

    fn add_impl(&mut self, impl_block: syn::ItemImpl) {
        self.impl_blocks.push(impl_block);
    }
}

impl Clone for TypeItem {
    fn clone(&self) -> Self {
        Self {
            type_def: self.type_def.clone(),
            impl_blocks: self.impl_blocks.clone(),
        }
    }
}

#[derive(Default)]
struct Duplicates {
    named_to_extract: Vec<TypeItem>,
    named_remove_set: HashSet<String>,
    ffi_to_extract: Vec<syn::ForeignItem>,
    ffi_remove_set: HashSet<String>,
}

impl Duplicates {
    fn remove(&mut self, names: &HashSet<String>) {
        self.named_to_extract.retain(|item| {
            if let Some(name) = Feature::item_name(&item.type_def) {
                if names.contains(&name) {
                    println!("{name} - remove type item defined in mod");
                    return false;
                }
            }
            true
        });
        self.ffi_to_extract.retain(|item| {
            if let Some(name) = Feature::foreign_item_name(item) {
                if names.contains(&name) {
                    println!("{name} - remove foreign item defined in mod");
                    return false;
                }
            }
            true
        });
    }
}

// ---------------------------------------------------------------------------
// impl Feature – merge methods
// ---------------------------------------------------------------------------

impl Feature {
    /// Merge the init output under `.c2rust/<feature>/rust/src/` into
    /// `.c2rust/<feature>/rust/src.2/`.
    ///
    /// For each `mod_xxx` directory: discovers symbol files by directory scan,
    /// merges them into a single `mod_xxx.rs`, then deduplicates common FFI
    /// declarations into `lib.rs`.
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
            println!("No mod_* directories found under {}; nothing to merge.", src_dir.display());
            return Ok(());
        }

        for mod_name in &mod_names {
            self.merge_mod_dir(mod_name)?;
        }

        self.deduplicate_mod_rs()?;
        self.link_src()?;

        println!("Feature '{}' merged successfully", self.name);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Module-level merge
    // -----------------------------------------------------------------------

    /// Scan `rust/src/` for `mod_*` subdirectories and return their names.
    fn scan_src_mod_dirs(src_dir: &Path) -> Result<Vec<String>> {
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

    /// **Key adaptation**: scan a `mod_xxx/` directory for `fun_*.rs` and
    /// `var_*.rs` files instead of parsing `mod.rs` for `mod fun_*;`
    /// declarations.
    fn discover_symbol_modules(mod_dir: &Path) -> Result<Vec<String>> {
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

    /// Merge all symbol files in a `mod_xxx` directory into
    /// `rust/src.2/mod_xxx.rs`.
    fn merge_mod_dir(&self, mod_name: &str) -> Result<bool> {
        let src_dir = self.root.join("rust/src");
        let mod_dir = src_dir.join(mod_name);

        if !mod_dir.exists() {
            return Ok(false);
        }

        println!("Processing mod for merge: {}", mod_name);

        let module_names = Self::discover_symbol_modules(&mod_dir)?;

        println!(
            "Merging {} modules for mod: {} ...",
            module_names.len(),
            mod_name
        );

        if module_names.is_empty() {
            println!("No symbol modules to merge for: {}", mod_name);
            return Ok(false);
        }

        let mut items: Vec<syn::Item> = Vec::new();
        let mut deps = DepNames::new();

        for module_name in &module_names {
            let rs_file = mod_dir.join(module_name).with_extension("rs");
            Self::merge_main_item(&rs_file, &mut items, &mut deps)?;
        }

        let mod_rs = mod_dir.join("mod.rs");
        let (type_items, foreign_mod) = Self::extract_dependencies(&mod_rs, &mut deps)?;

        let mut merged_items: Vec<syn::Item> = Vec::new();
        merged_items.push(syn::parse2(quote! { use super::*; }).unwrap());

        for alias in &module_names {
            merged_items
                .push(syn::parse_str(&format!("use super::{mod_name} as {alias};")).unwrap());
        }

        for type_item in &type_items {
            if let Some(type_name) = type_item.name() {
                let mut type_def = type_item.type_def.clone();
                Self::set_item_visibility(&mut type_def, deps.is_pub(&type_name));
                merged_items.push(type_def);
                for impl_block in &type_item.impl_blocks {
                    merged_items.push(syn::Item::Impl(impl_block.clone()));
                }
            }
        }
        if let Some(fm) = foreign_mod {
            merged_items.push(syn::Item::ForeignMod(fm));
        }
        merged_items.extend(items);

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
        Ok(true)
    }

    // -----------------------------------------------------------------------
    // Item helpers
    // -----------------------------------------------------------------------

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

    pub(crate) fn foreign_item_name(item: &syn::ForeignItem) -> Option<String> {
        match item {
            syn::ForeignItem::Fn(item) => Some(item.sig.ident.to_string()),
            syn::ForeignItem::Static(item) => Some(item.ident.to_string()),
            _ => None,
        }
    }

    fn impl_self_type_name(impl_item: &syn::ItemImpl) -> Option<String> {
        match &*impl_item.self_ty {
            syn::Type::Path(type_path) if type_path.qself.is_none() => {
                type_path
                    .path
                    .segments
                    .last()
                    .map(|s| s.ident.to_string())
            }
            _ => None,
        }
    }

    fn set_item_visibility(item: &mut syn::Item, is_pub: bool) {
        let vis: syn::Visibility = if is_pub {
            syn::parse_str("pub").unwrap()
        } else {
            syn::Visibility::Inherited
        };
        match item {
            syn::Item::Struct(s) => s.vis = vis,
            syn::Item::Union(u) => u.vis = vis,
            syn::Item::Const(c) => c.vis = vis,
            syn::Item::Type(t) => t.vis = vis,
            _ => {}
        }
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

    fn remove_private_attr(attrs: &mut Vec<syn::Attribute>) -> bool {
        let len = attrs.len();
        attrs.retain(|attr| {
            let s = attr.to_token_stream().to_string();
            !s.contains("_c2rust_private_")
        });
        len != attrs.len()
    }

    // -----------------------------------------------------------------------
    // Per-symbol-file merge helpers
    // -----------------------------------------------------------------------

    fn merge_main_item(
        rs_file: &Path,
        all_items: &mut Vec<syn::Item>,
        deps: &mut DepNames,
    ) -> Result<()> {
        let content =
            fs::read_to_string(rs_file).ctx(&format!("read {}", rs_file.display()))?;
        let ast =
            syn::parse_file(&content).ctx(&format!("parse {}", rs_file.display()))?;

        let file_stem = rs_file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();

        if !file_stem.starts_with("fun_") && !file_stem.starts_with("var_") {
            return Ok(());
        }
        let main_item_name = &file_stem[4..];

        let mut main_item: Option<syn::Item> = None;
        let mut other_items: Vec<syn::Item> = Vec::new();

        for item in ast.items {
            match item {
                syn::Item::Fn(ref item_fn) if item_fn.sig.ident == main_item_name => {
                    main_item = Some(item);
                }
                syn::Item::Static(ref item_static)
                    if item_static.ident == main_item_name =>
                {
                    main_item = Some(item);
                }
                syn::Item::Use(ref item_use) if Self::is_use_super(item_use) => {}
                _ => other_items.push(item),
            }
        }

        if let Some(syn::Item::Fn(mut fn_item)) = main_item {
            Self::merge_item_fn(other_items, &mut fn_item)?;
            if matches!(fn_item.vis, syn::Visibility::Public(_)) {
                PubDepVisitor(deps).visit_signature(&fn_item.sig);
            }
            visit_item(deps, &syn::Item::Fn(fn_item.clone()));
            all_items.push(syn::Item::Fn(fn_item));
        } else if let Some(syn::Item::Static(mut var_item)) = main_item {
            Self::merge_item_static(other_items, &mut var_item);
            if matches!(var_item.vis, syn::Visibility::Public(_)) {
                PubDepVisitor(deps).visit_type(&var_item.ty);
            }
            visit_item(deps, &syn::Item::Static(var_item.clone()));
            all_items.push(syn::Item::Static(var_item));
        } else {
            eprintln!(
                "Warning: could not find symbol '{}' in {}",
                main_item_name,
                rs_file.display()
            );
        }
        Ok(())
    }

    fn merge_item_fn(items: Vec<syn::Item>, fn_item: &mut syn::ItemFn) -> Result<()> {
        if Self::remove_private_attr(&mut fn_item.attrs) {
            fn_item.vis = syn::Visibility::Inherited;
        }
        if items.is_empty() {
            return Ok(());
        }
        let block = &fn_item.block;
        let new_block = quote! {{
            #(#items)*
            #block
        }};
        fn_item.block = syn::parse2(new_block).unwrap();
        Ok(())
    }

    fn merge_item_static(items: Vec<syn::Item>, static_item: &mut syn::ItemStatic) {
        if Self::remove_private_attr(&mut static_item.attrs) {
            static_item.vis = syn::Visibility::Inherited;
        }
        if items.is_empty() {
            return;
        }
        let expr = &static_item.expr;
        let new_expr = quote! {{
            #(#items)*
            #expr
        }};
        static_item.expr = syn::parse2(new_expr).unwrap();
    }

    // -----------------------------------------------------------------------
    // Dependency extraction from mod.rs
    // -----------------------------------------------------------------------

    fn extract_dependencies(
        mod_rs: &Path,
        deps: &mut DepNames,
    ) -> Result<(Vec<TypeItem>, Option<syn::ItemForeignMod>)> {
        if !mod_rs.exists() {
            return Ok((Vec::new(), None));
        }
        let content =
            fs::read_to_string(mod_rs).ctx(&format!("read {}", mod_rs.display()))?;
        let ast =
            syn::parse_file(&content).ctx(&format!("parse {}", mod_rs.display()))?;

        let mut all_types: HashMap<String, TypeItem> = HashMap::new();
        let mut all_ffi: HashMap<String, syn::ForeignItem> = HashMap::new();
        let mut foreign_mod_template: Option<syn::ItemForeignMod> = None;

        for item in ast.items {
            match item {
                syn::Item::ForeignMod(ref fm) => {
                    if foreign_mod_template.is_none() {
                        let mut template = fm.clone();
                        template.items.clear();
                        foreign_mod_template = Some(template);
                    }
                    for ffi_item in fm.items.clone() {
                        if let Some(name) = Self::foreign_item_name(&ffi_item) {
                            all_ffi.insert(name, ffi_item);
                        }
                    }
                }
                syn::Item::Impl(impl_block) => {
                    if let Some(type_name) = Self::impl_self_type_name(&impl_block) {
                        if let Some(type_item) = all_types.get_mut(&type_name) {
                            type_item.add_impl(impl_block);
                        }
                    }
                }
                _ => {
                    if let Some(name) = Self::item_name(&item) {
                        all_types.insert(name, TypeItem::new(item));
                    }
                }
            }
        }

        let mut dep_types = Vec::new();
        let mut dep_ffi = Vec::new();
        Self::filter_dependencies(all_types, all_ffi, deps, &mut dep_types, &mut dep_ffi);

        let foreign_mod = if !dep_ffi.is_empty() {
            let mut fm = foreign_mod_template.unwrap();
            fm.items = dep_ffi;
            Some(fm)
        } else {
            None
        };

        Ok((dep_types, foreign_mod))
    }

    fn filter_dependencies(
        mut all_types: HashMap<String, TypeItem>,
        all_ffi: HashMap<String, syn::ForeignItem>,
        deps: &mut DepNames,
        dep_types: &mut Vec<TypeItem>,
        dep_ffi: &mut Vec<syn::ForeignItem>,
    ) {
        for (name, item) in all_ffi {
            if deps.contains(&name) {
                visit_foreign_item(deps, &item);
                dep_ffi.push(item);
            }
        }

        let mut new_dep = true;
        while new_dep {
            new_dep = false;
            all_types.retain(|name, type_item| {
                if deps.contains(name) {
                    visit_item(deps, &type_item.type_def);
                    for impl_block in &type_item.impl_blocks {
                        visit_item(deps, &syn::Item::Impl(impl_block.clone()));
                    }
                    if deps.is_pub(name) {
                        PubDepVisitor(deps).visit_item(&type_item.type_def);
                    }
                    dep_types.push(type_item.clone());
                    new_dep = true;
                    return false;
                }
                true
            });
        }

        dep_types.sort_by(|a, b| a.name().cmp(&b.name()));
        // Intentionally descending – mirrors original c2rust-code-analyse behavior.
        dep_ffi.sort_by(|a, b| Self::foreign_item_name(b).cmp(&Self::foreign_item_name(a)));
    }

    // -----------------------------------------------------------------------
    // Deduplication across modules
    // -----------------------------------------------------------------------

    fn deduplicate_mod_rs(&self) -> Result<()> {
        let src_2 = self.root.join("rust/src.2");
        if !src_2.exists() {
            return Ok(());
        }

        let mod_files = Self::collect_mod_files(&src_2)?;
        if mod_files.is_empty() {
            return Ok(());
        }

        let collected = Self::collect_items_from_files(&mod_files)?;
        let mut duplicates =
            Self::find_duplicates(&collected.named_items, &collected.ffi_items);

        Self::update_lib_rs(&src_2, &mut duplicates, &collected.foreign_mod_template)?;

        if !duplicates.named_remove_set.is_empty() || !duplicates.ffi_remove_set.is_empty() {
            Self::remove_duplicates_from_files(&mod_files, &duplicates)?;
        }

        println!(
            "Deduplicated {} types and {} FFI declarations to lib.rs",
            duplicates.named_remove_set.len(),
            duplicates.ffi_remove_set.len()
        );
        Ok(())
    }

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

    fn collect_items_from_files(mod_files: &[PathBuf]) -> Result<CollectedItems> {
        let mut named_items: HashMap<String, Vec<TypeItem>> = HashMap::new();
        let mut ffi_items: HashMap<String, Vec<syn::ForeignItem>> = HashMap::new();
        let mut foreign_mod_template: Option<syn::ItemForeignMod> = None;

        for mod_file in mod_files {
            let content =
                fs::read_to_string(mod_file).ctx(&format!("read {}", mod_file.display()))?;
            let ast =
                syn::parse_file(&content).ctx(&format!("parse {}", mod_file.display()))?;

            let mut file_type_items: Vec<(String, TypeItem)> = Vec::new();
            let mut file_impls: Vec<(String, syn::ItemImpl)> = Vec::new();

            for item in ast.items {
                match item {
                    syn::Item::Struct(s) => {
                        let name = s.ident.to_string();
                        file_type_items.push((name, TypeItem::new(syn::Item::Struct(s))));
                    }
                    syn::Item::Union(u) => {
                        let name = u.ident.to_string();
                        file_type_items.push((name, TypeItem::new(syn::Item::Union(u))));
                    }
                    syn::Item::Const(c) => {
                        let name = c.ident.to_string();
                        file_type_items.push((name, TypeItem::new(syn::Item::Const(c))));
                    }
                    syn::Item::Type(t) => {
                        let name = t.ident.to_string();
                        file_type_items.push((name, TypeItem::new(syn::Item::Type(t))));
                    }
                    syn::Item::Impl(impl_block) => {
                        if let Some(type_name) = Self::impl_self_type_name(&impl_block) {
                            file_impls.push((type_name, impl_block));
                        }
                    }
                    syn::Item::ForeignMod(fm) => {
                        if foreign_mod_template.is_none() {
                            let mut template = fm.clone();
                            template.items.clear();
                            foreign_mod_template = Some(template);
                        }
                        for ffi_item in fm.items {
                            let name = Self::ffi_name(&ffi_item);
                            ffi_items.entry(name).or_default().push(ffi_item);
                        }
                    }
                    _ => {}
                }
            }

            for (type_name, mut type_item) in file_type_items {
                for (impl_type_name, impl_block) in &file_impls {
                    if *impl_type_name == type_name {
                        type_item.add_impl(impl_block.clone());
                    }
                }
                named_items.entry(type_name).or_default().push(type_item);
            }
        }

        Ok(CollectedItems {
            named_items,
            ffi_items,
            foreign_mod_template,
        })
    }

    fn item_body(item: &syn::Item) -> String {
        let mut item = item.clone();
        let (attrs, vis) = match item {
            syn::Item::Struct(ref mut i) => (&mut i.attrs, &mut i.vis),
            syn::Item::Union(ref mut i) => (&mut i.attrs, &mut i.vis),
            syn::Item::Type(ref mut i) => (&mut i.attrs, &mut i.vis),
            syn::Item::Const(ref mut i) => (&mut i.attrs, &mut i.vis),
            _ => return item.to_token_stream().to_string(),
        };
        attrs.clear();
        *vis = syn::Visibility::Inherited;
        item.to_token_stream().to_string()
    }

    fn find_duplicates(
        named_items: &HashMap<String, Vec<TypeItem>>,
        ffi_items: &HashMap<String, Vec<syn::ForeignItem>>,
    ) -> Duplicates {
        let mut named_to_extract: Vec<TypeItem> = Vec::new();
        let mut named_remove_set: HashSet<String> = HashSet::new();
        let mut ffi_to_extract: Vec<syn::ForeignItem> = Vec::new();
        let mut ffi_remove_set: HashSet<String> = HashSet::new();

        for (type_name, type_items) in named_items {
            if type_items.len() > 1 {
                let first_body = Self::item_body(&type_items[0].type_def);
                if type_items
                    .iter()
                    .all(|ti| Self::item_body(&ti.type_def) == first_body)
                {
                    named_to_extract.push(type_items[0].clone());
                    named_remove_set.insert(type_name.clone());
                }
            }
        }

        for (name, items) in ffi_items {
            if items.len() > 1 {
                ffi_to_extract.push(items[0].clone());
                ffi_remove_set.insert(name.clone());
            }
        }

        Duplicates {
            named_to_extract,
            named_remove_set,
            ffi_to_extract,
            ffi_remove_set,
        }
    }

    /// Returns true if `item` is a glob import of `core::ffi`, `::core::ffi`,
    /// or `std::ffi`.
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

    fn update_lib_rs(
        src_2: &Path,
        duplicates: &mut Duplicates,
        foreign_mod_template: &Option<syn::ItemForeignMod>,
    ) -> Result<()> {
        // Read from original rust/src/lib.rs (src_2 is rust/src.2)
        let lib_rs_file = src_2.parent().unwrap().join("src/lib.rs");
        let content =
            fs::read_to_string(&lib_rs_file).ctx(&format!("read {}", lib_rs_file.display()))?;
        let mut lib_rs =
            syn::parse_file(&content).ctx(&format!("parse {}", lib_rs_file.display()))?;
        let lib_items = &mut lib_rs.items;

        // Ensure `use ::core::ffi::*;` is present.
        if !lib_items.iter().any(Self::is_ffi_glob_import) {
            let use_ffi: syn::Item = syn::parse_str("use ::core::ffi::*;")
                .ctx("parse use ::core::ffi::*")?;
            lib_items.insert(0, use_ffi);
        }

        let mut used_names: HashSet<String> = HashSet::new();
        lib_items.iter().for_each(|item| {
            if let Some(name) = Self::item_name(item) {
                used_names.insert(name);
            }
        });
        duplicates.remove(&used_names);

        for type_item in &duplicates.named_to_extract {
            lib_items.push(type_item.type_def.clone());
            for impl_block in &type_item.impl_blocks {
                lib_items.push(syn::Item::Impl(impl_block.clone()));
            }
        }

        if !duplicates.ffi_to_extract.is_empty() {
            if let Some(mut fm) = foreign_mod_template.clone() {
                fm.items = duplicates.ffi_to_extract.clone();
                lib_items.push(syn::Item::ForeignMod(fm));
            }
        }

        let lib_content = prettyplease::unparse(&lib_rs);
        // Write to rust/src.2/lib.rs
        let lib_rs_path = src_2.join("lib.rs");
        fs::write(&lib_rs_path, lib_content.as_bytes())
            .ctx(&format!("write {}", lib_rs_path.display()))?;

        Ok(())
    }

    fn remove_duplicates_from_files(
        mod_files: &[PathBuf],
        duplicates: &Duplicates,
    ) -> Result<()> {
        for mod_file in mod_files {
            let content =
                fs::read_to_string(mod_file).ctx(&format!("read {}", mod_file.display()))?;
            let mut ast =
                syn::parse_file(&content).ctx(&format!("parse {}", mod_file.display()))?;

            ast.items.retain_mut(|item| match item {
                syn::Item::Struct(s) => {
                    !duplicates.named_remove_set.contains(&s.ident.to_string())
                }
                syn::Item::Union(u) => {
                    !duplicates.named_remove_set.contains(&u.ident.to_string())
                }
                syn::Item::Const(c) => {
                    !duplicates.named_remove_set.contains(&c.ident.to_string())
                }
                syn::Item::Type(t) => {
                    !duplicates.named_remove_set.contains(&t.ident.to_string())
                }
                syn::Item::Impl(impl_block) => {
                    if let Some(type_name) = Self::impl_self_type_name(impl_block) {
                        !duplicates.named_remove_set.contains(&type_name)
                    } else {
                        true
                    }
                }
                syn::Item::ForeignMod(fm) => {
                    fm.items.retain(|ffi| {
                        !duplicates.ffi_remove_set.contains(&Self::ffi_name(ffi))
                    });
                    !fm.items.is_empty()
                }
                _ => true,
            });

            let formatted = prettyplease::unparse(&ast);
            fs::write(mod_file, formatted.as_bytes())
                .ctx(&format!("write {}", mod_file.display()))?;
        }
        Ok(())
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

    /// Replace `rust/src` symlink/directory with a symlink to `rust/src.2`.
    ///
    /// # Platform note
    /// Uses `std::os::unix::fs::symlink`, which is Unix-only.  This is
    /// consistent with the rest of the tool (which relies on `LD_PRELOAD`
    /// for build capture) and is not expected to run on Windows.
    fn link_src(&self) -> Result<()> {
        let src = self.root.join("rust/src");
        if src.is_symlink() {
            fs::remove_file(&src).ctx("remove link[rust/src]")?;
        } else {
            let old_src = self.root.join("rust/src.1");
            let _ = fs::remove_dir_all(&old_src);
            fs::rename(&src, &old_src).ctx("rename[src -> src.1]")?;
        }
        let new_src = self.root.join("rust/src.2");
        std::os::unix::fs::symlink(&new_src, &src)
            .ctx(&format!("symlink {} -> {}", src.display(), new_src.display()))?;
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

    /// Build a minimal Feature pointing at a temp dir (no files needed for merge).
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

        // Create some symbol files
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

    #[test]
    fn discover_symbol_modules_no_mod_rs_entry_needed() {
        // Verify that discovery works even without mod.rs containing `mod fun_*;`
        let tmp = TempDir::new().unwrap();
        let mod_dir = tmp.path().join("mod_bar");
        fs::create_dir_all(&mod_dir).unwrap();

        // mod.rs has NO `mod fun_baz;` declaration
        fs::write(mod_dir.join("mod.rs"), "// empty mod.rs without submod decls").unwrap();
        // But the file exists on disk
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
        // Not a mod_ prefix – should be ignored
        fs::create_dir_all(src.join("lib")).unwrap();
        // A plain file – should be ignored
        fs::write(src.join("lib.rs"), "").unwrap();

        let names = Feature::scan_src_mod_dirs(&src).unwrap();
        assert_eq!(names, vec!["mod_bar", "mod_foo"]);
    }

    // -----------------------------------------------------------------------
    // Full merge flow on a synthetic directory
    // -----------------------------------------------------------------------

    #[test]
    fn merge_produces_merged_rs_files() {
        let tmp = TempDir::new().unwrap();
        let feature_root = tmp.path().join(".c2rust/default");

        // Build a minimal rust/src structure
        let src = feature_root.join("rust/src");
        let mod_dir = src.join("mod_foo");
        fs::create_dir_all(&mod_dir).unwrap();

        // mod.rs – minimal, produced by bindgen/init (no `mod fun_*;`)
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

        // fun_add.rs – a stub Rust implementation
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

        // lib.rs – generated by init
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

        // src.2/mod_foo.rs should exist
        let merged = feature_root.join("rust/src.2/mod_foo.rs");
        assert!(merged.exists(), "merged file should be created");

        // src.2/lib.rs should exist
        let lib = feature_root.join("rust/src.2/lib.rs");
        assert!(lib.exists(), "lib.rs should be created in src.2");

        // rust/src should now be a symlink pointing to src.2
        let src_link = feature_root.join("rust/src");
        assert!(src_link.is_symlink(), "rust/src should be a symlink after merge");

        // The merged file should contain the `add` function body
        let merged_content = fs::read_to_string(&merged).unwrap();
        assert!(
            merged_content.contains("fn add"),
            "merged file should contain the add function"
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
        assert_eq!(
            Feature::foreign_item_name(&item),
            Some("ext".to_string())
        );

        let item: syn::ForeignItem = syn::parse_str("static EXT_VAR: i32;").unwrap();
        assert_eq!(
            Feature::foreign_item_name(&item),
            Some("EXT_VAR".to_string())
        );
    }

    #[test]
    fn is_use_super_variants() {
        let yes: syn::ItemUse = syn::parse_str("use super::*;").unwrap();
        assert!(Feature::is_use_super(&yes));

        let no: syn::ItemUse = syn::parse_str("use super::SomeType;").unwrap();
        assert!(!Feature::is_use_super(&no));

        let no2: syn::ItemUse = syn::parse_str("use crate::foo::*;").unwrap();
        assert!(!Feature::is_use_super(&no2));
    }

    #[test]
    fn remove_private_attr_strips_c2rust_private() {
        let mut fn_item: syn::ItemFn =
            syn::parse_str("#[_c2rust_private_abc] fn test() {}").unwrap();
        assert!(Feature::remove_private_attr(&mut fn_item.attrs));
        assert!(fn_item.attrs.is_empty());

        let mut fn_item: syn::ItemFn = syn::parse_str("#[inline] fn test() {}").unwrap();
        assert!(!Feature::remove_private_attr(&mut fn_item.attrs));
        assert_eq!(fn_item.attrs.len(), 1);
    }

    #[test]
    fn merge_item_static_prepends_helper_items() {
        let items = vec![syn::parse_str("const HELPER: i32 = 1;").unwrap()];
        let mut static_item: syn::ItemStatic =
            syn::parse_str("static mut VAR: i32 = 42;").unwrap();
        Feature::merge_item_static(items, &mut static_item);
        let code = quote::quote!(#static_item).to_string();
        assert!(code.contains("HELPER"));
        assert!(code.contains("42"));
    }
}
