# c2rust-demo

`c2rust-demo` 是一个面向 C 项目的命令行工具，当前提供两步流程：

1. `init`：捕获 C 构建过程并生成按符号拆分的 Rust 脚手架。
2. `merge`：将 `init` 产出的按符号文件合并为按模块文件，并汇总共享 FFI 声明。

## 当前能力范围

- ✅ 已实现：`init`、`merge`
- ❌ 未实现：`update`、`reinit`、`sync`

## 核心流程

```text
C 项目目录
   │
   ├─ c2rust-demo init -- <构建命令>
   │    ├─ 编译 hook/libhook.so
   │    ├─ 通过 LD_PRELOAD 注入构建过程，捕获 .c2rust 与 .c2rust.opts
   │    ├─ 交互式选择参与转换的文件（非交互环境自动全选）
   │    ├─ 调用 bindgen 生成各模块类型/声明
   │    └─ 生成 .c2rust/<feature>/rust 及 init-interface-report.md
   │
   └─ c2rust-demo merge [--feature <name>]
        ├─ 合并 rust/src/mod_*/ 下的 fun_*.rs、var_*.rs
        ├─ 去重跨模块重复 FFI 并上提到 lib.rs
        ├─ 输出 rust/src.2（合并结果）
        ├─ 备份 rust/src.1（init 原始结果）
        └─ 将 rust/src 置为指向 src.2 的符号链接，并生成 merge-interface-report.md
```

## 项目结构（关键文件）

- `src/main.rs`：CLI 入口（`init` / `merge`）
- `src/capture.rs`：hook 构建与带环境变量的构建命令执行
- `src/layout.rs`：`.c2rust/<feature>/` 目录与元数据管理
- `src/selector.rs`：交互式文件选择（`dialoguer`）
- `src/split/feature.rs`：`init` 阶段 Rust 脚手架与报告生成
- `src/split/merge.rs`：`merge` 阶段合并、FFI 去重与报告生成
- `hook/`：`libhook.so` 源码与 Makefile
- `tests/`：单元测试 + 集成测试
- `scripts/validate-cjson.sh`：对 cJSON 的端到端验证脚本（与 CI 对齐）

## 环境要求

- Linux（依赖 `LD_PRELOAD` 和 Unix 符号链接）
- Rust / Cargo（`Cargo.toml` 要求 `rust-version = 1.82`）
- `gcc`、`make`（用于构建 `hook/libhook.so`）
- `bindgen-cli`（`init` 阶段需要 `bindgen` 命令）
- clang / libclang 环境（供底层分析与 bindgen 相关流程使用）

安装 `bindgen-cli`：

```bash
cargo install bindgen-cli
```

## 构建

```bash
cargo build
```

发布构建：

```bash
cargo build --release
```

## 使用方式

### 1) init：捕获构建并生成初始 Rust 结构

在目标 C 项目根目录（或其子目录）执行：

```bash
c2rust-demo init -- <你的构建命令>
```

示例：

```bash
c2rust-demo init -- make
c2rust-demo init --feature foo -- make -j4
c2rust-demo init -- gcc -c cJSON.c -I.
```

说明：

- `--feature` 默认值为 `default`
- `--` 之后的所有参数会原样作为构建命令传入

### 2) merge：将按符号文件合并为按模块文件

```bash
c2rust-demo merge
c2rust-demo merge --feature foo
```

`merge` 需要先完成对应 feature 的 `init`。

## 输入与输出说明

### 输入（init）

- 必填：构建命令（`BUILD_CMD...`）
- 可选：`--feature <name>`

### 输出目录

`init` 后（示意）：

```text
.c2rust/<feature>/
├── c/                       # 捕获到的 .c2rust / .c2rust.opts / targets.list
├── meta/
│   ├── build_cmd.txt
│   ├── selected_files.json
│   └── init-interface-report.md
└── rust/
    ├── Cargo.toml
    └── src/
        ├── lib.rs
        ├── lib.normalized
        └── mod_*/
            ├── mod.rs
            ├── mod.normalized
            ├── fun_*.rs / fun_*.c
            ├── var_*.rs / var_*.c
            └── decl_*.rs
```

