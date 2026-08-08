#!/usr/bin/env bash
# 在 macOS 上交叉编译并打包 retouch-rs 的 Windows 版（0 依赖）。
#
# 产出两个 zip（均内嵌「源码」快照，用 git archive 自动排除 target/models/密钥/.workbuddy/dist）：
#   初色-<VER>-windows-分享版.zip   —— 无 Qwen Key，可自由分享
#   初色-<VER>-windows-自用版.zip   —— 含 Qwen Key（来自 ~/.retouch/qwen_key），仅自用
#
# 用法：bash scripts/build_win.sh [版本号，默认 0.6.9]
# 注意：用 `set -eo pipefail` 而非 `set -euo pipefail`。
# 在 WorkBuddy 的 safe-bin shim（export -f rm/unlink/rmdir + 注入 set -u）下，
# `set -u`(nounset) 会与导出函数产生已知交互异常，导致脚本内「已赋值变量」被
# 误判为 unbound 而崩溃。去掉 u 即规避，变量均已定义。
set -eo pipefail
cd "$(dirname "$0")/.."

VER="${1:-0.6.9}"
BIN="retouch-rs-gui"
TARGET="x86_64-pc-windows-msvc"
SRC="target/$TARGET/release/$BIN.exe"
OUT_DIR="target/$TARGET/release/pack"
SHARE_DIR="$OUT_DIR/初色"
SELF_DIR="$OUT_DIR/初色-自用版"

echo "==> 1/6 交叉编译 Windows release ($TARGET)"
# 交叉编译时让 winresource 用 brew 的 llvm-rc 生成 MSVC 兼容的资源(.res)
export RC="/opt/homebrew/opt/llvm/bin/llvm-rc"
# RUSTC_WRAPPER= SDKROOT= ：避免全局 sccache 把 macOS SDKROOT 误传给 clang-cl，
#                          导致含 C 代码的 crate(ring) 交叉编译失败（隐蔽坑，改 rustflags 触发全量重编才爆）
# -p retouch-ui ：带 default features（含 qwen）。ureq 已换 native-tls，不再依赖 ring，
#                故 Windows 交叉编译可正常编入 retouch-agent（AI 追色），与 Mac 功能一致。
RUSTC_WRAPPER= SDKROOT= cargo xwin build --release --target "$TARGET" -p retouch-ui

echo "==> 2/6 准备打包目录 + 源码快照（git archive，自动排除密钥/模型/构建产物）"
rm -rf "$OUT_DIR"
mkdir -p "$SHARE_DIR/fonts" "$SELF_DIR/fonts" "$SHARE_DIR/源码" "$SELF_DIR/源码"

# 可执行文件改名 初色.exe 更友好
cp "$SRC" "$SHARE_DIR/初色.exe"
cp "$SRC" "$SELF_DIR/初色.exe"
# 随包字体 + 开源许可
cp "ui/retouch-ui/assets/fonts/NotoSansSC-VF.ttf" "$SHARE_DIR/fonts/"
cp "ui/retouch-ui/assets/fonts/NotoSansSC-VF.ttf" "$SELF_DIR/fonts/"
cp "ui/retouch-ui/assets/fonts/OFL.txt" "$SHARE_DIR/"
cp "ui/retouch-ui/assets/fonts/OFL.txt" "$SELF_DIR/"

# 源码快照：git archive HEAD = 已提交源码，自动排除 target/ models/ .workbuddy/ dist/ qwen_key 等 gitignore 项
echo "    生成源码快照（git archive HEAD）..."
git archive HEAD | tar -x -C "$SHARE_DIR/源码"
git archive HEAD | tar -x -C "$SELF_DIR/源码"

# 自用版：塞入 Qwen Key（来自 ~/.retouch/qwen_key）
echo "    处理自用版 Qwen Key..."
if [ -f "$HOME/.retouch/qwen_key" ]; then
  cp "$HOME/.retouch/qwen_key" "$SELF_DIR/qwen_key"
  echo "    ✅ 已内置 Key（$(wc -c < "$HOME/.retouch/qwen_key") 字节）"
else
  echo "    ⚠️ 未找到 ~/.retouch/qwen_key，自用版将不含 Key（需手动放置）"
fi

echo "==> 3/6 写说明文件"
# 分享版首开必看
cat > "$SHARE_DIR/首次打开必看-初色.txt" <<'EOF'
初色 · 轻量修图工具（Windows 分享版）—— 首次打开必看
========================================================

【先别慌】
你拿到的是免费分享版，Windows 第一次可能会拦一下（SmartScreen 报"未知发布者"）。
这不是病毒、软件没坏，只是首次需要你点一下"仍要运行"，之后就正常了。

【怎么运行】
1. 把「初色」文件夹解压到任意位置（桌面、D 盘都行，路径可含中文）。
2. 双击里面的 初色.exe 即可，不用安装、不用装任何运行库。

【第一次可能被拦，怎么办？选一种】
✦ 方法 A（最常见）
  弹窗里点「更多信息」→ 再点「仍要运行」，就能打开。
  —— 只需这样一次，以后双击直接进。

✦ 方法 B（右键解除锁定）
  1) 在 初色.exe 上按【右键】→「属性」
  2) 底部若有「安全：此文件来自其他计算机…」，
     勾选「解除锁定」→ 确定。
  之后双击就再也不会拦了。

