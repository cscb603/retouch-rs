#!/bin/bash
# 初色 Retouch macOS .app 打包脚本
# 用法：cd retouch-rs && bash scripts/package_mac_app.sh
#
# 注意：二进制名用 ASCII（"retouch-rs"），中文路径会崩。
# Finder 显示名仍为「初色」（CFBundleName）。

set -euo pipefail

APP_NAME="初色"
EXEC_NAME="retouch-rs"
BINARY="target/release/retouch-rs-gui"
FONT="ui/retouch-ui/assets/fonts/NotoSansSC-VF.ttf"
ICON="ui/retouch-ui/assets/AppIcon.icns"
OUTPUT_DIR="dist"

VERSION=$(grep '^version' Cargo.toml 2>/dev/null | head -1 | sed 's/.*"\(.*\)"/\1/' || echo "0.7.0")

echo "📦 打包 $APP_NAME v$VERSION"

# 检查必要文件
[ -f "$BINARY" ] || { echo "❌ 未找到 $BINARY，先 cargo build --release"; exit 1; }

mkdir -p "$OUTPUT_DIR"

# 创建 .app 目录结构
APP_BUNDLE="$OUTPUT_DIR/$APP_NAME.app"
rm -rf "$APP_BUNDLE"
mkdir -p "$APP_BUNDLE/Contents/MacOS"
mkdir -p "$APP_BUNDLE/Contents/Resources"

# 拷贝二进制（用 ASCII 名，中文路径会 SIGSEGV）
cp "$BINARY" "$APP_BUNDLE/Contents/MacOS/$EXEC_NAME"
chmod +x "$APP_BUNDLE/Contents/MacOS/$EXEC_NAME"

# 拷贝字体（可选）
if [ -f "$FONT" ]; then
    mkdir -p "$APP_BUNDLE/Contents/Resources/fonts"
    cp "$FONT" "$APP_BUNDLE/Contents/Resources/fonts/"
fi

# 拷贝图标（可选）
if [ -f "$ICON" ]; then
    cp "$ICON" "$APP_BUNDLE/Contents/Resources/AppIcon.icns"
fi

# Info.plist：显示名=初色，可执行文件=ASCII
cat > "$APP_BUNDLE/Contents/Info.plist" << EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>$EXEC_NAME</string>
    <key>CFBundleIdentifier</key>
    <string>com.startap.retouch-rs</string>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundleVersion</key>
    <string>$VERSION</string>
    <key>CFBundleShortVersionString</key>
    <string>$VERSION</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
EOF
if [ -f "$ICON" ]; then
    cat >> "$APP_BUNDLE/Contents/Info.plist" << EOF
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
EOF
fi
cat >> "$APP_BUNDLE/Contents/Info.plist" << EOF
</dict>
</plist>
EOF

# ad-hoc 签名
echo "✍️  ad-hoc 签名..."
codesign --force --sign - "$APP_BUNDLE" 2>/dev/null || true

echo "✅ 打包完成: $APP_BUNDLE ($(du -sh "$APP_BUNDLE" | cut -f1))"
echo "📁 路径: $(cd "$APP_BUNDLE" && pwd)"
echo ""
echo "首次运行：右键 → 打开 → 选择「打开」（仅第一次需要）"
