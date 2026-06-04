# c2rust-demo — Agent Instructions

## 项目概述

`c2rust-demo` 是一个将 C 项目迁移到 Rust 的命令行工具，分两步完成：

1. **`init`**：通过 `LD_PRELOAD` hook 捕获 C 构建过程，调用 `bindgen` 生成按符号拆分的 Rust FFI 脚手架。
2. **`merge`**：将按符号的 Rust 文件合并为按模块文件，去重跨模块共享的 FFI 声明并上提到 `lib.rs`。

## 构建与测试

```bash
# 构建
cargo build

# 运行全部测试（49 个，集成测试遇到缺失外部工具会自动跳过）
cargo test

# 仅运行集成测试
cargo test --test integration

# 覆盖率（需要 cargo-llvm-cov）
cargo llvm-cov --summary-only -- --test-threads=1
```

## 关键文件

| 文件 | 职责 |
|------|------|
| `src/main.rs` | CLI 入口（`init` / `merge` 子命令） |
| `src/capture.rs` | 构建 `libhook.so`、带 `LD_PRELOAD` 执行构建命令、处理覆盖率库 |
| `src/layout.rs` | `.c2rust/<feature>/` 目录与元数据管理 |
| `src/selector.rs` | 交互式文件选择（非 TTY 时自动全选） |
| `src/split/feature.rs` | `init` 阶段：生成 Rust 脚手架与 `init-interface-report.md` |
| `src/split/merge.rs` | `merge` 阶段：合并、FFI 去重、`merge-interface-report.md`，最后将 `rust/src` 置为指向 `src.2` 的符号链接 |
| `hook/` | `libhook.so` C 源码与 Makefile |
| `tests/integration.rs` | 集成测试，自动检测外部工具 |

## 输出结构

### init 后

```
.c2rust/<feature>/
├── c/                       # 捕获的 .c2rust / .c2rust.opts
├── meta/
│   ├── build_cmd.txt
│   ├── selected_files.json
│   └── init-interface-report.md
└── rust/
    ├── Cargo.toml
    └── src/
        ├── lib.rs
        └── mod_<name>/
            ├── mod.rs
            ├── fun_<sym>.rs   # 函数存根
            ├── var_<sym>.rs   # 全局变量存根
            └── decl_<sym>.rs  # 类型/声明
```

### merge 后（增量）

```
.c2rust/<feature>/
├── meta/
│   └── merge-interface-report.md
└── rust/
    ├── src.1/          # init 原始输出备份
    ├── src -> src.2    # 符号链接
    └── src.2/          # 合并后输出（lib.rs + mod_<name>.rs）
```

## generate-rust-ffi 任务说明

当需要为一个 C 项目生成 Rust FFI 绑定时，遵循以下流程：

### 步骤 1：运行 init

```bash
# 在 C 项目根目录执行
c2rust-demo init -- <构建命令>

# 示例
c2rust-demo init -- make
c2rust-demo init --feature my_feature -- gcc -c foo.c -I.
```

init 完成后，检查 `.c2rust/<feature>/meta/init-interface-report.md` 了解生成的接口清单。

### 步骤 2：运行 merge

```bash
c2rust-demo merge
# 或指定 feature
c2rust-demo merge --feature my_feature
```

merge 完成后，查看 `.c2rust/<feature>/meta/merge-interface-report.md` 确认 FFI 汇总结果。

### 步骤 3：实现 Rust 函数体

merge 后，在 `.c2rust/<feature>/rust/src/` 下每个 `mod_<name>.rs` 中填写各函数的 Rust 实现，替换生成的 `todo!()` 占位符。FFI 声明已由工具自动整合到 `lib.rs`，无需手动处理。

## 可选环境变量

| 变量 | 说明 |
|------|------|
| `C2RUST_CLANG` | 覆盖默认 `clang` 可执行文件 |
| `C2RUST_CC` | hook 识别的编译器名称（默认匹配 `gcc/clang/cc`） |
| `C2RUST_LD` | hook 识别的链接器名称（默认匹配 `ld/lld`） |
| `C2RUST_REMOVE_STATIC` | 非空时启用 static/inline 公开化 |
| `C2RUST_DEBUG` | 非空时输出 hook 调试日志到 stderr |
| `C2RUST_COV` | 启用 C 侧 LLVM 覆盖率插桩 |
| `C2RUST_COV_INSTRUMENTED` | 与 `C2RUST_COV` 同用，表示 C 构建系统已自行插桩 |

## 环境要求

- Linux（依赖 `LD_PRELOAD` 与 Unix 符号链接）
- Rust 1.82+
- `gcc`、`make`（构建 `libhook.so`）
- `bindgen-cli`（`cargo install bindgen-cli`）
- clang / libclang

## 代码风格约定

- 错误处理统一使用 `anyhow::Result`（见 `src/error.rs`）
- 路径操作优先使用 `std::path::Path` / `PathBuf`，避免字符串拼接
- 测试中外部工具缺失时使用 `eprintln!` + `return` 跳过，不 `panic`
- 新增功能需在 `tests/integration.rs` 中补充集成测试
