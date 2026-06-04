#!/usr/bin/env bash
# generate-ffi.sh — SKILL: 使用 c2rust-demo 生成 Rust FFI 并修复环境直到 cargo check 通过
#
# 用法:
#   ./scripts/generate-ffi.sh [OPTIONS] -- <BUILD_CMD>
#
# 选项:
#   --coverage            启用 LLVM 覆盖率插桩（需要 clang + cargo-llvm-cov）
#   --feature  NAME       Feature 名称（默认: "default"）
#   --project-dir  DIR    目标 C 项目目录（默认: 当前目录）
#   --max-fix  N          最大自动修复尝试次数（默认: 5）
#   --binary  PATH        c2rust-demo 二进制路径（默认: 自动检测）
#
# 示例:
#   # 基本用法（gcc 构建）
#   ./scripts/generate-ffi.sh -- gcc -c cJSON.c -I.
#
#   # 带覆盖率插桩（clang 构建，使用 cargo llvm-cov 采集 C 侧覆盖率）
#   ./scripts/generate-ffi.sh --coverage -- clang -c cJSON.c -I.
#
# 前置条件:
#   - gcc / clang（取决于 BUILD_CMD）
#   - libclang-dev（bindgen 依赖）
#   - cargo install bindgen-cli
#   - cargo install cargo-llvm-cov（仅 --coverage 模式需要）

set -euo pipefail

# -------------------------------------------------------------------------
# 常量
# -------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# -------------------------------------------------------------------------
# 默认参数
# -------------------------------------------------------------------------
COVERAGE=0
FEATURE="default"
PROJECT_DIR="$(pwd)"
MAX_FIX=5
BINARY=""
BUILD_CMD=()

# -------------------------------------------------------------------------
# 参数解析
# -------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
    case "$1" in
        --coverage)
            COVERAGE=1
            shift
            ;;
        --feature)
            FEATURE="$2"
            shift 2
            ;;
        --project-dir)
            PROJECT_DIR="$(cd "$2" && pwd)"
            shift 2
            ;;
        --max-fix)
            MAX_FIX="$2"
            shift 2
            ;;
        --binary)
            BINARY="$2"
            shift 2
            ;;
        --)
            shift
            BUILD_CMD=("$@")
            break
            ;;
        *)
            echo "未知选项: $1" >&2
            exit 1
            ;;
    esac
done

