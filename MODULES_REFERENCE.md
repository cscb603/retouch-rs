# 模块参考：darktable 19+ 模块 → retouch-rs OKLCH 原生设计

> 用途：把自有技能 `darktable-cli-retouch` 反推的 19+ 模块知识，转译成 retouch-rs 的
> OKLCH 原生实现对照。重点是**哪些 darktable 的坑被 OKLCH 架构天然规避**、**哪些功能要原样保留**。
> 这是设计阶段的参考资料，不是代码规格。

---

## 0. 总览：为什么这套参考有价值

darktable 是「线性 RGB 模块堆叠」的代表，其踩过的坑（假色、断层、色温覆盖偏色）正是我们要从底层规避的。
逐模块对照后，结论很清晰：

| darktable 坑 | OKLCH 架构是否天然规避 | 说明 |
|---|---|---|
| 假色（荧光蓝/洋红/蜡黄） | ✅ 是 | L/C/H 解耦 + 亮度联动彩度衰减 + 色相约束 |
| 色阶断层 | ✅ 是 | 全程 f32 计算 + OKLCH 感知均匀，避免线性 RGB 的 gamma 断层 |
| 色域硬裁切（生硬边缘） | ✅ 是 | 改用色域软裁切（降 C 不截断） |
| 线性高光滑块 → 荧光蓝 | ✅ 是 | 改用 AgX/Filmic 肩部平滑压缩（zentone） |
| 温度模块覆盖 as-shot WB → 青绿偏 | ✅ 是 | 我们根本不碰相机 WB，白平衡做成**加性色相旋转** |
| colorbalancergb C 过大 → 全图染色 | ⚠️ 仍需护栏 | 保留「C 宁小勿大」经验，但封装成 API 默认值 + UI 范围 |
| diffuse 整图模糊 | ⚠️ 仍需护栏 | 柔光做成亮度蒙版限定高光区 |
| 多模块累积损耗（像素烤熟再改） | ✅ 是 | 声明式参数 + 每次从原图全管线重渲（agx-photo 模式） |

---

## 1. 调明暗（Brightness / Lightness）

### 1.1 exposure（已验证 modversion 7）
- darktable：`int mode; float black; float exposure; float deflicker_percentile; float deflicker_target_level;`
- 本质：线性空间乘子 `2^EV`。
- **retouch-rs 设计**：`exposure.ev: f32`，在**线性 f32 阶段**乘 `2^ev`，早于 OKLCH 转换。
- 规避点：无（逻辑一致，只是搬到线性段做）。

### 1.2 shadhi（阴影/高光，modversion 5）
- darktable：order/radius/shadows/whitepoint/highlights/compress/shadows_ccorrect/highlights_ccorrect...
- 本质：分区亮度重建（暗部提、高光压，带色彩校正）。
- **retouch-rs 设计**：拆成 OKLCH 空间的两段 S 曲线（见 2.2 调光比），`shadows`/`highlights` 直接作用于 L 的趾部/肩部。
- 规避点：darktable 在线性 RGB 做易出假色；我们在 OKLCH L 通道做，C/H 不动 → 不染色。

### 1.3 hazeremoval（去雾，modversion 3，16 字节）
- darktable：`float strength; float distance; int compatibility_mode; int adaptive;`
- 本质：暗通道先验去雾。
- **retouch-rs 设计**：`dehaze.strength: f32`（0–1）。JPG 上用暗通道估计雾浓度，轻度去雾提通透。
- 注意：skill 复盘指出 strength=0.6 会偏冷紫 → 我们的去雾后接一个极轻微的暖补偿（或 UI 提示）。

---

## 2. 调光比（Contrast / Light Ratio）

### 2.1 filmicrgb（modversion 6，18 floats + 11 ints）
- darktable：完整 S 曲线 + 黑/白点 + 饱和度保护。
- **retouch-rs 设计**：**不照搬 filmicrgb**，改用 `zentone` 的 AgX/Filmic 作为「影调映射」阶段（见管线）。
- 规避点：filmicrgb 在线性 RGB 调反差时高光易荧光蓝；AgX 是 perceptually-inspired 的肩部压缩，天然防假色。
- 我们保留「反差 / 黑场 / 白场」三个可调旋钮，但底层是 AgX 而非 filmicrgb 原算法。

