#!/usr/bin/env bash
# 把 release 编译产物打包成 .app bundle，让 macOS 双击启动时不弹 Terminal，
# 关任何窗口都不会杀进程。
#
# 核心机制：Info.plist 里设了 LSUIElement=true，LaunchServices 把它当成
# 背景 UI 应用（agent）——不进 Dock、不依附 Terminal，但托盘图标与按需弹出的
# QR 窗口仍正常工作（与 Windows 的 windows_subsystem = "windows" 等效）。
#
# 用法：
#   cargo build --release
#   ./scripts/build-macos-app.sh
#
# 跨 target 编译时指定 triple：
#   cargo build --release --target aarch64-apple-darwin
#   TARGET_TRIPLE=aarch64-apple-darwin ./scripts/build-macos-app.sh
#
# 也可显式传入二进制路径：
#   ./scripts/build-macos-app.sh path/to/qrctrl
#
# 产物：与传入二进制同目录下的 qrctrl.app

set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"

# 1. 定位二进制
if [[ $# -ge 1 ]]; then
    bin="$1"
elif [[ -n "${TARGET_TRIPLE:-}" ]]; then
    bin="$root/target/${TARGET_TRIPLE}/release/qrctrl"
else
    bin="$root/target/release/qrctrl"
fi

if [[ ! -f "$bin" ]]; then
    echo "[error] 找不到二进制：$bin" >&2
    echo "        请先 cargo build --release" >&2
    exit 1
fi

bin_dir="$(cd "$(dirname "$bin")" && pwd)"
app="$bin_dir/qrctrl.app"

# 2. 从 Cargo.toml 解析 version（写到 Info.plist 的 CFBundleVersion / CFBundleShortVersionString）
version=$(grep -m1 '^version' "$root/Cargo.toml" | sed -E 's/^version[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/')
if [[ -z "$version" ]]; then
    echo "[error] 无法从 Cargo.toml 解析 version" >&2
    exit 1
fi

echo "[build] 组装 $app (version $version)"

# 3. 组装 bundle 结构
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

cp "$bin" "$app/Contents/MacOS/qrctrl"
chmod +x "$app/Contents/MacOS/qrctrl"

# 注入版本号
sed "s/@VERSION@/$version/g" "$root/assets/macos/Info.plist" > "$app/Contents/Info.plist"

# 4. ad-hoc 签名：让 macOS 至少认作合法 bundle；release CI 上未签名也能跑
codesign --force --deep --sign - "$app" >/dev/null 2>&1 || \
    echo "[warn] codesign 失败（可忽略），bundle 仍可用，但 macOS 会标记为未签名" >&2

echo "[build] 完成：$app"
echo "        双击启动，或拖到 /Applications 后用 Spotlight 启动"