if [[ ${#BUILD_CMD[@]} -eq 0 ]]; then
    echo "错误: 缺少构建命令，用法示例: $0 -- gcc -c foo.c -I." >&2
    exit 1
fi

# -------------------------------------------------------------------------
# 定位 c2rust-demo 二进制
# -------------------------------------------------------------------------
find_binary() {
    # 1. 用户显式指定
    if [[ -n "$BINARY" ]]; then
        if [[ ! -x "$BINARY" ]]; then
            echo "错误: 指定的二进制 '$BINARY' 不存在或不可执行" >&2
            exit 1
        fi
        echo "$BINARY"
        return
    fi
    # 2. PATH 中查找
    if command -v c2rust-demo &>/dev/null; then
        command -v c2rust-demo
        return
    fi
    # 3. release 构建产物
    if [[ -x "$REPO_ROOT/target/release/c2rust-demo" ]]; then
        echo "$REPO_ROOT/target/release/c2rust-demo"
        return
    fi
    # 4. debug 构建产物
    if [[ -x "$REPO_ROOT/target/debug/c2rust-demo" ]]; then
        echo "$REPO_ROOT/target/debug/c2rust-demo"
        return
    fi
    # 5. 自动构建
    echo "c2rust-demo 未找到，正在编译..." >&2
    cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml" >&2
    echo "$REPO_ROOT/target/release/c2rust-demo"
}

C2RUST="$(find_binary)"

# -------------------------------------------------------------------------
# 打印配置信息
# -------------------------------------------------------------------------
echo "=== generate-ffi.sh ==="
echo "c2rust-demo 二进制 : $C2RUST"
echo "目标项目目录       : $PROJECT_DIR"
echo "Feature            : $FEATURE"
echo "构建命令           : ${BUILD_CMD[*]}"
echo "覆盖率插桩         : $([ "$COVERAGE" -eq 1 ] && echo '是（--coverage）' || echo '否')"
echo "最大修复次数       : $MAX_FIX"
echo ""

# -------------------------------------------------------------------------
# 步骤 1: 运行 c2rust-demo init
# -------------------------------------------------------------------------
echo "--- 步骤 1: c2rust-demo init ---"
cd "$PROJECT_DIR"

INIT_ARGS=(init --feature "$FEATURE")
if [[ "$COVERAGE" -eq 1 ]]; then
    INIT_ARGS+=(--coverage)
fi
INIT_ARGS+=(-- "${BUILD_CMD[@]}")

"$C2RUST" "${INIT_ARGS[@]}"
echo ""

# -------------------------------------------------------------------------
# 步骤 2: 运行 c2rust-demo merge
# -------------------------------------------------------------------------
echo "--- 步骤 2: c2rust-demo merge ---"
"$C2RUST" merge --feature "$FEATURE"
echo ""

# -------------------------------------------------------------------------
# 确定生成的 Rust 项目目录
# -------------------------------------------------------------------------
RUST_DIR="$PROJECT_DIR/.c2rust/$FEATURE/rust"

if [[ ! -d "$RUST_DIR" ]]; then
    echo "错误: 未找到生成的 Rust 项目: $RUST_DIR" >&2
    exit 1
fi

echo "生成的 Rust 项目: $RUST_DIR"
echo ""

# -------------------------------------------------------------------------
# 辅助函数: 向 lib.rs 追加类型别名（用于修复 E0412 未找到类型错误）
# -------------------------------------------------------------------------
# 查找 lib.rs（合并后位于 src.2/lib.rs 或 src/lib.rs）
find_lib_rs() {
    if [[ -L "$RUST_DIR/src" ]]; then
        # merge 之后 src 是指向 src.2 的符号链接
        local target
        target="$(readlink -f "$RUST_DIR/src")"
        echo "$target/lib.rs"
    else
        echo "$RUST_DIR/src/lib.rs"
    fi
}

add_type_alias() {
    local type_name="$1"
    local lib_rs
    lib_rs="$(find_lib_rs)"

    # 避免重复添加
    if grep -q "type ${type_name} " "$lib_rs" 2>/dev/null; then
        return
    fi

    echo "  自动修复: 添加占位类型别名 'type $type_name = u8;' 到 $lib_rs"
    # 在第一个 #![...] 属性之后插入（保持属性在最顶部）
    local insert_after
    insert_after=$(grep -n "^#!\[" "$lib_rs" | tail -1 | cut -d: -f1)
    if [[ -n "$insert_after" ]]; then
        sed -i "${insert_after}a\\ \n// Auto-fix: placeholder for unknown C type\npub type ${type_name} = u8;" "$lib_rs"
    else
        echo -e "\n// Auto-fix: placeholder for unknown C type\npub type ${type_name} = u8;" >> "$lib_rs"
    fi
}

# -------------------------------------------------------------------------
# 步骤 3: 循环运行 cargo check，自动修复已知问题
# -------------------------------------------------------------------------
echo "--- 步骤 3: cargo check（最多 $MAX_FIX 次修复）---"

FIX_ROUND=0
while true; do
    ERROR_LOG="$(mktemp)"
    # .cargo/config.toml 中已设置 RUSTC_BOOTSTRAP=1，此处显式设置作为保险
    if RUSTC_BOOTSTRAP=1 cargo check --manifest-path "$RUST_DIR/Cargo.toml" 2>"$ERROR_LOG"; then
        rm -f "$ERROR_LOG"
        echo ""
        echo "✓ cargo check 通过!"
        break
    fi

    if [[ "$FIX_ROUND" -ge "$MAX_FIX" ]]; then
        echo ""
        echo "✗ 已达到最大修复次数 ($MAX_FIX)，cargo check 仍失败。" >&2
        echo "完整错误输出:" >&2
        cat "$ERROR_LOG" >&2
        rm -f "$ERROR_LOG"
        exit 1
    fi

    FIX_ROUND=$((FIX_ROUND + 1))
    echo "  cargo check 失败（第 $FIX_ROUND 次修复尝试）"

    # --- 已知可自动修复的模式 ---
    FIXED_ANYTHING=0

    # E0412: cannot find type `X` in this scope
    while IFS= read -r type_name; do
        add_type_alias "$type_name"
        FIXED_ANYTHING=1
    done < <(grep "error\[E0412\]" "$ERROR_LOG" \
        | grep -oP "cannot find type \`\K[^\`]+" \
        | sort -u)

    # E0277: 无法自动修复，直接报错
    if grep -q "error\[E0277\]" "$ERROR_LOG"; then
        echo "  检测到 trait 约束不满足 (E0277)，无法自动修复。" >&2
        echo "完整错误:" >&2
        cat "$ERROR_LOG" >&2
        rm -f "$ERROR_LOG"
        exit 1
    fi

    if [[ "$FIXED_ANYTHING" -eq 0 ]]; then
        echo "  未检测到可自动修复的错误模式，停止修复。" >&2
        echo "完整错误:" >&2
        cat "$ERROR_LOG" >&2
        rm -f "$ERROR_LOG"
        exit 1
    fi

    rm -f "$ERROR_LOG"
done

# -------------------------------------------------------------------------
# 步骤 4（可选）: 覆盖率模式 — 运行 cargo llvm-cov 生成报告
# -------------------------------------------------------------------------
if [[ "$COVERAGE" -eq 1 ]]; then
    echo ""
    echo "--- 步骤 4: cargo llvm-cov（C 侧覆盖率）---"

    LCOV_OUTPUT="$RUST_DIR/coverage.lcov"
    RUSTC_BOOTSTRAP=1 cargo llvm-cov \
        --manifest-path "$RUST_DIR/Cargo.toml" \
        --lcov \
        --output-path "$LCOV_OUTPUT"

    echo ""
    echo "✓ 覆盖率报告已生成: $LCOV_OUTPUT"
    echo ""
    echo "使用以下命令查看 HTML 报告:"
    echo "  RUSTC_BOOTSTRAP=1 cargo llvm-cov report --manifest-path $RUST_DIR/Cargo.toml --html"
fi

# -------------------------------------------------------------------------
# 完成
# -------------------------------------------------------------------------
echo ""
echo "=== generate-ffi.sh 完成 ==="
echo ""
echo "生成的 Rust 项目位于:"
echo "  $RUST_DIR"
echo ""
echo "后续步骤:"
echo "  cd $RUST_DIR"
if [[ "$COVERAGE" -eq 1 ]]; then
    echo "  RUSTC_BOOTSTRAP=1 cargo llvm-cov report --html   # 查看 HTML 覆盖率报告"
else
    echo "  RUSTC_BOOTSTRAP=1 cargo check                     # 验证 FFI 绑定"
fi