### 2.2 对比度 / 高光 / 阴影 / 白场 / 黑场 / 色调曲线
- **retouch-rs 设计**（全部在 OKLCH L 通道）：
  - `contrast: f32` → L 的 S 曲线（围绕 0.5 中点）。
  - `highlights` / `shadows` → L 曲线的肩/趾局部压缩/抬起。
  - `whites` / `blacks` → 端点定位（S 曲线上下沿）。
  - `tone_curve` → 可选：用户自定义 L 映射（仍作用于 OKLCH L）。
- 规避点：传统软件在 RGB 拉对比度会动 C/H；我们锁 C/H → 对比度变化**不染色**。

### 2.3 sigmoid（modversion 3，显示变换，12 floats + 2 ints）
- darktable：sigmoid 是显示端变换，**不能设 identity**。
- **retouch-rs 设计**：我们用 AgX/Filmic 已经包含显示映射角色，sigmoid 不再单独Needed；若保留，作为可选「显示伽马微调」放在管线末端（OKLCH→线性后、sRGB 编码前）。

---

## 3. 调色彩风格（Color Style）

### 3.1 temperature / channelmixerrgb（白平衡，modversion 4 / 3）
- darktable 大坑：`temperature` 会**覆盖相机 as-shot WB → 青绿色偏**，skill 默认不加载。
  `channelmixerrgb`（色彩校准）更可控，但仍是「白平衡 + 通道混合」。
- **retouch-rs 设计（M2b 已实现）**：
  - **完全不碰相机 WB**（JPG 没有 RAW 级 WB 数据，也没必要）。
  - 白平衡做成**线性 RGB 通道增益**（物理正确的 WB 位置，在 tone map 之前）：
    `wb.temp: f32`（`>0` 暖 = R×1.1 / B×0.9，`<0` 冷 = R↓/B↑）、`wb.tint: f32`（`>0` 品红 = G↓，`<0` 绿 = G↑）。
    两者都为 0 时恒等短路，零开销。
  - 这是「风格化白平衡」，不是「校正白平衡」，从根上避开覆盖 as-shot 的偏色。
- 规避点：darktable 的 WB 陷阱在我们架构里**不存在**（我们没有「相机白平衡」这个概念）。

### 3.2 colorbalancergb（分区色平衡，modversion 3，32 floats + 1 int）
- darktable 大坑：`*_H` 是角度、`*_C` 是标量；`global_C` 默认配 `global_H=0`（红），不指定 H 会全图泛红；**C 宁小勿大（0.05–0.10 足够）**。
- **retouch-rs 设计**：`grade.shadows_H/C`、`grade.midtones_H/C`、`grade.highlights_H/C` → 转成 OKLCH 的三分区（按 L 分暗/中/亮）色相旋转 + 彩度加性。
  - 默认值全部 = 0（无变化），UI 上限收紧到 ±0.15。
  - H 必须有明确角度，API 强制要求 `hue` 与 `chroma` 成对 → 杜绝「忘了给 H 就泛红」。

### 3.3 colorequal（按色相分区调，modversion 4，6 floats + 25 floats）
- darktable 大坑：`sat_*` / `bright_*` 是**乘法因子**（1.0=不变，1.2=+20%，0.8=−20%），不是绝对增量；误把 0.2 当 +0.2 会严重去色。
  8 分区：red/orange/yellow/green/cyan/blue/lavender/magenta。
- **retouch-rs 设计**：`hue_eq.<zone>.sat`、`hue_eq.<zone>.bright`、`hue_eq.<zone>.hue_shift`。
  - 语义与 darktable 一致：**乘法因子**，默认值 1.0。
  - 实现：在 OKLCH 按 H 落区（8 个 45° 扇区），对该像素 C×factor / L×factor / H+shift。
  - 保留 guided-filter 思路（平滑分区边界）可选。

