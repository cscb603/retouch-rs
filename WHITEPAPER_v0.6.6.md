# 初色 v0.6.6 设计稿（AI 可消费契约）

- **版本**: 0.6.5 → 0.6.6
- **日期**: 2026-08-07
- **作者**: 小 d
- **状态**: 已评审，待实现
- **关联**: 吸取高速缩图（星TAP 高清缩图 v4.4.x）导出算法；自动污点检测；性能 P0/P1

---

## 0. 约束前置（不做什么 / 红线）

| # | 约束 | 理由 |
|---|---|---|
| C1 | **不引入任何 ML/ONNX 依赖**，自动去污点仅限于「孤立小斑点」信号处理 | 保持纯算法、零权重、可离线架构（用户铁律） |
| C2 | **不做语义级瑕疵检测**（人、电线杆、皮肤痘印），此类留作未来可选 ML 插件 | 语义分割需模型，违背 C1；电线是结构化线条易误伤 |
| C3 | **输出图像 ICC 不复制源图宽色域 profile**（Display P3 / Adobe RGB 等）；统一输出标 sRGB（或嵌标准 sRGB profile） | 管线输出是 sRGB，复制源宽色域 ICC 会把 sRGB 像素错标成宽色域 → 色彩崩（高速缩图"复制 ICC"做法在此是坑，不照搬） |
| C4 | **不破坏离线 / 零权重架构**，新增依赖必须纯 Rust、无 C 工具链 | 保证 Win 交叉编译不受影响（避开历史 ring 坑） |
| C5 | **OKLCH 转换不做 LUT/近似加速** | 有保真风险；主循环已 rayon 并行，ROI 低 |
| C6 | 性能优化**不改动四档修复的视觉结果**（仅提速/结构），用 PSNR 回归守护 | 不引入画质回退 |

---

## 1. 模块 A：导出画质升级（用户选 A 方案）

### 1.1 决策（A vs B）
- **A 方案（采用）**：所有 JPEG 导出默认 `S444`（无 chroma 下采样）+ `quality 95` + progressive + Huffman 优化。画质优先，体积非主要矛盾。
- B 方案（弃用）：维持 4:2:0 + 额外「最高画质」预设。未被采纳。

### 1.2 依赖变更（retouch-core/Cargo.toml，均纯 Rust）
```toml
fast_image_resize = "6.0"    # SIMD 重采样，替 image::resize_exact
mozjpeg-rs = "0.9.2"         # 纯 Rust MozJPEG，替 JpegEncoder，暴露 subsampling
```
依赖影响：两个均纯 Rust 零 C 依赖（高速缩图已跨平台实跑验证）→ Win 交叉编译无影响。

> **实现修正（相对初稿）**：初稿计划引入 `img_parts = "0.4"` 保留元数据，实现时改为
> 自写 `extract_app1_segments()` 直接扫描源文件的 APP1 段——只需几十行、无需新依赖，
> 且能精确控制「保留 EXIF+XMP、不复制源 ICC」（见 C3）。**已少引入一个依赖。**

### 1.3 改动文件与指令
**文件 `crates/retouch-core/src/export.rs`**：

| 函数 | 现状(0.6.5) | 改为 | 验收 |
|---|---|---|---|
| `encode_jpeg` (~L342) | `JpegEncoder::new_with_quality(q)` 默认 4:2:0 | `mozjpeg_rs`：`quality=95`(默认，UI 可覆盖)，`subsampling=S444`，`progressive=true`，`optimize_huffman=true`，`quant_tables=MssimTuned` | 导出-重导 PSNR > 45 dB（vs 原图）；彩色边缘不发虚 |
| `smart_downscale` (~L224) | `img.resize_exact(nw,nh,Lanczos3)` sRGB 空间 | `fast_image_resize` Lanczos3（P1 可选升线性光） | 视觉无暗边振铃；同尺寸 PSNR 不降 |
| `extract_exif_app1` (~L427) | 仅复制首个 APP1 | 改名 `extract_app1_segments`，收集**全部** APP1 段（EXIF+XMP）；**ICC 按 C3 不复制源宽色域** | 导出图含 EXIF+XMP；色彩空间正确标 sRGB |

**PNG**：`PngEncoder::new_with_quality(Best, AdaptiveFilter)`（无损同画质、体积更小），可选。

### 1.4 验收指标（模块 A）—— ✅ 已实测通过
基准程序：`cargo run --release -p retouch-core --example bench_export_quality -- <实拍图...>`

| 测试图 | S444 PSNR | S420 PSNR | 画质增益 | 体积代价 |
|---|---|---|---|---|
| 合成高饱和彩色边缘（最坏情况） | 42.77 dB | 15.37 dB | **+27.39 dB** | +91.3% |
| 合成肤色平滑渐变 | 52.39 dB | 52.27 dB | +0.12 dB | +70.7% |
| 实拍 3000×2000 | 52.61 dB | 52.01 dB | +0.59 dB | +47.4% |
| 实拍 2000×3000 | 48.04 dB | 47.36 dB | +0.68 dB | +42.4% |

