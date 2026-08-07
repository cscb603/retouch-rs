## 初色 Retouch v0.6.6

**一句话**：一款**完全本地运行、不联网、免费**的修图工具——帮你一键智能调色、无痕修复照片污点瑕疵，Mac 和 Windows 都能用，照片不出本机。v0.6.6 重点是**三件事：自动找污点、导出更高清、整体更跟手**。

### v0.6.6 新增

#### 1. 自动污点检测（一键找出灰尘 / 瑕疵）
- 新增「**自动检测**」按钮：点一下，算法自动扫一遍画面，把灰尘、sensor 脏点、明显瑕疵**标出来并直接加入修复队列**，不用再拿画笔一个个圈。
- **纯算法、零权重、可离线**：不引入任何 ML / ONNX 模型，不联网、不下载模型，照片不出本机，老电脑也能跑。
- **不乱点**（关键）：用「孤立性校验」区分真正的污点和树叶 / 织物纹理——只有周围干净、自身突出的才判定为污点，**误检比漏检危险得多**，所以宁可少检也不瞎修。
- 实测：合成测试图 **18/18 召回、0 误检**，预览尺寸约 47ms；全尺寸档（1–6px 半径）6/6 检出且零误检（带回归护栏测试）。
- 自动检测后仍可手动增删，两不误。

#### 2. 导出画质优化（彩色边缘不再发虚）
- 导出改用 **MozJPEG 4:4:4 全色度采样 + quality 95 + 感知优化量化表**，彻底消除旧版（4:2:0 子采样）在彩色边缘、细线、红字上**发虚、镶边**的问题。
- 实测对比（PSNR，越高越接近原图）：
  - 合成彩色边缘图：**+27.4 dB**（画质提升巨大，旧版彩色边明显糊）
  - 实拍人像 / 风景：**约 +0.6 dB**，代价是文件体积约 **+45%**——取舍明确：要最高画质就吃这点体积。
- 重采样换 **fast_image_resize（SIMD）**，缩放更快。
- 导出**保留 EXIF + XMP 元数据**，且只标 sRGB、不复制源图的宽色域 ICC，避免色彩管理翻车。

#### 3. 复用性能（整体更跟手）
- 去冗余内存分配、修复只算 mask 包围盒，**大图处理更省**。
- 自动检测里的中值滤波改用 **Huang 直方图滑窗算法 + 并行**，比朴素排序 **8.4× 提速**（同样耗时几乎与核大小无关）。
- 全程新代码 **clippy 零警告**，干净收尾。

### 对你有什么用？

- **省心**：所有运算都在你电脑上跑，照片不上传任何服务器，隐私安全；软件免费，无订阅、无广告。
- **省事**：自动检测一键标污点，不用手动画；智能一键调色 + 污点修复（像专业软件的"修复画笔"，从附近自然取纹理补上）；拖进来就能修。
- **省时**：导入 / 切图 / 导出全部后台处理，界面不卡顿；导出画质升级后，发朋友圈 / 打印都更经得起放大看。

### 30 秒上手

1. 下载下面的对应压缩包，解压。
2. **第一次打开请先看压缩包里的 `首次打开必看-初色.txt` / `首次打开必看-Retouch修图.txt`**（Mac 会提示"无法验证"、Win 会被 SmartScreen 拦，都是正常误报，照说明点一下即可）。
3. 把照片拖进窗口 → 点「自动检测」标污点（或左侧手动调色 / 选"污点"工具涂抹）→ 点导出。

### 主要特性

- 智能一键调色（本地规则 + 可选 AI 联网，API key 仅存内存）
- **新增**：自动污点检测（纯算法、零权重、防误检）
- 污点修复四档：传统 / 自然 / 精修（Poisson）/ 内容感知（PatchMatch）
- **导出升级**：MozJPEG 4:4:4 最高画质 + 保留 EXIF/XMP
- 导入、切图、批量导出全异步，状态栏有进度和转圈提示
- 响应式布局：窗口拉窄自动折叠菜单、工具栏自动换行
- 纯本地、零依赖（Windows 无需安装 VC++；macOS 双击即用）

### 下载

- **macOS（自用版，界面中文名「初色」）**：`初色-0.6.6-macOS.zip`
- **macOS（分享版，界面英文名 Retouch，给朋友用）**：`Retouch-0.6.6-macOS.zip`
- **Windows 10 / 11（64 位）**：`初色-0.6.6-windows-x64.zip`

> 自用版与分享版功能完全一致，只是显示名不同（中文「初色」/ 英文「Retouch」），按喜好选一个即可。

---

#### English

**Retouch v0.6.6** — a fully local, offline, free photo retouching tool. One-click intelligent color grading and seamless spot/healing repair, for macOS and Windows. v0.6.6 focuses on three things: **auto spot detection, higher-quality export, and snappier performance**.

**What's new in v0.6.6**

1. **Auto spot detection** — one click scans the frame and adds dust / sensor spots / obvious blemishes straight to the healing queue. Pure algorithm, **zero model weights, fully offline** (no ML/ONNX, no download). An **isolation check** prevents false positives on leaf/fabric textures (missing a spot is far safer than destroying real detail). Verified: **18/18 recall, 0 false positives**, ~47ms at preview size.
2. **Higher-quality export** — switched to **MozJPEG 4:4:4 + quality 95 + perceptual quantization**, killing the color-fringe blur of the old 4:2:0 path. Measured: **+27.4 dB** on synthetic color edges, **~+0.6 dB** on real photos (at ~+45% file size — an explicit quality/size trade-off). Resampling now uses fast_image_resize (SIMD). **EXIF + XMP preserved**; only sRGB tagged, no wide-gamut ICC copied.
3. **Reuse performance** — removed redundant allocations, heal restricted to the mask bounding box; the median filter now uses a **Huang histogram sliding-window + parallelism**, **8.4× faster** than naive sort. New code is **clippy-warning-free**.

**Highlights**
- Smart one-click grading (local heuristics, optional AI via your own API key kept in memory only)
- New: auto spot detection (offline, zero-weight, false-positive guarded)
- Spot healing in 4 modes: Traditional / Natural / Pro (Poisson) / Content-Aware (PatchMatch)
- Export upgrade: MozJPEG 4:4:4 max quality + EXIF/XMP kept
- Fully async import / switch / batch-export with progress UI
- Responsive layout: auto-collapsing panels and wrapping toolbar
- Zero-dependency static builds (no VC++ runtime on Windows; double-click on macOS)

**Build**: Rust + egui. `cargo build --release` (cross-compile Windows via `cargo xwin`).

**Download**
- macOS (self-use, Chinese name 初色): `初色-0.6.6-macOS.zip`
- macOS (share build, English name Retouch): `Retouch-0.6.6-macOS.zip`
- Windows 10/11 (64-bit): `初色-0.6.6-windows-x64.zip`

---

### 许可证 / License

本项目以 MIT 许可证发布，完全免费、可自由使用与修改。版权所有 © 2026 星TAP。

Licensed under the MIT License — free to use and modify. Copyright © 2026 星TAP.

完整条款见仓库根目录 [`LICENSE`](https://github.com/cscb603/retouch-rs/blob/main/LICENSE)。