### 3.4 vibrance（自然饱和度，modversion 2，float amount）
- darktable：`float amount`，低彩度优先增饱和（更自然）。
- **retouch-rs 设计**：`vibrance: f32`。实现：`C *= 1 + amount * (1 - C_norm)`（彩度越低增益越大）。
  - 这天然符合 OKLCH（C 直接可乘），比 darktable 在 RGB 里做更可控。

### 3.5 colisa（对比/亮度/饱和，modversion 1，contrast/brightness/saturation）
- darktable 大坑：**默认 0,0,0=无变化**（不是 1,1,1）。
- **retouch-rs 设计**：`saturation: f32`（OKLCH C 整体乘，受亮度联动约束）、`brightness`（OKLCH L 整体加减）。默认 0。
  - 注意：我们把它拆成「saturation（调色彩风格）」+「brightness（调明暗）」，不混在一个模块。

### 3.6 colorzones（HSL 分区曲线，modversion 5，520 字节）
- darktable 大坑：`curve_y` 是 3×20 浮点，L 在 0-19, C 在 20-39, H 在 40-59；**错放通道曲线无效**；strength=0 恒等。
- **retouch-rs 设计**：暂作为**进阶能力**保留概念，但我们用更直观的「分区色相 HSL」替代（见 3.3 + 3.5），不暴露 60 节点曲线给普通用户。
  - 若 M 后期要做「精细分区」，直接用 OKLCH 的 L/C/H 三维软蒙版（参考 skill 的 `mask_blend.py` L/a/b/C/h 任意组合）。

### 3.7 色彩分级 Split Tone（高光/阴影分离色调）
- darktable 无独立模块，靠 colorbalancergb 分区模拟。
- **retouch-rs 设计**：`split_tone.shadow_hue/C`、`split_tone.highlight_hue/C` → OKLCH 按 L 分暗/亮两段独立色相旋转。
  - 比 darktable 更直白（darktable 要手动配 shadows_H/midtones_H/highlights_H）。

---

## 4. 去假色（De-fake-color，OKLCH 专属核心）

这一大类是 darktable 没有的「架构级保障」，来自 skill 复盘 + 豆包帖子思路。

### 4.1 亮度联动彩度衰减 Chroma Decay
- 思想：`C_out = C_in · f(L)`。高光降饱和（防荧光蓝）、暗部压彩噪（防色块）。
- **retouch-rs 设计**：`color.chroma_decay: f32`（0–1）。
  - `f(L) = 1 - decay * (高光权重 + 暗部权重)`。
  - 默认轻微开启（如 0.1），可在 UI 关。
- 规避点：darktable 没有这个机制，高光滑块直接拉就出假色。

### 4.2 天空色相约束 Sky Constraint
- 思想：天空 `H∈[210,240]`（蓝）限幅 C，防荧光蓝/赛博蓝。
- **retouch-rs 设计**：`color.fix_sky: bool`。命中天空扇区时 C 上限收紧。
- 规避点：对应 darktable 拉高光/加蓝时的荧光蓝。

### 4.3 肤色保护 Skin Protect
- 思想：肤色 `H∈[20,45]` 独立蒙版，防洋红/蜡黄。
- **retouch-rs 设计**：`color.protect_skin: bool`。命中肤色扇区时，限制 H 偏移幅度 + 轻度压 C。
- 规避点：对应 darktable colorbalancergb 染到肤色、diffuse 柔化肤色失真。

### 4.4 色域软裁切 Gamut Soft-clip
- 思想：颜色溢出 sRGB 色域时**降 C 不截断**（esoc-color 思路），避免生硬边缘。
- **retouch-rs 设计**：`color.gamut_softclip: bool`（默认开）。在 OKLCH→线性后、sRGB 编码前做：若 C 过大导致 RGB 超界，按比例降 C 直到不超界。
- 规避点：darktable colorout 硬裁切会丢色阶；我们软裁切保平滑。

---

## 5. 细节 / 特效 / 几何（M4b / M5 / 原 M6，✅ 已实现）

