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

# 4. 生成 AppIcon.icns：用 sips 把 assets/icon.png 切成 10 个标准尺寸，
#    再用 iconutil 打成 .icns。两者都是 macOS 自带，CI 与本地都有。
#    失败仅 warn——bundle 仍可用，只是 Finder/Dock 显示通用图标。
if command -v sips >/dev/null 2>&1 && command -v iconutil >/dev/null 2>&1; then
    iconset_dir="$(mktemp -d -t qrctrl-iconset)/AppIcon.iconset"
    mkdir -p "$iconset_dir"
    src="$root/assets/icon.png"
    # 规格：<像素尺寸>:<文件名>
    specs=(
        "16:icon_16x16.png"
        "32:icon_16x16@2x.png"
        "32:icon_32x32.png"
        "64:icon_32x32@2x.png"
        "128:icon_128x128.png"
        "256:icon_128x128@2x.png"
        "256:icon_256x256.png"
        "512:icon_256x256@2x.png"
        "512:icon_512x512.png"
        "1024:icon_512x512@2x.png"
    )
    for spec in "${specs[@]}"; do
        size="${spec%%:*}"
        name="${spec#*:}"
        sips -s format png -z "$size" "$size" "$src" --out "$iconset_dir/$name" >/dev/null 2>&1 || \
            echo "[warn] sips 生成 $name 失败" >&2
    done
    if iconutil -c icns "$iconset_dir" -o "$app/Contents/Resources/AppIcon.icns" >/dev/null 2>&1; then
        echo "[build] AppIcon.icns 已生成"
    else
        echo "[warn] iconutil 失败，bundle 将无自定义图标" >&2
    fi
    rm -rf "$(dirname "$iconset_dir")"
else
    echo "[warn] 找不到 sips 或 iconutil（非 macOS？），bundle 将无自定义图标" >&2
fi

# 5. ad-hoc 签名：让 macOS 至少认作合法 bundle；release CI 上未签名也能跑
codesign --force --deep --sign - "$app" >/dev/null 2>&1 || \
    echo "[warn] codesign 失败（可忽略），bundle 仍可用，但 macOS 会标记为未签名" >&2

echo "[build] 完成：$app"
echo "        双击启动，或拖到 /Applications 后用 Spotlight 启动"