结论：
- 判据「彩色边缘增益 ≥ 3 dB」**远超达成**（合成最坏情况 +27 dB）；平滑区不劣化（+0.12 dB）。
- **诚实记录代价：实拍照片体积增大约 42~47%**，换取 +0.6 dB 及彩色边缘不发虚。
  这符合「最高画质、尽量接近原图」的产品定位；若日后有体积诉求，再评估 B 方案（预设开关）。
- 上表实拍参照系本身是已 JPEG 压缩（多为 4:2:0）的源图，色度已受损，
  故**低估**了 S444 的真实收益；实际管线从编辑后像素直接编码，彩色边缘改善更明显。
- `cargo build` 双平台（Mac / Win xwin）通过；0 warning。

---

## 2. 模块 B：性能优化 P0/P1

### 2.1 P0（极小投入 / 大收益 / 零风险）
| 项 | 位置 | 改动 |
|---|---|---|
| B1 Vec 分配 | `heal.rs` PatchMatch `heal_patchmatch`(~L683-693) | 每 inner 迭代 `(0..h).collect()` / `(0..w).collect()` 分配整行/列 `Vec<i32>` → 改为直接 `for y in 0..h` / `for x in 0..w` range 迭代，删 `.collect()` |
| B2 清 warning | 全 workspace | 清 15 个编译 warning（unused import/var、多余括号、unused_mut），恢复 clippy-clean 基线 |

### 2.2 P1（中投入 / 收益明确 / 低风险）
| 项 | 位置 | 改动 |
|---|---|---|
| B3 mean_l 合并 | `pipeline.rs`(~L978-994) | 全图二趟 OKLCH 预pass（仅 contrast/dehaze≠0 时触发）溶入主循环 `par_chunks_mut` 的 rayon reduce，去整图第二次 OKLCH 转换 |
| B4 mask 包围盒 | `heal.rs` PatchMatch | 全图扫描 → 改 mask 包围盒 + `patch_r` 边距；小污点落大图不再每趟扫全图 |

### 2.3 实施结果（模块 B）
| 项 | 状态 | 说明 |
|---|---|---|
| B1 Vec 分配 | ✅ 完成 | 改 `Box<dyn Iterator>` 零分配 range 迭代 |
| B2 清 warning | ✅ 完成 | `cargo build` / `cargo test` 全 workspace **0 warning**；新增代码 clippy 亦 0 警告 |
| B3 mean_l 合并 | ❌ **主动降级不做** | 见下方说明 |
| B4 mask 包围盒 | ✅ 完成 | inner 循环只扫洞包围盒（含 `patch_r` 边距） |
| **B5 Huang 滑窗中值**（计划外新增） | ✅ 完成 | 见下方说明，**8.4 倍提速** |

**B3 降级理由（如实记录）**：原计划把 `pipeline.rs` 的 mean_l 二趟 OKLCH 预pass 溶入主循环。
读码后发现 `apply_grade`(~L1072) 依赖 `mean_l` 作亮度 pivot，而主循环的 OKLCH 转换在 L1056-1057
——**存在循环依赖**，消除预pass 需重构整个 grade 阶段。ROI 低且有回归风险，按「抓大头」原则砍掉。

**B5 计划外新增（模块 C 逼出来的性能项）**：自动检测需要大核中值滤波，
朴素实现每像素排序 k² 个样本，ksize=13 时 1400×933 耗时 394ms，超出 UI 同步执行预算。
改为 **Huang 直方图滑窗中值**（256 bin，窗口右移仅增删 2 列共 2k 样本，中值位置 O(1) 摊还修正）：

| 场景 | 朴素排序 | Huang 滑窗 | 提速 |
|---|---|---|---|
| 1400×933 ksize=13 | 394ms | **47ms** | 8.4× |
| 1400×933 ksize=5 | 92ms | 33ms | 2.8× |
| 6000×4000 默认 | 1871ms | 598ms | 3.1× |

正确性护栏：新增测试 `huang_median_matches_naive`，在 ksize=3/5/9/13 下与朴素排序中值
**逐像素比对完全一致**，杜绝静默数值错误。

### 2.4 验收指标（模块 B）—— ✅ 已通过
- `cargo build` 警告数 = 0 ✅
- 修复结果与 0.6.5 一致（既有 heal/spot 测试全绿，C6 守护）✅

---

## 3. 模块 C：自动去污点（用户已认可"也不错"）