### 5.1 denoiseprofile（降噪，modversion 12，416 字节）→ ✅ 已实现 `detail.rs::bilateral_denoise`
- darktable 大坑：`a[0]=-1` 哨兵触发 RAW 噪声画像自动检测；JPG 上自动模型精度不同。
- **retouch-rs 设计**：JPG 无 RAW 噪声画像，**只做轻量近似**——亮度域联合 bilateral（`detail.rs`）：5×5 空间高斯 + sRGB 亮度域 range 权重，radius 固定 2，range sigma 随 strength 增大；色彩边缘受保护、不放大颗粒。
- 规避点：我们不依赖 RAW 数据，行为可预期；`Detail::is_identity()`（denoise≤0）短路零重采样。
- CLI `--denoise <0..1>`，GUI「细节后处理 · 降噪」，参数注册表 `DetailDenoise`（SoftKnee）。

### 5.2 sharpen（锐化，modversion 1）→ ✅ 已实现 `detail.rs::unsharp_sharpen`
- darktable：`radius/amount/threshold`。
- **retouch-rs 设计**：`Detail.sharpen`，阈值保护 USM——`gaussian_blur(1)` 作低频，噪声阈值 `thr = 4 + 8·(1-amount)` 屏蔽颗粒，增益 `k = 1.3·amount`，绝不放大噪点。
- CLI `--sharpen <0..1>`，GUI「细节后处理 · 锐化」，参数注册表 `DetailSharpen`（SoftKnee）。

### 5.3 diffuse（柔光/扩散，modversion 2，60 字节）→ ✅ 已实现 `detail.rs::glow`
- darktable 大坑：dreamy.json 是**整图模糊**，单渲必糊；正确做法是**亮度蒙版只让亮区发柔光**。
- **retouch-rs 设计**：`Detail.diffuse`，**强制亮度蒙版限定**——`gaussian_blur(6)` 取低频，highlight bias `smoothstep(0.45,0.95,L)` 仅高光泛光，**暗部不动、不整图模糊**。
- CLI `--diffuse <0..1>`，GUI「细节后处理 · 柔光」，参数注册表 `DetailDiffuse`（SoftKnee）。

### 5.4 vignette（暗角，modversion 4）
- darktable：8 floats + 3 ints。
- **retouch-rs 设计**：`vignette.strength/radius`，径向亮度衰减（OKLCH L）。
- ⚠️ **按用户明确要求已取消**："m 到 m7 暗角就不要了"。retouch-rs 全管线不实现暗角，不留接口。

### 5.5 graduatednd（渐变滤镜，modversion 1）
- darktable：density/hardness/rotation/offset/hue/saturation。
- **retouch-rs 设计**：`gradnd.density/angle/offset/hue`，线性渐变区域做 L/C/H 调整。

### 5.7 高级修图（原 M6：频谱磨皮 + 金字塔融合，✅ 已实现 `advanced.rs`）
> darktable 对应能力分散在 `skin_retouch.py`（频率分离磨皮）与 `mask_blend.py`（金字塔融合）思路，retouch-rs 移植为原生 Rust。

- **频谱磨皮 `FreqSepSkin`**（对标 `skin_retouch.py`）：
  - 拆出平滑低频层（大尺度肤色）+ 高频层（毛孔纹理），重建 `smoothed_low + texture_keep·high`。
  - 用肤色概率蒙版（YCbCr 肤色簇高斯 + 亮度门控）限定皮肤像素，发丝/背景/眼睛零触碰。
  - 五参：`enabled` / `strength`(0..1) / `texture_keep`(0..1) / `smoothness`(0..1，控制低频半径) / `mask_feather`(0..1，蒙版羽化)。
  - `is_identity()`：未启用或 strength≤0 时严格恒等。
  - CLI `--freqsep --freqsep-strength --freqsep-texture --freqsep-smooth --freqsep-feather`；GUI「高级修图」折叠区；参数注册表 `FreqSepStrength/Texture/Smooth/Feather`（SoftKnee）。
