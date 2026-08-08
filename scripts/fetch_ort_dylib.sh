#!/usr/bin/env bash
# 获取 onnxruntime 1.28.0 共享库到 vendor/onnxruntime/，作为「随包 dll」的离线来源。
#
# 背景：ort 2.0.0-rc.13 绑定 onnxruntime 1.28.0。其自带的 copy-dylibs 特性会在构建期
# 从 cdn.pyke.io 下载预编译二进制并复制到可执行文件同目录（"随包"）。但 CI/沙箱构建脚本
# 常无外网，下载会静默失败。本脚本提供确定性兜底：从 PyPI 拉取同版本 onnxruntime wheel
# （MS 官方构建，MIT，ABI 与 ort-sys 绑定版一致），解出 lib 放到 vendor/，构建即可离线随包。
#
# 用法：bash scripts/fetch_ort_dylib.sh
# 之后：cargo build/test --features onnx 时，把 vendor/onnxruntime/ 下的 lib 复制到
#       可执行文件同目录（或设 ORT_DYLIB_PATH 指向它）即可被 load-dynamic 优先加载。
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="$ROOT/vendor/onnxruntime"
mkdir -p "$DEST"
PYBIN="${PYTHON:-python3}"
ORT_VER="1.28.0"

echo "[fetch] 下载 onnxruntime==${ORT_VER} wheel (PyPI) ..."
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
"$PYBIN" -m pip download "onnxruntime==${ORT_VER}" --no-deps --only-binary=:all: -d "$TMP" >/dev/null
WHL="$(find "$TMP" -name 'onnxruntime-*.whl' | head -1)"
[ -n "$WHL" ] || { echo "[fetch] 未找到 onnxruntime wheel" >&2; exit 1; }
echo "[fetch] 解包 $WHL"

"$PYBIN" - "$WHL" "$DEST" <<'PY'
import sys, zipfile, os, shutil
whl, dest = sys.argv[1], sys.argv[2]
found = False
with zipfile.ZipFile(whl) as z:
    for n in z.namelist():
        if n.startswith("onnxruntime/capi/libonnxruntime") and \
           (n.endswith(".dylib") or n.endswith(".so") or n.endswith(".dll")):
            base = os.path.basename(n)
            # 规范化为 load-dynamic 期望的文件名（去掉版本号后缀）
            if base.endswith(".dylib") and ".dylib." not in base:
                tgt_name = "libonnxruntime.dylib"
            elif base.endswith(".so"):
                tgt_name = "libonnxruntime.so"
            else:
                tgt_name = "onnxruntime.dll"
            tgt = os.path.join(dest, tgt_name)
            with z.open(n) as src, open(tgt, "wb") as out:
                shutil.copyfileobj(src, out)
            print("  ->", tgt)
            found = True
if not found:
    print("  [warn] wheel 内未找到 libonnxruntime* 共享库", file=sys.stderr)
PY

echo "[fetch] 完成："
ls -la "$DEST"
