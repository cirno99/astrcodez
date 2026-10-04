#!/bin/bash
# 产出 astrcode-server 的内嵌资源，再构建服务端二进制。
#
# crates/astrcode-webui/www/wasm/ 在 astrcode-server 编译期被读取，必须先于最后的
# cargo build 生成；缺了它服务端启动时会告警，浏览器里也加载不到 UI。
#
# 用法：scripts/build.sh [--debug]
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

RELEASE_FLAG="--release"
BUILD_MODE="release"
if [[ "$1" == "--debug" ]]; then
  RELEASE_FLAG=""
  BUILD_MODE="debug"
fi


cd "$WORKSPACE_ROOT"

# wasm-ld 默认 1 MiB 栈对 gpui 的渲染树不够，链接期抬高。
cargo rustc -p astrcode-webui --lib --target wasm32-unknown-unknown $RELEASE_FLAG -- \
  -C link-arg=-zstack-size=8388608

WASM_PATH="$WORKSPACE_ROOT/target/wasm32-unknown-unknown/$BUILD_MODE/astrcode_webui.wasm"
if [[ ! -f "$WASM_PATH" ]]; then
  echo "未找到 wasm 产物：$WASM_PATH" >&2
  exit 1
fi

# 产物落点就是服务端内嵌的那一份，`www/` 整体即 Web UI 的静态站点根目录。
mkdir -p "$WORKSPACE_ROOT/crates/astrcode-webui/www/wasm"
wasm-bindgen "$WASM_PATH" \
  --out-dir "$WORKSPACE_ROOT/crates/astrcode-webui/www/wasm" \
  --target web \
  --no-typescript

cargo build $RELEASE_FLAG -p astrcode-server --bin astrcode-http-server
