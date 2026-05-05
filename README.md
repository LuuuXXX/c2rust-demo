# c2rust-demo

A minimal but usable CLI tool that integrates C-build capture with Rust scaffolding generation, combining the capabilities of [`c2rust-build`](https://github.com/LuuuXXX/c2rust-build) and [`c2rust-code-analyse`](https://github.com/LuuuXXX/c2rust-code-analyse).

## Current scope

Only `init` is implemented.  `update`, `reinit`, `merge`, and `sync` are **not** part of this first version.

## How it works

```
Your C project
     │
     ▼
c2rust-demo init -- make
     │
     ├─ Build libhook.so (hook/hook.c)
     ├─ Run make with LD_PRELOAD=libhook.so
     │    └─ hook intercepts gcc/clang and preprocesses each .c file
     │         → .c2rust/<feature>/c/**/*.c2rust
     │         → .c2rust/<feature>/c/**/*.c2rust.opts
     │         → .c2rust/<feature>/c/targets.list  (if linker is invoked)
     │
     ├─ Interactive file selection
     │    → .c2rust/<feature>/meta/selected_files.json
     │
     └─ Init split (via clang AST + bindgen)
          → .c2rust/<feature>/rust/  (cargo new --lib)
          → rust/src/lib.rs + lib.normalized
          → rust/src/mod_<file>/
               ├── mod.rs + mod.normalized
               ├── fun_<name>.rs  (one per function)
               ├── var_<name>.rs  (one per variable)
               ├── decl_<name>.rs (FFI declarations)
               └── <name>.c       (normalized C source)
```

## Dependencies

| Tool | Required | Notes |
|------|----------|-------|
| Linux | ✓ | `LD_PRELOAD` hook requires Linux |
| Rust / cargo | ✓ | ≥ 1.82 |
| gcc | ✓ | For building `libhook.so` and C preprocessing |
| make | Recommended | Or any other build tool |
| clang | ✓ | For AST dump (`-ast-dump=json`) |
| bindgen | ✓ | Generates `mod.rs` from C headers |

Install `bindgen`:
```bash
cargo install bindgen-cli
```

## Usage

### Basic

```bash
# In the root of your C project:
c2rust-demo init -- make
```

### With a custom feature name

```bash
c2rust-demo init --feature foo -- make -j4
```

### Pass arguments through `--`

Everything after `--` is treated as the build command:

```bash
c2rust-demo init -- cmake --build build/
c2rust-demo init -- ninja -C out/
```

## Output structure

```
.c2rust/<feature>/
├── c/                  Build capture output
│   ├── src/
│   │   ├── foo.c2rust          Preprocessed C (for AST analysis)
│   │   └── foo.c2rust.opts     Compiler flags used
│   └── targets.list            Link targets (if any)
├── meta/               Metadata
│   ├── build_cmd.txt           The original build command
│   └── selected_files.json     Files the user chose to include
└── rust/               Generated Rust project (cargo lib)
    ├── Cargo.toml
    └── src/
        ├── lib.rs              Module re-exports + crate attributes
        ├── lib.normalized      Baseline copy of lib.rs
        └── mod_src_foo/        One directory per C source file
            ├── mod.rs          bindgen output (normalized)
            ├── mod.normalized  Baseline copy of mod.rs
            ├── fun_add.rs      Stub for function `add`
            ├── fun_add.c       Normalized C code for `add`
            ├── decl_add.rs     FFI declaration for `add`
            └── ...
```

## Interactive file selection

After the build-capture phase, `c2rust-demo` scans for captured `.c2rust` files and presents a multi-select prompt (powered by [`dialoguer`](https://crates.io/crates/dialoguer)):

```
Select files to include in this feature (space to toggle, enter to confirm)
> [ ] src/foo.c2rust
  [x] src/bar.c2rust
  [x] src/baz.c2rust
```

- Use **space** to toggle individual files.
- Press **Enter** to confirm.
- Files that are **not** selected are recorded but excluded from the Rust scaffolding step.
- When stdin is not a terminal (CI, scripts, pipes) all files are selected automatically.

## Running tests

Unit tests (no toolchain required):

```bash
cargo test
```

Integration tests auto-detect whether the required tools (gcc, make, clang, bindgen) are
available and print a clear skip message if any are missing:

```bash
cargo test --test integration
```

## Manual validation with cJSON

> **Note:** This section is for manual validation only. These steps are **not** part of the
> normal CI test suite and will not run automatically.

The helper script `scripts/validate-cjson.sh` exercises `c2rust-demo init` and
`c2rust-demo merge` against the real-world [`DaveGamble/cJSON`](https://github.com/DaveGamble/cJSON)
project. It clones the repository, runs both commands, and prints the resulting output
structure along with pass/fail indicators for each expected artifact.

### Prerequisites

| Tool | Notes |
|------|-------|
| Linux | LD_PRELOAD hook required |
| git | For cloning cJSON |
| Rust / cargo ≥ 1.82 | Build c2rust-demo |
| gcc | Build libhook.so |
| make | Run the cJSON build |
| clang | AST dump |
| bindgen | `cargo install bindgen-cli` |

### Running the script

```bash
# Quick run (cleans up automatically)
bash scripts/validate-cjson.sh

# Keep the working directory for inspection
bash scripts/validate-cjson.sh --keep-workdir
```

### Expected terminal output

```
[INFO]  Checking prerequisites...
[OK]    All required tools are present.
[INFO]  Building c2rust-demo (cargo build --release)...
[OK]    Binary built at .../target/release/c2rust-demo
[INFO]  Working directory: /tmp/c2rust-demo-cjson-XXXXXX
[INFO]  Cloning DaveGamble/cJSON into .../cJSON...
[OK]    cJSON cloned.
[INFO]  Running: c2rust-demo init -- make
------------------------------------------------------------
=== c2rust-demo init ===
Project root : .../cJSON
Feature      : default
Build command: make

Captured N .c2rust file(s)
N file(s) selected for this feature

Running init split...
...
✓ c2rust-demo init completed successfully!
------------------------------------------------------------
[OK]    c2rust-demo init completed.
[OK]    c/          directory exists
[OK]    meta/       directory exists
[OK]    meta/build_cmd.txt exists
[OK]    meta/selected_files.json exists
[OK]    rust/       directory exists
[OK]    rust/Cargo.toml exists
[OK]    rust/src/lib.rs exists
[OK]    Found N mod_* director(ies) under rust/src/
[INFO]  Running: c2rust-demo merge
------------------------------------------------------------
=== c2rust-demo merge ===
...
✓ c2rust-demo merge completed successfully!
------------------------------------------------------------
[OK]    c2rust-demo merge completed.
==============================
  Validation complete
==============================
```

### Expected output structure after `init`

```
.c2rust/default/
├── c/
│   └── cJSON.c2rust          (preprocessed C source captured by the hook)
├── meta/
│   ├── build_cmd.txt         (contains "make")
│   └── selected_files.json   (list of selected .c2rust files)
└── rust/
    ├── Cargo.toml
    └── src/
        ├── lib.rs
        ├── lib.normalized
        └── mod_cjson/        (one directory per captured C file)
            ├── mod.rs
            ├── mod.normalized
            ├── fun_cJSON_Parse.rs
            ├── fun_cJSON_Parse.c
            ├── decl_cJSON_Parse.rs
            └── ...
```

### Expected output structure after `merge`

```
.c2rust/default/rust/
├── src/            (original init output — untouched)
└── src.2/          (merged output — one .rs per mod_xxx)
    ├── lib.rs
    └── mod_cjson.rs
```

### Running as an ignored integration test

An `#[ignore]` test is also available for opt-in use in automated pipelines
that have the full toolchain and network access:

```bash
cargo test --test integration cjson_manual_validation -- --ignored
```

## Current limitations

- **Linux only** – relies on `LD_PRELOAD`.
- **Only `init`** – `update`, `reinit`, `merge`, `sync` are not implemented.
- `set_lint_rules` (cargo lint config) is not implemented in this version; lint configuration must be managed manually if desired.
- The clang binary used for AST dumps can be overridden with the `C2RUST_CLANG` environment variable (defaults to `clang`).
- The hook intercepts `gcc` / `clang` / `cc` by default; set `C2RUST_CC` to use a different compiler name, and `C2RUST_LD` for a different linker.