【注意事项】
• 界面用了随包的开源「思源黑体」（fonts/ 目录下），别删 fonts 文件夹，
  否则中文会变成方块（豆腐）。
• 整个文件夹要一起搬，不要只拷 初色.exe。
• 分享版不含 AI 追色 Key：AI 功能（自动命名/追色）需自己在软件里填 Key，
  或改用「自用版」。

【支持什么系统】
• Windows 10 / Windows 11 的 64 位系统（x86_64）均可。
• 不需要联网、不收集任何信息。

【命令行（高级）】
初色还有无界面 CLI，可在后台批量跑：
  初色.exe 不支持 CLI；CLI 是另一可执行文件 retouch-rs（见压缩包内 源码/cli 说明）。
  或直接用 Mac/Linux 上的 retouch-rs：
    retouch-rs render 原图.jpg 成品.jpg --exposure 0.3 --contrast 0.15
    retouch-rs analyze 原图.jpg --json      （把图量化成 AI 可读的 OKLCH 指标）
详见压缩包内 README / 源码中 retouch-cli 的说明。

【特色】
• 30MB，纯本地算法，照片不出电脑
• 套色/AI追色、感知色彩引擎、HSL分区调色、一键中性化
• 胶片预设、污点修复、人像美肤、批量导出
• 永久免费，无订阅

用得不顺或想提建议，随时找分享给你的人就行。祝你修图愉快 🌿
EOF

# 自用版首开必看（多一段 Key 放置说明）
cat > "$SELF_DIR/首次打开必看-初色.txt" <<'EOF'
初色 · 轻量修图工具（Windows 自用版）—— 首次打开必看
========================================================

【先别慌】
你拿到的是免费自用版，Windows 第一次可能会拦一下（SmartScreen 报"未知发布者"）。
这不是病毒、软件没坏，只是首次需要你点一下"仍要运行"，之后就正常了。

【怎么运行】
1. 把「初色-自用版」文件夹解压到任意位置（桌面、D 盘都行，路径可含中文）。
2. 双击里面的 初色.exe 即可，不用安装、不用装任何运行库。

【第一次可能被拦，怎么办？选一种】
✦ 方法 A（最常见）：弹窗里点「更多信息」→「仍要运行」。
✦ 方法 B：在 初色.exe 上【右键】→「属性」→ 勾选「解除锁定」→ 确定。

【AI 追色 / 自动命名 需要 Key】
本自用版已附带 qwen_key 文件，但软件默认从以下位置读取：
    C:\Users\你的用户名\.retouch\qwen_key
请这样放好（只需一次）：
  1) 把本文件夹里的 qwen_key 文件，复制到
     C:\Users\你的用户名\.retouch\qwen_key
     （.retouch 文件夹若不存在，新建一个即可；注意前面有个点）
  2) 或者在软件里「设置 → Qwen Key」直接粘贴 qwen_key 文件里的内容。
放好后，AI 追色、自动命名等功能即可联网使用。Key 仅存本地，不会上传。

【注意事项】
• 别删 fonts 文件夹（随包思源黑体），否则中文变方块。
• 整个文件夹一起搬，不要只拷 初色.exe。

【支持什么系统】
• Windows 10 / Windows 11 的 64 位系统（x86_64）。
• 默认不联网、不收集信息；仅 AI 功能才访问 Qwen 接口。

【特色】
• 纯本地算法修图 + 可选 AI 追色/命名
• 30MB，双击即用，免 VC++ 运行库
• 永久免费，无订阅

祝你修图愉快 🌿
EOF

echo "==> 4/6 打包 zip（Python zipfile，UTF-8 防中文乱码）"
cd "$OUT_DIR"
rm -f "../初色-$VER-windows-分享版.zip" "../初色-$VER-windows-自用版.zip"
python3 - "../初色-$VER-windows-分享版.zip" "$SHARE_DIR" <<'PY'
import zipfile, os, sys
out, root = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(out, 'w', zipfile.ZIP_DEFLATED) as zf:
    for cur, _, files in os.walk(root):
        for f in files:
            p = os.path.join(cur, f)
            zf.write(p, os.path.relpath(p, root))
print("  ✅", out)
PY
python3 - "../初色-$VER-windows-自用版.zip" "$SELF_DIR" <<'PY'
import zipfile, os, sys
out, root = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(out, 'w', zipfile.ZIP_DEFLATED) as zf:
    for cur, _, files in os.walk(root):
        for f in files:
            p = os.path.join(cur, f)
            zf.write(p, os.path.relpath(p, root))
print("  ✅", out)
PY

echo "==> 5/6 复制到 dist/（软件仓库目录）"
mkdir -p dist
cp "../初色-$VER-windows-分享版.zip" "dist/初色-$VER-windows-分享版.zip"
cp "../初色-$VER-windows-自用版.zip" "dist/初色-$VER-windows-自用版.zip"

echo "==> 6/6 完成"
echo "分享版：dist/初色-$VER-windows-分享版.zip"
echo "自用版：dist/初色-$VER-windows-自用版.zip"
ls -lh "dist/初色-$VER-windows-分享版.zip" "dist/初色-$VER-windows-自用版.zip"
EOF
