---
name: generate-rust-ffi
description: >
  为任意 C 项目生成 Rust FFI 脚手架（`cargo check` 通过），并可选地输出 C 侧测试覆盖率。
  适用场景：将 C 库集成到 Rust 项目、对 C 代码进行 Rust 侧测试、评估 C 代码的测试覆盖情况。
  触发词：生成 FFI、Rust 绑定、C to Rust、c2rust、FFI 脚手架、C 覆盖率、rust ffi。
groups:
  - public
---

# 为 C 项目生成 Rust FFI 脚手架

使用 `c2rust-demo` 工具为任意 C 项目自动完成以下工作：

1. **生成 Rust FFI 脚手架**：通过拦截 C 构建过程，自动产出通过 `cargo check` 的 Rust FFI crate
2. **（可选）输出 C 侧测试覆盖率**：在 Rust 测试中调用 C 函数，获得 C 源文件的函数/行覆盖率报告

---

## 前置条件

| 工具 | 用途 | 安装方式 |
|------|------|----------|
| `c2rust-demo` | 主工具 | `cargo install c2rust-demo` 或本地 `cargo build --release` |
| `gcc` | 编译 hook 库（`libhook.so`） | 系统包管理器 |
| `clang` + `libclang-dev` | bindgen 生成类型绑定；覆盖率插桩（可选） | 系统包管理器 |
| `bindgen` | 生成 Rust FFI 类型 | `cargo install bindgen-cli` |
| `cargo-llvm-cov` | 输出 C+Rust 联合覆盖率（仅覆盖率模式需要） | `cargo install cargo-llvm-cov` |
| Rust stable + `llvm-tools-preview` | Rust 工具链 | `rustup component add llvm-tools-preview` |

> **覆盖率模式注意**：C 侧覆盖率插桩依赖 LLVM（`-fprofile-instr-generate -fcoverage-mapping`），
> 必须使用 `clang` 而非 `gcc`。若项目原本使用 `gcc`，覆盖率模式下需替换为 `clang`，
> 或设置 `C2RUST_CC=clang` 让 hook 仅在插桩编译步骤改用 clang。

---

## 步骤一：运行 init（拦截 C 构建）

### 1a. 询问用户的构建命令

**向用户提问**：

> 请提供您的 C 项目构建命令（例如 `make -j4`、`cmake --build build`、`gcc -c foo.c -I.`）。

等待用户回答后，记录构建命令为 `<用户构建命令>`。

### 1b. 判断构建命令是否支持覆盖率插桩

覆盖率插桩要求 C 源文件使用 **clang** 编译（`-fprofile-instr-generate -fcoverage-mapping`）。根据用户提供的构建命令，**向用户说明并询问**：

> 覆盖率模式需要用 clang 编译 C 源文件。
> 请判断您的构建命令是否满足以下任一条件：
> 1. 命令中已使用 `clang`（如 `make CC=clang` 或 `cmake -DCMAKE_C_COMPILER=clang`）
> 2. 可以通过添加 `CC=clang` 参数切换为 clang（如 `make CC=clang -j4`）
> 3. 不方便修改构建命令，但可以接受使用 `C2RUST_CC=clang` 让工具仅在插桩步骤替换编译器
>
> **您是否需要输出 C 侧测试覆盖率？** 如需要，请确认上述哪种方式适合您的项目。

根据用户回答决定模式：

| 用户回答 | 执行模式 |
|----------|----------|
| 不需要覆盖率 | 基础模式，不加 `C2RUST_COV=1` |
| 需要覆盖率，且构建命令已使用或可切换到 clang | 覆盖率模式：`C2RUST_COV=1`，并在构建命令中添加 `CC=clang`（或等效方式） |
| 需要覆盖率，但构建命令难以替换编译器 | 覆盖率模式：`C2RUST_CC=clang C2RUST_COV=1`，构建命令保持原样 |

### 1c. 执行 init

在 C 项目根目录执行 `c2rust-demo init`，将原有构建命令原样传入：