- **金字塔融合 `PyramidFusion`**（对标 `mask_blend.py`）：
  - 4 级高斯模糊（半径 [2,4,8,16]）分解，按 per-scale profile `[0.5,1.0,0.8,0.3]` 用 `gain = 1 + strength·detail_scale·profile` 重组，产生跨尺度自然细节/局部反差（非扁平 USM）。
  - `is_identity()`：未启用或 strength≤0 时所有 gain=1，各带 telescoping 回退原图（恒等）。
  - 两参：`enabled` / `strength`(0..1) / `detail_scale`(默认 1.0，额外倍率)。
  - CLI `--pyramid --pyramid-strength --pyramid-scale`；GUI「高级修图」折叠区；参数注册表 `PyramidStrength/Scale`（SoftKnee，center=1.0/half=2.0）。
- 单测：`advanced_identity_is_noop` / `freqsep_smooths_skin_pixels` / `pyramid_zero_is_identity` / `pyramid_changes_image_when_strength_nonzero` 全过。

### 5.8 几何预处理（M4b：裁剪/旋转/透视，✅ 已实现 `geometry.rs`）
> 在「解码后、线性化前」作为独立预处理阶段（坐标重采样，从底层自研，无新依赖）。

- **透视纠正 Perspective**：单应矩阵 homography keystone（`(v_key, h_key)` ∈ -1..1），8×8 高斯解同态 + 3×3 求逆 + 双线性重采样；解算失败时原样返回（不崩）。
- **旋转 Rotate**：90° 步进用精确整数旋转（`imageops::rotate90/180/270`），任意角走双线性仿射 warp（逆时针，度）。
- **翻转 Flip**：水平 / 垂直（`imageops::flip_horizontal/vertical`）。
- **裁剪 Crop**：归一化矩形 `x,y,w,h` ∈ 0..1，作用于当前图。
- 顺序严格：透视 → 旋转 → 翻转 → 裁剪；`Geometry::is_identity()` 为真时整阶段零重采样，维持 M0 像素级 round-trip。
- CLI `--crop X,Y,W,H` / `--rotate` / `--flip-h` / `--flip-v` / `--persp-v` / `--persp-h`；GUI「几何预处理」折叠区（旋转/透视纵/透视横 slider + 翻转 checkbox + 4 个 crop slider + 清除裁剪）；参数注册表 `GeomRotate`(Linear) / `GeomPerspV` / `GeomPerspH`(SoftKnee)。

### 5.6 retouch-rs 原生新增模块（M5–M7，感知 / 智能，✅ 已实现）
> 对应 darktable 无直接对等物；是 retouch-rs 按用户"智能符合人眼、减少纯线性生硬感"指令自研的 OKLCH 原生能力。

- **感知滑块映射 `perceptual`**（M5a）：`CurveKind{Linear, SoftKnee, LogSat}` + `slider_to_raw/raw_to_slider` 互逆。
  - `SoftKnee`：中心中和 + `tanh(SOFT_K·p)/tanh(SOFT_K)`（`SOFT_K=2.2`）加权衰减 → 两端趋缓，消除纯线性拉杆生硬感。
  - `LogSat`：饱和度类走对数 `center·(p·half.ln()).exp()`，低饱和区更跟手。
  - 3 单测全过（线性过中心 / SoftKnee 中性且两端缓 / LogSat 中心恒等）。
- **粉嫩肤色 `skin`（M5b，已扩展意图级控制）**：OKLCH 肤色概率遮罩（色相高斯 `中心35°` × 彩度门 × 亮度门 `0.12..0.92`）→ 命中像素 H/C/L 拉向健康粉嫩目标（`hue_target≈25°`/`chroma_target≈0.10`/轻提亮），非肤色保护（默认开）；`strength` 强度 + `smoothness` 羽化；`SkinTone::pink()` 健康默认。单测 `skin_tone_pinkifies_skin_leaves_blue` / `skin_off_is_identity` 全过。
  - **意图级控制（本次新增）**：`去黄/减淡/加红/加粉` 四个小白友好滑块经 `resolved_targets()` 解析成 OKLCH 目标（hue/chroma/lift 的加性组合），后台自动算，用户无需色相度数。CLI 同步 `--skin-yellow/-lighten/-redden/-pinken`。
