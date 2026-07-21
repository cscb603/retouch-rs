#!/usr/bin/env bash
# 在 macOS 上交叉编译并打包 retouch-rs 的 Windows 版（0 依赖）。
#
# 产物：target/x86_64-pc-windows-msvc/release/Retouch-<VER>-windows-x64.zip
#   内含 Retouch.exe（MSVC CRT 静态链接，不依赖 VC++ Redistributable）
#        + fonts/NotoSansSC-VF.ttf（随包思源黑体，OFL）
#        + OFL.txt + README.txt
#
# 用法：bash scripts/build_win.sh [版本号，默认 0.1.4]
set -euo pipefail
cd "$(dirname "$0")/.."

VER="${1:-0.1.8}"
BIN="retouch-rs-gui"
TARGET="x86_64-pc-windows-msvc"
SRC="target/$TARGET/release/$BIN.exe"
PKG_NAME="Retouch-$VER-windows-x64"
OUT_DIR="target/$TARGET/release/pack"

echo "==> 1/4 交叉编译 Windows release ($TARGET)"
# 交叉编译时让 winresource 用 brew 的 llvm-rc 生成 MSVC 兼容的资源(.res)
export RC="/opt/homebrew/opt/llvm/bin/llvm-rc"
cargo xwin build --release --target "$TARGET"

echo "==> 2/4 组装打包目录"
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR/$PKG_NAME/fonts"

# 可执行文件改名 Retouch.exe 更友好（字体解析按 current_exe().parent()/fonts 寻路）
cp "$SRC" "$OUT_DIR/$PKG_NAME/Retouch.exe"
# 随包字体 + 开源许可
cp "ui/retouch-ui/assets/fonts/NotoSansSC-VF.ttf" "$OUT_DIR/$PKG_NAME/fonts/"
cp "ui/retouch-ui/assets/fonts/OFL.txt" "$OUT_DIR/$PKG_NAME/"

# 中文说明（UTF-8，Windows 10+ 记事本可直接读；文件名与 Mac 版统一，醒目）
NOTE_FILE="首次打开必看-Retouch修图.txt"
cat > "$OUT_DIR/$PKG_NAME/$NOTE_FILE" <<'EOF'
Retouch 修图 · 首次打开必看（Windows 版，30 秒）
================================================

【先别慌】
你拿到的是免费分享版，没有花几百块做"付费代码签名"，
所以 Windows 第一次可能会拦一下（SmartScreen 报"未知发布者"）。
这不是病毒、软件没坏，只是首次需要你点一下"仍要运行"，之后就正常了。

【怎么运行】
1. 把 Retouch 文件夹解压到任意位置（桌面、D 盘都行，路径可含中文）。
2. 双击里面的 Retouch.exe 即可，不用安装、不用装任何运行库。

【第一次可能被拦，怎么办？选一种】
✦ 方法 A（最常见）
  弹窗里点「更多信息」→ 再点「仍要运行」，就能打开。
  —— 只需这样一次，以后双击直接进。

✦ 方法 B（右键解除锁定）
  1) 在 Retouch.exe 上按【右键】→「属性」
  2) 底部若有「安全：此文件来自其他计算机…」，
     勾选「解除锁定」→ 确定。
  之后双击就再也不会拦了。

【注意事项】
• 界面用了随包的开源「思源黑体」（fonts/ 目录下），别删 fonts 文件夹，
  否则中文会变成方块（豆腐）。
• 整个 Retouch 文件夹要一起搬，不要只拷 Retouch.exe。

【支持什么系统】
• Windows 10 / Windows 11 的 64 位系统（x86_64）均可。
• 不需要联网、不收集任何信息。

用得不顺或想提建议，随时找分享给你的人就行。祝你修图愉快 🌿
EOF

echo "==> 3/4 打包 zip"
cd "$OUT_DIR"
rm -f "../$PKG_NAME.zip"
# 用 Python 打包：macOS 自带 zip 对中文文件名会乱码，Python zipfile 走 UTF-8 正确写入
python3 - "../$PKG_NAME.zip" "$PKG_NAME" <<'PY'
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
echo "==> 4/4 完成：target/$TARGET/release/$PKG_NAME.zip"