```bash
cd /path/to/my-c-project

# 基础模式（仅生成 FFI，无覆盖率）
c2rust-demo init -- <用户构建命令>

# 覆盖率模式 —— 构建命令可切换 clang
C2RUST_COV=1 c2rust-demo init -- <用户构建命令（含 CC=clang）>

# 覆盖率模式 —— 构建命令难以替换编译器时
C2RUST_CC=clang C2RUST_COV=1 c2rust-demo init -- <用户构建命令>
```

**常见构建命令示例：**

| 场景 | 命令 |
|------|------|
| 单文件 | `c2rust-demo init -- gcc -c foo.c -I.` |
| Makefile | `c2rust-demo init -- make -j4` |
| Makefile + 覆盖率 | `C2RUST_COV=1 c2rust-demo init -- make CC=clang -j4` |
| CMake | `c2rust-demo init -- cmake --build build -j4` |
| CMake + 覆盖率 | `C2RUST_COV=1 c2rust-demo init -- cmake --build build -j4` |

**init 完成后，输出结构：**

```
.c2rust/default/
├── c/              ← 捕获的预处理文件（.c2rust / .c2rust.opts）
├── cov/            ← 仅覆盖率模式生成
│   └── libcov.a   ← 插桩后的 C 静态库
├── meta/
│   ├── build_cmd.txt
│   ├── cov_lib.txt ← libcov.a 路径（仅覆盖率模式）
│   └── init-interface-report.md
└── rust/
    ├── Cargo.toml
    ├── build.rs    ← 仅覆盖率模式生成，链接 libcov.a
    └── src/
        ├── lib.rs
        └── mod_*/  ← 每个 C 翻译单元对应一个目录
```

---

## 步骤二：运行 merge（合并为按模块文件）

```bash
c2rust-demo merge
```

`merge` 将 `rust/src/mod_*/` 下按符号拆分的文件合并为按模块组织的文件，并去除跨模块重复的 FFI 声明：

```
.c2rust/default/rust/
├── src.1/          ← init 原始输出备份
├── src -> src.2    ← 符号链接（始终指向最新输出）
└── src.2/          ← 合并后的最终输出
    ├── lib.rs
    └── mod_*.rs
```

---

## 步骤三：验证 cargo check，按需迭代修复

生成的 Rust 项目使用了 `#![feature(linkage)]`，需要 `RUSTC_BOOTSTRAP=1`：

```bash
RUSTC_BOOTSTRAP=1 cargo check \
  --manifest-path .c2rust/default/rust/Cargo.toml \
  2>&1 | tee /tmp/cargo-check.log

echo "exit code: $?"
```

### 若 cargo check 通过

直接进入步骤四（如需覆盖率）或结束。

### 若 cargo check 失败 — 迭代修复循环

持续循环，无次数上限，每轮：

1. 读取 `/tmp/cargo-check.log`，提取所有 `error[E...]` 条目
2. 按下表修复：

   | 错误码 | 含义 | 修复策略 |
   |--------|------|----------|
   | `E0412` | 找不到类型 `X` | 在对应 `.rs` 文件头部添加 `use ::core::ffi::*;` 或 `type X = ...;` 别名 |
   | `E0428` | 名称重复定义 | 检查 `lib.rs` 和 `mod_*.rs` 中重复的 `pub use`，移除多余项 |
   | `E0023` | 模式绑定类型错误 | 核对 FFI 函数签名与 bindgen 生成的类型，修正参数/返回值类型 |
   | `E0133` | 不安全代码缺少 `unsafe` 块 | 将对应函数体包裹在 `unsafe { }` 中 |
   | `E0308` | 类型不匹配 | 添加 `as` 显式转换或修正 FFI 签名 |
   | `E0601` | 找不到 `main` | 忽略（staticlib 无需 main） |

3. 修改 `.c2rust/default/rust/src.2/` 下的 `.rs` 文件（不要改动 `lib.rs` 中的 `#![allow(...)]` 属性）
4. 重新运行 `cargo check`
5. 每完成 **10 轮**仍未通过时，向用户汇报当前剩余错误，并询问是否继续修复；用户确认后继续，用户放弃则停止并报告现状

