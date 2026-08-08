#!/usr/bin/env bash
#
# 初色 · 带「皮肤精修」(TrueSkin) 的 macOS .app 打包脚本
# ============================================================
# 与 build_mac_app.sh 的区别：
#   1) 编译时带 `--features skin`（接入 trueskin-core ONNX + BiSeNet 语义门控）
#   2) 把 onnxruntime dylib 随包进 Contents/MacOS/
#   3) 把模型文件随包进 Contents/Resources/models/
#      (poc.onnx / poc.onnx.data / resnet18_parsing.onnx)
#
# 红线（全程严守）：模型(*.onnx)+dylib 不进 git（已在 .gitignore 覆盖）；
#   本脚本只把它们复制进 target/ 下的 .app（gitignored），不触碰仓库源码树。
#
# 用法: bash scripts/build_mac_app_skin.sh [版本号]
#   默认版本 = retouch-ui/Cargo.toml 里的 version。
#   产物: target/release/初色.app + target/release/初色-<版本>-macOS.zip
#
# 注意：用 `set -eo pipefail`（不用 -u），规避 WorkBuddy safe-bin shim 的 nounset 交互异常。
set -eo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

APP_DISPLAY="初色"
EXEC_NAME="retouch-rs-gui"
BINARY="target/release/$EXEC_NAME"
ICON_SRC="ui/retouch-ui/assets/AppIcon.icns"
FONT_DIR="ui/retouch-ui/assets/fonts"

# 版本：优先参数，否则读 retouch-ui/Cargo.toml
VERSION="${1:-$(grep '^version' ui/retouch-ui/Cargo.toml 2>/dev/null | head -1 | sed 's/.*"\(.*\)"/\1/')}"
[ -n "$VERSION" ] || VERSION="0.6.9"

RELEASE_DIR="$ROOT/target/release"
APP="$RELEASE_DIR/$APP_DISPLAY.app"
ZIP="$RELEASE_DIR/$APP_DISPLAY-$VERSION-macOS.zip"

echo "==> 1/5 编译 release ($EXEC_NAME, +skin)"
cargo build --release --features skin -p retouch-ui

echo "==> 2/5 定位 dylib 与模型"
# dylib 候选：trueskin-rs 的 target、本仓库 vendor、本仓库 target
DYLIB=""
for c in \
  "$ROOT/../trueskin-rs/target/release/libonnxruntime.1.28.0.dylib" \
  "$ROOT/vendor/onnxruntime/libonnxruntime.dylib" \
  "$ROOT/target/release/libonnxruntime.1.28.0.dylib" ; do
  if [ -f "$c" ]; then DYLIB="$c"; break; fi
done
if [ -z "$DYLIB" ]; then
  echo "❌ 未找到 libonnxruntime dylib。先跑: bash scripts/fetch_ort_dylib.sh"
  exit 1
fi
echo "    dylib: $DYLIB"

MODEL_DIR="$ROOT/models"
if [ ! -f "$MODEL_DIR/poc.onnx" ] || [ ! -f "$MODEL_DIR/resnet18_parsing.onnx" ]; then
  echo "❌ 未找到模型文件（models/poc.onnx + models/resnet18_parsing.onnx）。"
  exit 1
fi
echo "    模型: $MODEL_DIR"

echo "==> 3/5 组装 $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/models"

cp "$BINARY" "$APP/Contents/MacOS/$EXEC_NAME"
chmod +x "$APP/Contents/MacOS/$EXEC_NAME"
cp "$DYLIB" "$APP/Contents/MacOS/libonnxruntime.1.28.0.dylib"
cp "$MODEL_DIR/poc.onnx"        "$APP/Contents/Resources/models/"
cp "$MODEL_DIR/poc.onnx.data"    "$APP/Contents/Resources/models/"
cp "$MODEL_DIR/resnet18_parsing.onnx" "$APP/Contents/Resources/models/"

[ -f "$ICON_SRC" ] && cp "$ICON_SRC" "$APP/Contents/Resources/AppIcon.icns"
if [ -d "$FONT_DIR" ]; then
  mkdir -p "$APP/Contents/Resources/fonts"
  cp -R "$FONT_DIR/." "$APP/Contents/Resources/fonts/"
fi

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>$APP_DISPLAY</string>
  <key>CFBundleDisplayName</key><string>初色 修图</string>
  <key>CFBundleIdentifier</key><string>com.startap.retouch</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleExecutable</key><string>$EXEC_NAME</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
PLIST

echo "==> 4/5 免费 ad-hoc 签名（.app 双击启动必需；同时签署随包 dylib）"
codesign --force --deep --sign - "$APP"

NOTE_FILE="首次打开必看-初色.txt"
cat > "$RELEASE_DIR/$NOTE_FILE" <<NOTE
初色 修图 · 首次打开必看（30 秒，含「皮肤精修」模式）
================================================

【先别慌】
你拿到的是免费分享版，没有付费开发者签名，苹果第一次会拦一下。
这不是病毒、软件没坏，只是第一次需要你手动"放行"一次。

【第一次怎么打开？选一种】
✦ 方法 A（最推荐）：在 初色.app 图标上【按右键】→ 点「打开」→ 再点一次「打开」。
✦ 方法 B：弹窗只有关闭按钮时，去「系统设置 → 隐私与安全性」点「仍要打开」。
✦ 方法 C（万能）：终端粘贴  xattr -dr com.apple.quarantine "/Applications/初色.app"

【皮肤精修怎么用】
打开人像图 → 顶栏点「皮肤精修」→ 点「✨ 一键优化」（首次会加载模型，
约 1~3 秒）→ 拖三滑块（强度/色调/自然度）实时微调 → 用画笔局部加强/恢复。
效果实时生效、保存/导出即所见；按住 \ 键临时看原图。不想修了点「↩ 撤销精修」。

用得不顺或想提建议，随时找分享给你的人。祝你修图愉快 🌿
NOTE

echo "==> 5/5 打包 $ZIP"
cd "$RELEASE_DIR"
rm -f "$ZIP"
python3 - "$ZIP" "$APP_DISPLAY.app" "$NOTE_FILE" <<'PY'
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
echo "    .app: $APP ($(du -sh "$APP" | cut -f1))"
echo "    发给朋友后按「首次打开必看」右键打开即可。"