- **多分区亮度融合 `zones`（M5b）**：4 区高斯权重平滑融合（`ZONE_CENTERS=[0.12,0.32,0.6,0.85]`、`ZONE_WIDTH=0.22`），`lift:[shadows,dark_mid,light_mid,highlights]` 无硬边合成到 L。单测 `zone_lift_brightens_shadows` 过。
- **胶片感 S 曲线 `film_curve`（M5b）**：`L += film_curve·sin(π·(L−0.5))`，toe+shoulder 两端斜率→0，中间调加胶片般柔和反差。单测 `film_curve_adds_contrast` 过。
- **光比融合 `light_ratio`（M5b）**：多膝曲线 `L = pivot + d·(1 + lr·(1 − xc²))`，均值保持（不整体提亮），一键压/拉光比。单测 `light_ratio_fusion_works` 过。

---

## 6. 可直接复用的「方法论」（非模块，但很重要）

来自 skill 的 AI 调色闭环经验，retouch-rs 的 M5 接入 retouch_app 时直接复用：

1. **客观指标驱动**：Lab 度量（meanL/stdL/a*/b*/per-hue C/肤色/中性灰）—— 我们的 OKLCH 同样可算这些指标（L/C/H 直接映射）。
2. **护栏（guardrail）**：肤色 C±7 / 过饱和>3% / 偏色+2 / 削波+1 → 硬拒。我们的去假色模块本身就是「前置护栏」。
3. **甜点搜索**：从当前 cfg 向目标插值采样 t∈[1.0,0.65,0.35] → 小图渲染 → 护栏 → 评分选优。
4. **声明式 + 原图重渲**：非破坏性，undo = 回退参数集（我们天然满足，因为管线每次从原图跑）。
5. **AI 只看缩图（512–1024px Q75）**：文本决策不收图，视觉模型只起名点评。

---

## 7. 结论

- **19+ 模块中，约 60% 可直接映射为 OKLCH 原生实现，且更稳（不染色/不假色）**。
- **约 30% 是 darktable 的坑，在我们架构里根本不存在**（WB 覆盖偏色、线性高光假色、色域硬裁切、像素累积损耗）。
- **约 10% 是进阶能力**（colorzones 60 节点曲线、denoiseprofile RAW 级降噪），JPG 场景降级为轻量版或留待后期。
- **去假色四大件（chroma decay / sky / skin / gamut soft-clip）是我们的核心差异点**，darktable 没有对等物。

---

## 8. 预设/CLI 参数映射表（M3 落地）

retouch-rs 的 TOML 预设键与 CLI 旗标刻意对齐 darktable 模块名，便于 M7 零改动接入 `retouch_app`。