> **提示**：生成的 `lib.rs` 已包含大量 `#![allow(...)]`，`unused_imports` / `dead_code`
> 等 **warning** 可安全忽略。

---

## 步骤四（可选）：在 Rust 测试中调用 C 函数并输出 C 侧覆盖率

> **仅当 `init` 时设置了 `C2RUST_COV=1` 才可执行此步骤。**

### 4a. 编写 FFI 集成测试

在 `.c2rust/default/rust/tests/ffi_smoke.rs` 中创建测试（目录不存在时先创建）。

> **重要**：仅写 `link_succeeds` 占位测试会导致覆盖率 TOTAL=0，因为没有执行任何 C 函数。
> 必须在测试中调用真实 C 函数，LLVM 才能记录 C 侧的覆盖率数据。

**第一步：查看生成的 FFI 绑定，找出可用的 C 函数**

```bash
# 查看合并后的 Rust 源文件，找出已绑定的 C 函数名
grep -h "pub fn\|pub unsafe fn" .c2rust/default/rust/src.2/*.rs | head -20
```

**第二步：编写调用这些 C 函数的测试**

在 `tests/ffi_smoke.rs` 中，用 `extern "C"` 块声明要调用的函数，然后在测试中调用它们：

```rust
// tests/ffi_smoke.rs
// 调用生成的 FFI 函数以触发 C 侧代码路径，产生覆盖率数据。
// 根据实际 C 项目 API 替换函数名和签名。
#![allow(non_snake_case)]

use std::os::raw::{c_char, c_void};

// 示例：声明要测试的 C 函数（根据实际 API 修改）
extern "C" {
    // 替换为 C 项目实际暴露的函数
    // fn my_function(arg: c_int) -> c_int;
    // fn my_init() -> *mut c_void;
    // fn my_cleanup(ctx: *mut c_void);
}

// 示例测试（根据实际 C API 替换）：
// #[test]
// fn smoke_my_function() {
//     let result = unsafe { my_function(0) };
//     assert_eq!(result, 0);
// }

// 最简验证（仅确认链接成功，不产生 C 侧覆盖率）：
#[test]
fn link_succeeds() {
    let _ = 42_u64.wrapping_add(1);
}
```

**如何根据 C 项目 API 补充真实测试：**

1. 运行 `grep "pub fn\|extern \"C\"" .c2rust/default/rust/src.2/*.rs` 找出所有绑定函数
2. 选取几个有代表性的函数（初始化/清理、简单计算、字符串处理等）
3. 在 `unsafe {}` 块中调用这些函数，验证返回值

**以 cJSON 项目为例**（该 C 项目暴露了 `cJSON_Version`、`cJSON_CreateNull`、`cJSON_Delete` 等函数）：

```rust
#![allow(non_snake_case)]
use std::os::raw::{c_char, c_void};

extern "C" {
    fn cJSON_Version() -> *const c_char;
    fn cJSON_CreateNull() -> *mut c_void;
    fn cJSON_Delete(item: *mut c_void);
    fn cJSON_Parse(value: *const c_char) -> *mut c_void;
}

#[test]
fn cjson_version_not_null() {
    let v = unsafe { cJSON_Version() };
    assert!(!v.is_null());
}

#[test]
fn cjson_create_and_delete_null() {
    let item = unsafe { cJSON_CreateNull() };
    assert!(!item.is_null());
    unsafe { cJSON_Delete(item) };
}

#[test]
fn cjson_parse_simple() {
    let json = b"true\0".as_ptr() as *const c_char;
    let item = unsafe { cJSON_Parse(json) };
    assert!(!item.is_null());
    unsafe { cJSON_Delete(item) };
}
```

在 `Cargo.toml` 中注册测试：

```toml
[[test]]
name = "ffi_smoke"
path = "tests/ffi_smoke.rs"
```

### 4b. 运行 cargo llvm-cov 输出覆盖率报告

