#!/usr/bin/env bash
#
# 构建 macOS .app。两种模式：
#   1) 免费分享（默认）：ad-hoc 签名（免费，.app 双击启动必需），打成 zip，
#      朋友右键→打开即可，无需付费证书。
#   2) 正式分发：设置环境变量 SIGN="Developer ID Application: 你的名"
#      会用正式身份签名（用于公证 notarize）。
#
# 用法: scripts/build_mac_app.sh [版本号]
#   默认版本 0.1.0。产物在 target/release/Retouch-0.1.0-macOS.zip
#
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

APP_NAME="Retouch"
BIN="retouch-rs-gui"
ICON_SRC="ui/retouch-ui/assets/AppIcon.icns"
VERSION="${1:-0.1.8}"
SIGN="${SIGN:-}"            # 空 = 免费分享模式（不签名）
RELEASE_DIR="$ROOT/target/release"
OUT="$RELEASE_DIR/$APP_NAME.app"
ZIP="$RELEASE_DIR/$APP_NAME-$VERSION-macOS.zip"

echo "==> 编译 release ($BIN)"
cargo build --release -p retouch-ui

echo "==> 组装 $OUT"
rm -rf "$OUT"
mkdir -p "$OUT/Contents/MacOS" "$OUT/Contents/Resources"
cp "target/release/$BIN" "$OUT/Contents/MacOS/"
cp "$ICON_SRC" "$OUT/Contents/Resources/AppIcon.icns"

# 随包中文字体（OFL 许可，不编进二进制）：思源黑体变量字体 + 许可文件。
# 复制到 Contents/Resources/fonts，运行时由 setup_cjk_fonts 读取作为兜底。
mkdir -p "$OUT/Contents/Resources/fonts"
cp -R ui/retouch-ui/assets/fonts/. "$OUT/Contents/Resources/fonts/"

cat > "$OUT/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$APP_NAME</string>
  <key>CFBundleDisplayName</key><string>Retouch 修图</string>
  <key>CFBundleIdentifier</key><string>com.startap.retouch</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleExecutable</key><string>$BIN</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
PLIST

if [ -n "$SIGN" ]; then
    echo "==> 正式签名: $SIGN"
    codesign --force --deep --options runtime --sign "$SIGN" "$OUT"
else
    # 免费 ad-hoc 签名（必需要！macOS 对 .app 走 GUI 启动强制要求至少 ad-hoc 签名，
    # 否则 launchd 报 Code=163 Launch failed，双击打不开）。ad-hoc 不花一分钱。
    echo "==> 免费 ad-hoc 签名（.app 双击启动必需）"
    codesign --force --deep --sign - "$OUT"
fi

# 给朋友的傻瓜说明（醒目文件名，覆盖「完成/删除 → 设置里仍要打开」完整路径）
NOTE_FILE="首次打开必看-Retouch修图.txt"
cat > "$RELEASE_DIR/$NOTE_FILE" <<NOTE
Retouch 修图 · 首次打开必看（30 秒）
====================================

【先别慌】
你拿到的是免费分享版，没有花几百块做"付费开发者签名"，
所以苹果第一次会拦一下。这不是病毒、软件没坏、也不会自动被删，
只是第一次需要你手动"放行"一次，之后就永远正常了。

【解压后你会看到两个东西】
  Retouch.app                      ← 软件本体
  首次打开必看-Retouch修图.txt      ← 就是本文件
把 Retouch.app 拖到「应用程序」或桌面都可以。


【第一次怎么打开？下面选一种就行】

✦ 方法 A（最推荐，最稳）
  1) 在 Retouch.app 图标上【按右键】（或按住 Control 键点一下）
  2) 点菜单里的「打开」
  3) 弹出的窗口里再点一次【打开】
  —— 只需这样一次，以后直接双击就能开。

✦ 方法 B（如果弹窗只有「完成 / 删除」两个按钮）
  1) 点「完成」关掉弹窗（千万别点"删除"）
  2) 打开「系统设置」→「隐私与安全性」
  3) 往下拉，会看到一行「Retouch 已被拦截使用」
  4) 点它右边的【仍要打开】→ 再确认一次【仍要打开】
  —— 之后双击就能正常开了。

✦ 方法 C（万能，一行命令搞定）
  打开「终端」（启动台里搜"终端"），粘贴下面这行后回车：
  xattr -dr com.apple.quarantine "/Applications/Retouch.app"
  如果放在桌面，就把路径改成 "~/Desktop/Retouch.app"。


【常见问题】
Q：会不会有病毒？
A：不会。苹果只是不认识"无名开发者"，放行一次就好了。

Q：以后每次打开都要这样吗？
A：不用，只在第一次。之后双击直接进。

Q：支持什么电脑？
A：macOS 11 及以上，Apple 芯片（M1 / M2 / M3 / M4）都行。


用得不顺或想提建议，随时找分享给你的人就行。祝你修图愉快 🌿
NOTE

echo "==> 打包 $ZIP"
cd "$RELEASE_DIR"
rm -f "$ZIP"
# 用 Python 打包：macOS 自带 zip 对中文文件名会乱码，Python zipfile 走 UTF-8 正确写入
python3 - "$ZIP" "$APP_NAME.app" "$NOTE_FILE" <<'PY'
import zipfile, os, sys
out, items = sys.argv[1], sys.argv[2:]
with zipfile.ZipFile(out, 'w', zipfile.ZIP_DEFLATED) as zf:
    for it in items:
        if os.path.isdir(it):
            for root, _, files in os.walk(it):
                for f in files:
                    p = os.path.join(root, f)
                    zf.write(p, os.path.relpath(p, '.'))
        else:
            zf.write(it, it)
PY
cd "$ROOT"

echo "==> 完成: $ZIP"
if [ -z "$SIGN" ]; then
    echo "    免费分享版已就绪 → 发给朋友，按「分享说明.txt」右键打开即可。"
else
    echo "    已用 $SIGN 签名，可继续 notarize 公证后分发。"
fi