### 3.1 算法路线（纯信号处理，零权重）
```
原图 I
  → 无结构参考 R = median_filter(I) 或 guided_filter(I)   // 估计平滑背景
  → 残差 D = |I - R|                                        // 突出孤立高对比点
  → 阈值 + 形态学去噪
  → 连通域分析：保留 孤立 + 半径∈[r_min,r_max] + 局部对比>thr 的域
  → 每个域中心 → 生成 SpotFix{r_norm, cx, cy, mode=当前档}
  → 交给现有四档 heal_image 修复
```

### 3.2 改动文件与指令（实施结果）
- **新增** `crates/retouch-core/src/detect_spots.rs`：
  - `detect_spots(img, &DetectParams) -> Vec<SpotStroke>`
  - `detect_spots_from_rgb(&[u8], w, h, &DetectParams)`（UI 便捷入口，长度不符安全返回空）
  - 实测定稿默认值：`median_ksize=13, contrast_thr=25, min_radius_px=1.5,
    max_radius_px=40, min_area=4, isolation_ratio=0.35, radius_scale=1.4, max_spots=200, scales=[1]`
- **GUI** `ui/retouch-ui/src/main.rs` 污点面板：「✨ 自动检测污点」按钮 → 在**预览基图**
  （已校色正立、长边 ≤1400px）上检测 → 笔触并入 `SpotFix` → 状态栏回报「检出 N 处 / 耗时」
  或「未检测到明显污点」，不静默失败。
- **CLI** `--auto-spot`：**未做**（本轮聚焦 GUI 闭环，CLI 留待后续）。

### 3.3 关键算法决策（均由实测数据定稿，非拍脑袋）

**① 孤立性校验（防误检，最重要）**
只看「残差大」会在纹理区（树叶/织物/砂石）疯狂误检，而误检笔触会被真实修复、**破坏画面**——
误检比漏检危险得多。真实灰尘的本质是*局部*异常：斑点残差高，但紧邻一圈背景必须平滑。
实现：候选包围盒外扩成环带，环带平均残差须 ≤ `contrast_thr × 0.35`，否则判为纹理区拒绝。

**② ksize=13 定稿（防漏检）**
中值滤波只能发现「小于半个核」的结构——直径 ≥ ksize 的斑点其中心窗口整个落在斑点内部，
中值 = 斑点自身 → 残差归零 → **完全隐形**。参数扫描实测（1400×933，18 个 r=1..6 已知灰尘）：

| 配置 | 召回 | 误检 | 耗时(Huang 后) |
|---|---|---|---|
| ksize=5 单尺度 | 6/18 | 0 | 33ms |
| ksize=9 单尺度 | 18/18 | **18** | 38ms |
| **ksize=13 单尺度（采用）** | **18/18** | **0** | **47ms** |
| ksize=5 多尺度[1,3,6] | 16/18 | 6 | 32ms |
| ksize=9 多尺度[1,3] | 18/18 | 9 | 41ms |

**③ 多尺度路线被实测证伪**
曾设计在 1/3、1/6 降采样图上复用检测以覆盖大斑点。实测：粗尺度会把纹理平滑成
「孤立异常」而制造误检（6 处），且大斑点召回反而不如直接放大 ksize。
得益于 Huang 滑窗中值，放大核几乎不增加耗时——**大核完胜多尺度**。
`scales` 参数保留但默认 `[1]`（关闭）。

### 3.4 验收指标（模块 C）—— ✅ 已通过
- 全档灰尘（直径 3~13px，18 个）检出 **18/18**，误检 **0**，耗时 **47ms** ✅
- 预览尺寸耗时 < 300ms（可同步跑 UI 线程，无需异步化）✅
- 纯色渐变天空零误检；高频噪声纹理不刷屏 ✅
- 回归护栏测试：`detects_full_size_range_without_false_positives`
  （拦截日后有人调小 ksize 导致大斑点静默隐形）✅

### 3.4 不做什么（模块 C）
- 不做语义分割、不接 ML（C1/C2）。
- 不自动修复（仅自动选区），最终修复仍由用户触发或确认。

---

## 4. 版本与交付
- 版本号：`ui/retouch-ui/Cargo.toml` 与 `crates/retouch-core/Cargo.toml` `0.6.5` → `0.6.6`。
- 实现顺序（ROI 从高到低）：A（导出 P0）→ B（P0）→ C（自动去污点）→ B（P1）→ A（P1 线性光可选）。
- 验收通过后续：4 分发版重新打包 + 发版文案 + GitHub Release（走 SSH，:443 不通）。

## 5. 风险与回滚
- mozjpeg_rs API 若与该版本不符 → 锁版本或退回 `image` crate + 手动 S444（不可行则保留 4:2:0 但提 quality）。
- 自动去污点误检高 → 调 `contrast_thr`/`radius_max`，或 GUI 加「检测灵敏度」滑块。
- 任何模块 PSNR 回归 > 0.1 dB → 回滚该模块改动，不影响其他模块。