```bash
# 终端摘要（包含 C 侧文件覆盖率）
RUSTC_BOOTSTRAP=1 cargo llvm-cov \
  --manifest-path .c2rust/default/rust/Cargo.toml \
  --summary-only

# 详细 lcov 报告（可导入 Codecov / Coveralls 等平台）
RUSTC_BOOTSTRAP=1 cargo llvm-cov \
  --manifest-path .c2rust/default/rust/Cargo.toml \
  --lcov --output-path c-coverage.lcov

# HTML 可视化报告（在浏览器中查看每行覆盖情况）
RUSTC_BOOTSTRAP=1 cargo llvm-cov \
  --manifest-path .c2rust/default/rust/Cargo.toml \
  --html --output-dir cov-html
# 打开 cov-html/index.html 查看交互式覆盖率报告
```

报告同时包含：
- **Rust 侧**：`lib.rs`、`mod_*.rs` 等文件的行覆盖率
- **C 侧**：原始 C 源文件（如 `foo.c`、`bar.c`）的函数/行覆盖率

> **注意**：若摘要中 C 源文件的覆盖率为 0 或 C 文件根本不出现，说明测试没有调用任何 C 函数。
> 请检查 `tests/ffi_smoke.rs` 是否包含了真实的 C 函数调用（参见 4a 步骤）。

> **原理**：`build.rs` 令测试二进制链接了 `libcov.a`（clang 插桩编译的目标文件）。
> 当测试调用 C 函数时，LLVM profdata 记录对应 C 源码路径；`cargo llvm-cov` 合并后即可输出 C 文件覆盖率。

---

## 快速参考：完整命令序列

**基础模式（仅生成 FFI）：**

```bash
cd /path/to/my-c-project
c2rust-demo init -- make -j4          # 替换为实际构建命令
c2rust-demo merge
RUSTC_BOOTSTRAP=1 cargo check \
  --manifest-path .c2rust/default/rust/Cargo.toml
```

**覆盖率模式（FFI + C 侧覆盖率）：**

```bash
cd /path/to/my-c-project
C2RUST_COV=1 c2rust-demo init -- make CC=clang -j4
c2rust-demo merge
RUSTC_BOOTSTRAP=1 cargo check \
  --manifest-path .c2rust/default/rust/Cargo.toml
# （如需修复错误，参考步骤三的迭代修复循环）

# 编写真实调用 C 函数的测试（见步骤四 4a）后运行覆盖率报告：
RUSTC_BOOTSTRAP=1 cargo llvm-cov \
  --manifest-path .c2rust/default/rust/Cargo.toml \
  --summary-only
# HTML 可视化报告（可选）：
RUSTC_BOOTSTRAP=1 cargo llvm-cov \
  --manifest-path .c2rust/default/rust/Cargo.toml \
  --html --output-dir cov-html
```

---

## 常见问题

**Q: init 时提示 "no .c2rust files were generated"**
A: 检查构建命令是否实际编译了 `.c` 文件（而非只是链接）。`c2rust-demo` 会自动从当前目录向上搜索项目根，一般无需手动设置 `C2RUST_PROJECT_ROOT`。

**Q: cargo check 报 `#![feature(linkage)] requires nightly`**
A: 设置 `RUSTC_BOOTSTRAP=1` 即可在 stable 工具链上启用该特性。生成的 `lib.rs` 顶部有注释说明。

**Q: 覆盖率报告中看不到 C 文件**
A: 确认 `init` 时设置了 `C2RUST_COV=1` 且使用了 clang 编译 C 源文件。检查 `.c2rust/default/meta/cov_lib.txt` 和 `.c2rust/default/rust/build.rs` 是否存在。

**Q: 项目使用 cmake / autoconf 等构建系统**
A: 直接将构建系统命令传给 `init`，通过环境变量覆盖编译器：
```bash
# cmake
C2RUST_COV=1 c2rust-demo init -- cmake --build build -j4

# autoconf / configure
C2RUST_COV=1 c2rust-demo init -- make CC=clang -j4

# 若不方便修改构建命令，用 C2RUST_CC=clang 仅替换插桩编译步骤的编译器
C2RUST_CC=clang C2RUST_COV=1 c2rust-demo init -- make -j4
```
