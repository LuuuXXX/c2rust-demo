# SKILL: 为 C 项目生成 Rust FFI 并输出 C 侧测试覆盖率

## 目标

使用 `c2rust-demo` 工具为给定的 C 项目：

1. 生成通过 `cargo check` 的 Rust FFI 脚手架项目
2. 输出 C 侧的测试覆盖率数据（哪些 C 函数/行被 Rust 测试覆盖）

---

## 前置条件

在开始之前，确认以下工具已安装：

| 工具 | 用途 |
|------|------|
| `c2rust-demo` | 主工具（`cargo install` 或本地 `cargo build --release`） |
| `gcc` | 编译 hook 库（`libhook.so`） |
| `clang` + `libclang-dev` | bindgen 生成类型绑定；C 覆盖率插桩 |
| `bindgen` | 生成 Rust FFI 类型（`cargo install bindgen-cli`） |
| `cargo-llvm-cov` | 输出 C+Rust 联合覆盖率（`cargo install cargo-llvm-cov`） |
| Rust 工具链（stable + `llvm-tools-preview`） | `rustup component add llvm-tools-preview` |

> **注意**：覆盖率插桩必须使用 `clang`（而非 `gcc`）作为 C 编译器，因为 LLVM 的
> `-fprofile-instr-generate -fcoverage-mapping` 只对 clang 有效。如果 C 项目原本
> 使用 `gcc`，在覆盖率模式下将构建命令中的编译器替换为 `clang`，或使用
> `C2RUST_CC=clang` 让 hook 改用 clang 进行插桩编译。

---

## 步骤一：在 C 项目目录中运行 init（启用覆盖率）

在 C 项目的根目录执行：

```bash
C2RUST_COV=1 c2rust-demo init -- <你的构建命令>
```

**示例（单文件项目）：**

```bash
cd /path/to/my-c-project
C2RUST_COV=1 c2rust-demo init -- clang -c foo.c -I.
```

**示例（使用 make 的项目）：**

```bash
cd /path/to/my-c-project
C2RUST_COV=1 c2rust-demo init -- make CC=clang -j4
```

`C2RUST_COV=1` 的效果：
- hook 拦截 C 编译调用，除生成 `.c2rust` 预处理文件外，还用 clang 额外编译一份
  带 `-fprofile-instr-generate -fcoverage-mapping` 标志的 `.o` 目标文件
- 将这些插桩 `.o` 文件打包为 `.c2rust/default/cov/libcov.a`
- 在 `meta/cov_lib.txt` 中记录该静态库的路径
- `init` 阶段在生成的 Rust 项目中自动写入 `build.rs`，令其链接 `libcov.a`

**init 完成后，输出结构示例：**

```
.c2rust/default/
├── c/              ← 捕获的 .c2rust / .c2rust.opts 文件
├── cov/
│   └── libcov.a   ← 插桩后的 C 静态库（覆盖率数据来源）
├── meta/
│   ├── build_cmd.txt
│   ├── cov_lib.txt ← libcov.a 的绝对路径
│   └── init-interface-report.md
└── rust/
    ├── Cargo.toml
    ├── build.rs    ← 自动生成，链接 libcov.a
    └── src/
        ├── lib.rs
        └── mod_*/  ← 每个 C 模块对应一个目录
```

---

## 步骤二：运行 merge

```bash
c2rust-demo merge
```

`merge` 将 `rust/src/mod_*/` 下的按符号文件合并为按模块文件，并去重跨模块重复的
FFI 声明：

```
.c2rust/default/rust/
├── src.1/          ← init 原始输出备份
├── src -> src.2    ← 符号链接
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

### 3a. 若 cargo check **通过**

继续步骤四。

### 3b. 若 cargo check **失败** — 迭代修复循环

最多循环 **5 次**，每轮执行：

1. **读取错误日志**（`/tmp/cargo-check.log`），提取所有 `error[E...]` 条目
2. **按错误类型分类处理**：

   | 错误码 | 含义 | 修复策略 |
   |--------|------|----------|
   | `E0412` | 找不到类型 `X` | 在对应 `.rs` 文件头部添加 `use ::core::ffi::*;` 或具体的 `type X = ...;` 别名 |
   | `E0428` | 名称 `X` 重复定义 | 检查 `lib.rs` 和 `mod_*.rs` 是否有重复 `pub use`，移除多余项 |
   | `E0023` | 模式绑定类型错误 | 检查 FFI 函数签名是否与 bindgen 生成的类型匹配，修正参数/返回值类型 |
   | `E0133` | 不安全代码无 `unsafe` 块 | 将对应函数体包裹在 `unsafe { }` 块中 |
   | `E0308` | 类型不匹配 | 添加显式类型转换（`as`）或修正 FFI 签名 |
   | `E0601` | 找不到 `main` | 忽略（staticlib 无需 main） |

3. **修改生成的 `.rs` 文件**（位于 `.c2rust/default/rust/src.2/`），不要修改
   `lib.rs` 中由工具生成的 `#![allow(...)]` 属性