`merge` 后（在上面基础上新增/调整）：

```text
.c2rust/<feature>/
├── meta/
│   └── merge-interface-report.md
└── rust/
    ├── src.1/               # init 原始输出备份
    ├── src -> src.2         # 符号链接
    └── src.2/               # 合并后输出（含 lib.rs 与 mod_*.rs）
```

## 开发与测试

运行全部测试：

```bash
cargo test
```

仅运行集成测试：

```bash
cargo test --test integration
```

集成测试会自动检测外部工具（如 `gcc`、`make`、`clang`、`bindgen`），缺失时打印跳过信息。

可在本地执行 cJSON 验证脚本：

```bash
./scripts/validate-cjson.sh
```

## 可选环境变量

- `C2RUST_CLANG`：覆盖默认 `clang` 可执行文件名
- `C2RUST_REMOVE_STATIC`：设为非空时启用 static/inline 公开化步骤
- `C2RUST_CC`：hook 识别的编译器名称（默认自动匹配 `gcc/clang/cc` 及带版本后缀）
- `C2RUST_LD`：hook 识别的链接器名称（默认自动匹配 `ld/lld`）
- `C2RUST_DEBUG`：设为非空时输出 hook 调试日志到 stderr
- `C2RUST_COV`：设为非空时启用 C 侧测试覆盖率插桩（Case 2：自动用 clang 编译带插桩的 .o）
- `C2RUST_COV_INSTRUMENTED`：与 `C2RUST_COV` 同时设置时表示 Case 1（C 构建系统已自行插桩），hook 只收集 .a 路径，不重复编译

## 测试覆盖率（C + Rust 联合）

`cargo llvm-cov` 可统一采集 Rust 和 C 代码的覆盖率数据，前提是 C 代码用 clang 的 LLVM 插桩方式编译。

### 依赖

```bash
cargo install cargo-llvm-cov   # 需要 LLVM/clang 工具链
which clang ar                  # 必须在 PATH 中
```

### Case 1：C 构建系统已支持 LLVM 插桩

如果你的 C 构建系统（如 CMake）可以通过某个选项使 clang 以 `-fprofile-instr-generate -fcoverage-mapping` 编译，只需在运行 `init` 时同时设置两个变量，工具会从 hook 捕获的链接目标列表中自动提取 `.a` 路径：

```bash
C2RUST_COV=1 C2RUST_COV_INSTRUMENTED=1 c2rust-demo init -- <你的构建命令>
```

### Case 2：C 构建系统不支持插桩（默认自动插桩）

只设置 `C2RUST_COV=1`，hook 会在截获每次编译调用时额外 fork 一个 clang 进程，用 LLVM 插桩方式再编译一次，产物写入 `.c2rust/<feature>/cov_obj/`，init 完成后打包为 `libcov.a`：

```bash
C2RUST_COV=1 c2rust-demo init -- <你的构建命令>
```

两种 Case 完成后，`init` 会在生成的 Rust 项目中自动写入 `rust/build.rs`，链接插桩的 `libcov.a`，并在 `rust/Cargo.toml` 中注册 `build = "build.rs"`。

### 运行覆盖率

```bash
cd .c2rust/<feature>/rust
LLVM_PROFILE_FILE="cov-%p-%m.profraw" cargo llvm-cov --lcov --output-path lcov.info
```

查看 HTML 报告：

```bash
cargo llvm-cov report --html
```

> **说明**：`LLVM_PROFILE_FILE` 可控制 `.profraw` 文件的存放路径。C 侧函数的覆盖数据包含在同一份 profraw 中，`llvm-cov` 会一并展示。

## 注意事项

- 目前仅支持 Linux。
- `merge` 会调整 `rust/src` 为符号链接；如需查看 `init` 原始结果，请看 `rust/src.1`。
- 本仓库根目录包含历史归档文件（如 `cJSON-*.rar`），不参与 `c2rust-demo` 运行逻辑。