| darktable 模块 | darktable 关键参 | retouch-rs 预设键 | retouch-rs CLI 旗标 | 说明 |
|---|---|---|---|---|
| `exposure` | `exposure` (EV) | `[exposure] ev` | `--exposure` | 线性乘子 `2^ev` |
| `filmicrgb` / `sigmoid` | 肩部压缩 | `[tone_map] mode` | `--tone-map` (`none`/`agx`/`filmic`) | zentone 肩部；推曝光自动 AgX |
| `colorzones`(chroma) | chroma 控制 | `[defake] enabled/chroma_decay/fix_sky/protect_skin` | `--defake` / `--chroma-decay` | 我们的去假色（暗调对齐 colorzones 的 chroma 思路） |
| `shadhi`+`levels`+`toneequal` | 微调节 | `[grade] brightness_lift/contrast/dehaze/shadow_lift/deep_shadow_lift` | `--brightness`/`--contrast`/`--dehaze`/`--shadow-lift`/`--deep-shadow-lift` | OKLCH L 通道 |
| `channelmixerrgb`(色彩校准) | WB/通道 | `[white_balance] temperature/tint` | `--temp`/`--tint` | 线性 RGB 通道增益（风格化 WB） |
| `vibrance` | `amount` | `[color] vibrance` | `--vibrance` | 低彩度优先 |
| `colisa`(saturation) | `saturation` | `[color] saturation` | `--saturation` | OKLCH C 乘子 |
| 全局色相 | — | `[color] hue_rotate` | `--hue-rotate` | 创意偏色 |
| 色彩分级 | shadows/mid/high H | `[color] split_shadow/split_highlight` | `--split-shadow`/`--split-highlight` | 按 L 分暗/亮两段 |
| `colorzones`(HSL 分区) | 8 区 H/S/L | `[hsl.<band>] hue/sat/light` | `--hsl <band>:<h>,<s>,<l>` | ACR 8 色相带（red/orange/yellow/green/aqua/blue/purple/magenta） |
| （无直接对等 / 自研） | 感知滑块映射 | — | （GUI `param_slider` 走 `CurveKind`） | `perceptual.rs`：SoftKnee/LogSat 非纯线性，中心中和 + `tanh` 加权衰减 |
| （无直接对等 / 自研） | 胶片感 S 曲线 | `[grade] film_curve` | `--film-curve` | 正弦 toe+shoulder，柔和反差过渡 |
| （无直接对等 / 自研） | 光比融合 | `[grade] light_ratio` | `--light-ratio` | 多膝曲线，均值保持，一键压/拉光比 |
| （无直接对等 / 自研） | 粉嫩肤色 | `[skin] enabled/strength/hue_target/chroma_target/light_lift/smoothness/protect_non_skin` | `--skin --skin-strength --skin-hue --skin-chroma --skin-light --skin-smooth --skin-protect` | OKLCH 肤色概率遮罩 → 拉向健康粉嫩目标；`SkinTone::pink()` 健康默认 |
| （无直接对等 / 自研） | 多分区亮度融合 | `[zones] shadows/dark_mid/light_mid/highlights` | `--zone-shadows --zone-dark-mid --zone-light-mid --zone-highlights` | 4 区高斯平滑无硬边 |
| `geometry`（自研预处理） | 透视/旋转/翻转/裁剪 | `[geometry] crop=[x,y,w,h]/rotate_deg/flip_h/flip_v/perspective=[v,h]` | `--crop X,Y,W,H --rotate --flip-h --flip-v --persp-v --persp-h` | 解码后线性化前的坐标重采样；顺序 透视→旋转→翻转→裁剪 |
| `denoiseprofile`（轻量近似） | 降噪 | `[detail] denoise` | `--denoise` | 亮度域联合 bilateral，JPG 友好，不放大颗粒 |
| `sharpen`（USM） | 锐化 | `[detail] sharpen` | `--sharpen` | 阈值保护非锐化掩蔽，绝不放大噪点 |
| `diffuse`（高光柔光） | 柔光/扩散 | `[detail] diffuse` | `--diffuse` | 高光蒙版 glow，不整图模糊、暗部不动 |
| `skin_retouch`（频率分离） | 频谱磨皮 | `[advanced] freqsep_enabled/freqsep_strength/freqsep_texture_keep/freqsep_smoothness/freqsep_mask_feather` | `--freqsep --freqsep-strength --freqsep-texture --freqsep-smooth --freqsep-feather` | 肤色蒙版限定的频率分离磨皮，发丝/背景零触碰 |
| `mask_blend`（金字塔） | 金字塔融合 | `[advanced] pyramid_enabled/pyramid_strength/pyramid_detail_scale` | `--pyramid --pyramid-strength --pyramid-scale` | 4 级拉普拉斯式多层细节融合，strength=0 恒等 |

> 预设为基底、CLI 旗标（`Option`）覆盖；既不给预设也不给旗标 → 恒等往返。
> 完整 TOML 范例见 `presets/factory.toml`、`presets/summer_warm.toml`。
> 负值参数须用 `--flag=value`（如 `--zone-highlights=-0.1`），否则 clap 把 `-0.1` 当旗标。

→ **M4b / M5 / 原 M6 已全部实现并跑通**：几何预处理（裁剪/旋转/透视/翻转）、细节后处理（降噪/锐化/柔光）、高级修图（频谱磨皮/金字塔融合），以及感知滑块（SoftKnee/LogSat）、粉嫩肤色、胶片感 S 曲线、光比融合、多分区融合、智能一键 GUI、暗角取消。
→ **剩余方向**：M7 接入 `retouch_app` AI 编排（文本决策 + 视觉起名点评闭环）；graduatednd 渐变滤镜（5.5）暂未实现，留待后期。