4. **重新运行** `RUSTC_BOOTSTRAP=1 cargo check ...`
5. 若仍失败，继续下一轮；若 5 轮后仍失败，报告剩余错误并请求用户提供额外信息

> **提示**：生成的 `lib.rs` 已包含大量 `#![allow(...)]` 抑制常见告警，若仍报
> `unused_imports` 或 `dead_code` 等 **warning**，可安全忽略。

---

## 步骤四：在 Rust 测试中调用 C 函数并输出 C 侧覆盖率

### 4a. 在生成的 Rust 项目中编写测试

在 `.c2rust/default/rust/` 下创建集成测试文件
`.c2rust/default/rust/tests/ffi_smoke.rs`（目录不存在时先创建 `tests/` 目录）：

```rust
// tests/ffi_smoke.rs
// 调用生成的 FFI 函数以产生 C 侧覆盖率数据。
// 根据实际的 C 项目 API，将 <MODULE> 和函数名替换为真实名称。

// 示例（假设 C 项目暴露了 init/cleanup 函数）：
// #[test]
// fn smoke_init_cleanup() {
//     unsafe {
//         let ctx = <crate_name>::<MODULE>::init();
//         <crate_name>::<MODULE>::cleanup(ctx);
//     }
// }
//
// 最简验证（不需要调用任何函数）：
#[test]
fn link_succeeds() {
    // 如果程序能编译到这里，说明 libcov.a 链接成功
    let _ = 42_u64.wrapping_add(1);
}
```

将 `tests/ffi_smoke.rs` 注册到 `Cargo.toml`：

```toml
[[test]]
name = "ffi_smoke"
path = "tests/ffi_smoke.rs"
```

根据 C 项目实际暴露的函数，在测试中调用这些函数以触发 C 侧代码路径，从而获得有意义的覆盖率数据。

### 4b. 运行 cargo llvm-cov 输出 C 侧覆盖率

```bash
RUSTC_BOOTSTRAP=1 cargo llvm-cov \
  --manifest-path .c2rust/default/rust/Cargo.toml \
  --summary-only

# 生成详细的 lcov 报告（可导入 Codecov / Coveralls 等平台）：
RUSTC_BOOTSTRAP=1 cargo llvm-cov \
  --manifest-path .c2rust/default/rust/Cargo.toml \
  --lcov --output-path c-coverage.lcov
```

覆盖率报告会同时包含：
- **Rust 侧**：`lib.rs`、`mod_*.rs` 等 Rust 文件的行覆盖率
- **C 侧**：原始 C 源文件（如 `foo.c`、`bar.c`）的函数/行覆盖率

> **原理**：`build.rs` 令测试二进制链接了 `libcov.a`（用 clang 编译的插桩目标文件）。
> 当测试调用 C 函数时，LLVM profdata 记录了对应的 C 源码路径；`cargo llvm-cov`
> 合并这些数据后即可输出 C 文件的覆盖率。

---

## 快速参考：完整命令序列

```bash
# 0. 进入 C 项目目录
cd /path/to/my-c-project

# 1. 生成 Rust FFI + 覆盖率库
C2RUST_COV=1 c2rust-demo init -- clang -c foo.c bar.c -I.

# 2. 合并为按模块文件
c2rust-demo merge

# 3. 验证 Rust FFI 可编译
RUSTC_BOOTSTRAP=1 cargo check \
  --manifest-path .c2rust/default/rust/Cargo.toml

# 4a. （如需）迭代修复直到通过：
#   查看 cargo check 错误 → 修改 .c2rust/default/rust/src.2/ → 重复步骤 3

# 4b. 添加测试（调用 C 函数），然后输出 C 侧覆盖率
RUSTC_BOOTSTRAP=1 cargo llvm-cov \
  --manifest-path .c2rust/default/rust/Cargo.toml \
  --summary-only
```

---

## 常见问题

**Q: init 时提示 "no .c2rust files were generated"**
A: 检查构建命令是否实际编译了 `.c` 文件。确认 `C2RUST_PROJECT_ROOT` 是 C 源文件的祖先目录（`c2rust-demo` 会自动从当前目录向上搜索，一般无需手动设置）。

**Q: cargo check 报 `#![feature(linkage)] requires nightly`**
A: 设置 `RUSTC_BOOTSTRAP=1` 以允许在 stable 工具链上使用 nightly 特性。生成的 `lib.rs` 顶部已有注释说明这一点。

**Q: cargo llvm-cov 报告中看不到 C 文件**
A: 确认 `init` 时设置了 `C2RUST_COV=1`，且使用的是 clang（而非 gcc）进行编译。
检查 `.c2rust/default/meta/cov_lib.txt` 是否存在，以及 `.c2rust/default/rust/build.rs` 是否已生成。

**Q: 构建命令使用了 cmake / autoconf 等构建系统**
A: 通过 `CC=clang` 等环境变量将编译器替换为 clang，然后将完整的构建命令传给 `c2rust-demo init`：
```bash
C2RUST_COV=1 c2rust-demo init -- cmake --build build -j4
```
或使用 `C2RUST_CC=clang` 让 hook 仅将覆盖率编译步骤改用 clang，原始构建命令不变。
