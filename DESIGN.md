# retouch-rs 设计文档（v0.2，已评审通过）

> 目标：从底层自研一个**半指令式**修图工具，只处理 JPG/TIFF，非线 性 OKLCH 管线，
> 规避传统专业软件（darktable / PS / LR）的假色、断层、DLL 依赖等坑。
> v0.1 草稿已评审，本版锁定 4 项决策并并入用户追加的功能（几何构图 + 高级修图）。

---

## 1. 目标与原则

1. **从底层自研，不依赖任何第三方修图软件**：无 darktable、无 LibRaw、无 Photoshop 外部进程。
2. **只处理 JPG / TIFF**（用户明确范围；最高画质走 16-bit TIFF）。
3. **非线 性、感知均匀**：全程在 OKLCH 色彩空间工作，L/C/H 三通道解耦。
4. **单文件二进制、零系统依赖**：Windows + macOS 双系统同源编译，无 `140.dll` 类污染。
5. **半指令式交互**（M4）：类 ACR 滑块面板 + Cmd+K 命令盘。
6. **去假色默认常开**（v0.2 决策）：每张图过一遍亮度联动彩度衰减 + 天空/肤色约束 + 色域软裁切，从根上防假色。
7. **CLI / 预设 100% 对齐 darktable-cli 参数名**（v0.2 决策）：便于 M7 接入 `retouch_app` 时零改动替换后端。
8. **所有滑块走「感知映射」，非纯线性**（M5a 决策）：滑块位置→实际值经 `CurveKind{Linear/SoftKnee/LogSat}` 映射；
   中心中和（位置 0 = 无变化）+ `tanh` 加权衰减（SoftKnee，两端趋缓，消除纯线性生硬感）+ 饱和度对数（LogSat）。
   目标：比传统软件更"跟手"、更合人眼，避免线性拉杆的突兀跳变。

---

## 2. 功能分类（六大类，v0.2 扩展）

在 v0.1 四大类基础上，并入用户追加的**几何构图**与**高级修图**。

### 2.1 调明暗（Brightness / Lightness）
- 曝光 Exposure（线性乘子 `2^EV`）
- 黑位/提亮 Lift（阴影区加亮，OKLCH L 抬升 + 趾部）
- 亮度 Brightness（OKLCH L 整体加减）
- 去雾/通透（暗通道轻度去雾）

### 2.2 调光比（Contrast / Light Ratio）
- 对比度 Contrast（OKLCH L 的 S 曲线）
- 高光 Highlights / 阴影 Shadows（影调曲线肩/趾）
- 白场 Whites / 黑场 Blacks（端点定位）
- 参数色调曲线 Tone Curve（可选）
- **影调映射 Tone Map（Filmic / AgX）**：非线 性肩部压缩（zentone），**替代线性高光滑块 → 根治荧光蓝/蜡黄**
- **胶片感 S 曲线 `film_curve`（M5b，✅ 已实现）**：正弦 S 曲线 `L += film_curve · sin(π·(L−0.5))`，toe+shoulder 两端斜率→0，
  给中间调加胶片般柔和的反差过渡（非硬 S），`0`=关，`±0.25` 常用区间；走 SoftKnee 感知滑块。
- **光比融合 `light_ratio`（M5b，✅ 已实现）**：单一感知控制 = 阴影提升 + 中间调分离 + 高光压缩的多膝曲线，
  `L = pivot + d·(1 + lr·(1 − xc²))`，均值保持（不整体提亮），一键压/拉光比；走 SoftKnee 感知滑块。
  > 注：**暗角（vignette）按用户明确要求已取消**，不再实现（"m 到 m7 暗角就不要了"）。

### 2.3 调色彩风格（Color Style）
- 白平衡 WB / 色温·色调（**线性 RGB 通道增益**，物理正确的 WB 位置：`temp>0` 暖/R↑B↓、`tint>0` 品红/G↓；出厂档恒等）✅ M2b 已实现
- 自然饱和度 Vibrance（低彩度优先增饱和：boost 权重随 C 增大而减小，红不爆、浊中调活）✅ M2b 已实现
- 饱和度 Saturation（OKLCH C 整体乘子，1.0=不变）✅ M2b 已实现
- 全局色相旋转 Hue Rotate（创意偏色，仅作用于有色相像素，中性灰不动）✅ M2b 已实现
- 色彩分级 Split Tone（高光/阴影分离色调：阴影用 `1-smoothstep(0,0.5,L)`、高光用 `smoothstep(0.5,1,L)` 加权，中性灰天然隔离）✅ M2b 已实现
- 分区色相 HSL（ACR 式 8 色相带，每带独立 H/S/L；三角最近邻混权=partition of unity，无缝无溢出；band 中心用 palette 实测 OKLCH 角：红 29/橙 53/黄 110/绿 142/青 195/蓝 264/紫 294/品红 328）✅ M2c 已实现
- 胶片模拟 / 风格 LUT（可选，后期）
- **粉嫩肤色 `skin`（M5b，✅ 已实现，本次扩展意图级控制）**：OKLCH 肤色概率遮罩（色相高斯 `中心35°` × 彩度门 × 亮度门 `0.12..0.92`）
  → 把命中像素的 H/C/L 拉向「健康粉嫩」目标（默认 `hue_target≈25°`/`chroma_target≈0.10`/轻提亮），
  非肤色保护（默认开）；`strength` 控制强度，`smoothness` 控制遮罩羽化。比 darktable 的 colorbalancergb 染肤色更可控、零洋红/蜡黄。
  - **意图级控制（✅ 本次新增，小白友好）**：GUI 用"去黄 / 减淡 / 加红 / 加粉"四个白话滑块替代原 raw 色相/彩度/提亮滑块；
    后台经 `resolved_targets()` 解析成 OKLCH 目标：
    `hue = hue_target + pinken·10 − yellow_reduce·10`，`chroma = chroma_target + redden·0.04 + pinken·0.02`，
    `lift = light_lift + lighten·0.06`。用户无需懂色相度数，只说"想去黄/加点粉"即可。
  - CLI 同步新增 `--skin-yellow --skin-lighten --skin-redden --skin-pinken`（与原 `--skin-*` 族对齐）。

### 2.4 去假色（De-fake-color，核心差异点，OKLCH 专属，**默认常开**）
- 亮度联动彩度衰减 Chroma Decay：`C_out = C_in · f(L)`，高光降饱和、暗部压彩噪
- 天空色相约束 Sky Constraint：`H∈[210,240]` 限幅 C，防荧光蓝
- 肤色保护 Skin Protect：`H∈[20,45]` 独立蒙版，防洋红/蜡黄
- 色域软裁切 Gamut Soft-clip：溢出时降 C 不截断（**自研**：palette 转换 + 二分降 C，零额外依赖）

### 2.5 几何 / 构图（Geometry，M4b，✅ 已实现）
- **裁剪 Crop**（后台仍是归一化矩形 `x,y,w,h` ∈ 0..1；**GUI 改为直观的"左裁 / 右裁 / 上裁 / 下裁"四条边缘滑竿**（各 0..0.5），自动换算回矩形；另加 1:1 / 3:2 / 4:3 / 16:9 常用比例一键居中裁切）
- **旋转 Rotate**（90° 步进用精确整数旋转，任意角走双线性仿射 warp；逆时针，度）
- **翻转 Flip**（水平 / 垂直）
- **透视纠正 Perspective Correction**（单应矩阵 homography keystone，纵向/横向 keystone ∈ -1..1）

> 几何变换在「解码后、线性化前」作为预处理的独立阶段；顺序严格：透视纠正 → 旋转 → 翻转 → 裁剪。
> 全部从底层自研（`geometry.rs`：8×8 高斯解同态 + 3×3 求逆 + 双线性重采样），无新依赖。
> `Geometry::is_identity()` 为真时整阶段零重采样，维持 M0 像素级 round-trip。

### 2.6 细节（Detail，M5，✅ 已实现）
对 OKLCH 管线的 **sRGB 8-bit 结果**做最终感知收尾，JPG 友好、与色彩阶段解耦：
- **降噪 Denoise**（亮度域联合 bilateral，5×5 空间高斯 + 亮度域 range 权重，radius 固定 2，
  range sigma 随 strength 增大；保护色彩边缘、不放大颗粒，零拷贝 identity 短路）
- **锐化 Sharpen**（非锐化掩蔽 USM，gaussian_blur(1) 作低频；噪声阈值保护 `thr = 4 + 8·(1-amount)`，
  增益 `k = 1.3·amount`，绝不放大噪点）
- **柔光 / 扩散 Diffuse**（亮度蒙版限定高光的 glow：gaussian_blur(6) 作低频，
  highlight bias `smoothstep(0.45,0.95,L)` 仅高光泛光，**不对整图模糊**、暗部不动）

### 2.7 高级修图（Advanced，原 M6，✅ 已实现）
对细节（M5）之后的 sRGB 8-bit 结果运行，两项均「关闭 / strength=0 时严格恒等」：
- **频谱 / 频率分离磨皮 `FreqSepSkin`**：拆出平滑低频层（大尺度肤色）+ 高频层（毛孔纹理），
  重建为 `smoothed_low + texture_keep·high`，用肤色概率蒙版（YCbCr 肤色簇高斯 + 亮度门控）
  限定皮肤像素，发丝 / 背景 / 眼睛零触碰。`enabled`+`strength`+`texture_keep`+`smoothness`+`mask_feather` 五参。
  算法源自 `darktable-cli-retouch` 的 `skin_retouch.py`，移植 Rust。
- **金字塔式自然影调多层融合 `PyramidFusion`**：4 级高斯模糊（半径 [2,4,8,16]）分解，
  按 per-scale profile `[0.5,1.0,0.8,0.3]` 用 `gain = 1 + strength·detail_scale·profile` 重组，
  产生跨尺度的自然细节 / 局部反差（非扁平 USM）。`strength=0` 时所有 gain=1，各带 telescoping 回退到原图（恒等）。
  思路源自 `mask_blend.py`。

---

### 2.8 多分区亮度融合（Multi-zone Lift，M5b，✅ 已实现）
- **4 区高斯权重平滑融合**（无硬边）：`ZONE_CENTERS=[0.12,0.32,0.6,0.85]`、`ZONE_WIDTH=0.22`。
  每像素按到 4 个亮度中心的高斯距离加权，把 4 个分区抬升量（`lift: [shadows, dark_mid, light_mid, highlights]`）平滑合成到 L。
  比 darktable 的硬分区曲线更自然（无缝过渡，无分带接缝）。CLI：`--zone-shadows --zone-dark-mid --zone-light-mid --zone-highlights`；走 SoftKnee 感知滑块。

### 2.9 感知滑块映射（Perceptual Sliders，M5a，✅ 已实现）
- 核心：`perceptual.rs` 的 `CurveKind{Linear, SoftKnee, LogSat}` + `slider_to_raw` / `raw_to_slider` 互逆映射。
  - `Linear`：纯线性（曝光等需绝对线性量）。
  - `SoftKnee`：中心中和 + `tanh(SOFT_K·p)/tanh(SOFT_K)` 加权衰减（`SOFT_K=2.2`），两端趋缓 → 消除纯线性生硬感（胶片曲线/光比/提亮/对比/去雾/分区等）。
  - `LogSat`：饱和度类走对数 `center·(p·half.ln()).exp()`，低饱和区更跟手（饱和度/HSL 分区饱和）。
- 参数注册表 `params.rs`：`Field` 枚举 + `ParamSpec{label, field, curve, bipolar, center, half, unit, dec, tooltip}` + `registry()`，
  GUI 与 CLI 共用同一套元数据（滑块曲线/范围/单位/小数位集中定义），保证"智能、跟手、符合人眼"。

### 2.10 交互层改进（GUI，本次迭代）
面向小白的自然交互，后台负责技术细节：
- **缩放 / 平移 / 前后对比查看器**：滚轮以光标为锚点缩放、`Shift`+拖拽或中键平移、双击复位；`\` 键按住看原图、`对比:关/开/分屏` 三态切换（分屏带可拖动分割线，借鉴 FilmRust / Lightroom 习惯）。
- **色彩条滑竿**：白平衡色温/色调、饱和/自然饱和、色相旋转、分离色调、肤色色相/彩度、HSL 分区等**色彩类滑块下方自动绘制渐变色彩条**，直观显示该参数影响的颜色方向（蓝→橙、绿→品红、色相环等）。
- **人话提示（on_hover_text）**：每个滑块悬停即显示一句朋友式说明（讲清楚"干嘛的、别调过头"）；滑竿下方实时显示**效果值**（原"实际"易误解，改为"效果值"明确这是经感知映射后的真实生效量）。
- **影调映射说明**：`无 / AgX / Filmic` 三选项悬停解释差异——无=直接输出易过曝、AgX=电影感压高光、Filmic=胶片柔顺灰阶。
- **直观裁剪**：见 2.5，边缘滑竿 + 常用比例一键裁切。
- **命名更直白**："暗部抬起"→"提亮暗部"、"黑位抬起"→"提亮黑位"，降低理解门槛。

---

## 3. 处理管线（每像素 + 几何预处理）

```
JPG/TIFF 解码 (sRGB u8)
  → [几何预处理] 透视纠正 → 旋转 → 裁剪        (M4, 坐标操作)
  → 线性 f32（手动 sRGB 转移函数，精确，无依赖）
  → 曝光（线性乘子 2^EV，仅当 EV>0 时自动套 AgX 护高光）
  → 白平衡 WB（线性 RGB 通道增益 temp/tint，物理正确位置）        ✅ M2b
  → 影调映射 AgX / Filmic（zentone，肩部平滑压缩，**按需启用**：推曝光或选影调时；默认不套，避免无谓提亮毁细节）
  → 转 OKLCH (L, C, H)
  → 去假色(默认常开) → 调色彩风格(饱和度/活力/色相旋转/分离色调) → 调明暗(L 通道) → 分区色相 HSL(M2c)   ✅ M2a/M2b/M2c
  → 胶片感 S 曲线 film_curve(M5b) → 光比融合 light_ratio(M5b) → 多分区亮度融合 zones(M5b) → 粉嫩肤色 skin(M5b)
  → 转回线性 f32
  → 色域软裁切（降 C 不截断）
  → 编码 sRGB u8
  → [细节: 降噪/锐化/柔光]                         (M5, 可选)
  → [高级: 频谱磨皮 / 金字塔融合]                  (M6, 可选, 人脸/多版本)
  → 写出 JPG/TIFF（保留 EXIF / ICC）
```

**原则**：所有参数为**声明式、与顺序无关**；每次渲染都从原图重算（agx-photo 模式），
undo = 回退参数集，命令盘 = 增改参数。

---

## 4. 架构（Rust workspace）

```
retouch-rs/                  (workspace 根)
├── crates/
│   ├── retouch-core/        (lib: 管线 + 所有调整模块 + 几何 + 预设解析 + 度量)
│   └── retouch-cli/         (bin: clap CLI，100% 对齐 darktable-cli 接口)
├── ui/retouch-ui/          (egui/eframe 桌面端，M4；ACR 面板 + Cmd+K 命令盘)
├── presets/                 (TOML 预设库，参数名对齐 darktable-cli)
├── tests/                   (基准图 + roundtrip / 度量断言)
├── DESIGN.md
└── MODULES_REFERENCE.md
```

- 模块化：每个调整项 = 纯函数 `fn adjust(oklch: &mut Oklch<f32>, p: &Param)`。
- 几何变换 = 独立 `geometry` 模块（在管线入口前调用）。
- 并行：rayon 按行分块（像素阶段）。
- 无 `unsafe`、无外部 C 依赖；`retouch_core` 可被 CLI 与 egui GUI 共用。

---

## 5. 参数模型 / 预设 / CLI

**TOML 预设**（order-independent，声明式，参数名对齐 darktable 模块）：
```toml
[exposure]              # darktable: exposure
ev = 0.3
[tone_map]              # darktable: filmicrgb / sigmoid（我们的肩部压缩）
mode = "none"           # none | agx | filmic
[defake]                # darktable: colorzones（chroma 控制，我们的去假色）
enabled = true
chroma_decay = 0.1
fix_sky = true
protect_skin = true
[grade]                 # darktable: shadhi + levels + toneequal 微调节
brightness_lift = 0.06
contrast = 0.15
dehaze = 0.25
shadow_lift = 0.15
deep_shadow_lift = 0.15
[white_balance]         # darktable: channelmixerrgb（色彩校准）
temperature = 0.0
tint = 0.0
[color]                 # darktable: vibrance / colisa / colorequal
saturation = 1.0
vibrance = 0.0
hue_rotate = 0.0
split_shadow = 0.0
split_highlight = 0.0
[hsl.blue]              # darktable: colorzones HSL 分区（band: red|orange|yellow|green|aqua|blue|purple|magenta）
hue = 0.0
sat = 1.4
light = 1.0
```
> 预设为**基底**：CLI 旗标（均为 `Option`）在预设之上逐个覆盖；既不给预设也不给旗标 → 恒等往返。
> 示例见 `presets/factory.toml`（= `photo_default`）与 `presets/summer_warm.toml`。

**出厂默认微调档（v0.3 锁定，写入 `Adjustments::photo_default()`）**：
- 去假色常开：`chroma_decay=0.1` + `fix_sky` + `protect_skin` + 色域软裁切
- 轻量调级（OKLCH L 通道，绝不动 hue/chroma → 零偏色）：
  `brightness_lift=0.06`（柔和提亮）、`contrast=0.15`、`dehaze=0.25`（层次感，均值轴不变亮）、
  `shadow_lift=0.15`（Shadows 救中暗调）、`deep_shadow_lift=0.15`（Blacks 救最暗处，≈0.25 效果）
- 用户审美："质感优先、不全局提亮、干净即可"；暗图救暗部必然带整体亮度到 ~68（物理上互斥于更小亮度）
- 默认 `tone_map=None`（不套 AgX，避免 ev=0 时提亮毁细节）；推曝光时 CLI 自动套 AgX 护高光
- 出厂档的 `white_balance` 与 `color` 均为恒等（调色彩风格是用户主动创意选项，不偷偷加）

**CLI**（二进制名 `retouch-rs`，位置参数 `<INPUT> <OUTPUT>` 在前；预设为基底、旗标覆盖）：
```
retouch-rs render in.jpg out.jpg --preset presets/factory.toml
retouch-rs render in.jpg out.jpg --exposure 0.3 --tone-map agx --defake
# M2b 调色彩风格旗标：
retouch-rs render in.jpg out.jpg --temp 0.3 --tint -0.1          # 暖调 / 偏品
retouch-rs render in.jpg out.jpg --saturation 1.25 --vibrance 0.3 # 增饱和 + 活力
retouch-rs render in.jpg out.jpg --hue-rotate 12                 # 全局色相旋转
retouch-rs render in.jpg out.jpg --split-shadow 40 --split-highlight -20 # 阴影加暖/高光加冷
# M2c 分区色相 HSL（可重复，band: red|orange|yellow|green|aqua|blue|purple|magenta）
retouch-rs render in.jpg out.jpg --hsl blue:0,1.6,1.0 --hsl green:0,1.25,1.0 --hsl red:18,1.0,1.0
# M5b 非纯线性感知控制（胶片感 S 曲线 / 光比融合 / 多分区亮度融合 / 粉嫩肤色）
retouch-rs render in.jpg out.jpg --film-curve 0.22 --light-ratio 0.35
retouch-rs render in.jpg out.jpg --zone-shadows 0.2 --zone-highlights=-0.1
retouch-rs render in.jpg out.jpg --skin --skin-strength 0.5 --skin-hue 25 --skin-chroma 0.10 --skin-light 0.01
# M4b 几何预处理（透视纠正 → 旋转 → 翻转 → 裁剪，任意顺序组合）
retouch-rs render in.jpg out.jpg --crop 0.1,0.1,0.8,0.8
retouch-rs render in.jpg out.jpg --rotate 90 --flip-h
retouch-rs render in.jpg out.jpg --persp-v -0.2 --persp-h 0.1
# M5 细节后处理（降噪 / 锐化 / 高光柔光）
retouch-rs render in.jpg out.jpg --denoise 0.4 --sharpen 0.4 --diffuse 0.3
# 原 M6 高级修图（频谱磨皮 / 金字塔融合）
retouch-rs render in.jpg out.jpg --freqsep --freqsep-strength 0.5 --freqsep-texture 0.8 --freqsep-smooth 0.3 --freqsep-feather 0.5
retouch-rs render in.jpg out.jpg --pyramid --pyramid-strength 0.4 --pyramid-scale 1.2
# 一条龙（日常「质感」组合）：粉嫩 + 轻降噪 + 锐化 + 柔光 + 磨皮 + 金字塔
retouch-rs render in.jpg out.jpg --skin --denoise 0.4 --sharpen 0.4 --diffuse 0.3 --freqsep --freqsep-strength 0.5 --pyramid --pyramid-strength 0.4
# 把当前解析后的参数导出为 TOML（便于 retouch_app 迁移 / 标准化）
retouch-rs dump --preset presets/summer_warm.toml out.toml
```
> **注意**：负值参数必须用 `--flag=value` 形式（如 `--zone-highlights=-0.1`），否则 clap 会把 `-0.1` 当成新旗标。
> 所有调整旗标都是 `Option`：给了就覆盖预设对应项，没给就沿用预设（或恒等默认）。
> 推曝光（`exposure > 0`）且未显式选影调时自动套 AgX 护高光（与 M1/M2a 行为一致）。

**UI（M4，二进制 `retouch-rs-gui`）**：类 ACR 滑块面板（每一项=一个滑块，分组：曝光/影调、去假色、调明暗、白平衡、调色彩风格、分区 HSL）+ Cmd+K 命令盘。
- 载入图片后**实时预览**（源图先降采样到长边 ≤1400px 再渲染，拖动滑块即重渲）。
- 快捷键：`Cmd+O` 打开、`Cmd+S` 保存全分辨率、`Cmd+K` 命令盘、`Cmd+P` 载入预设。
- 命令盘指令：`open` `save` `preset` `factory` `reset` `dump` `exposure <v>` `contrast <v>` `brightness <v>` `dehaze <v>` `shadow <v>` `deepshadow <v>` `wb <v>` `tint <v>` `sat <v>` `vibrance <v>` `hue <v>` `splitshadow <v>` `splithighlight <v>` `hsl <band> <h> <s> <l>` `film <v>` `lightratio <v>` `zone <i> <v>` `skin <on>` `auto` `autoexposure` `autowb` `autodehaze` `rotate <deg>` `flip <h|v>` `crop <x> <y> <w> <h>` `denoise <v>` `sharpen <v>` `diffuse <v>` `freqsep <strength>` `pyramid <strength>` `help`。
- **智能一键（M7，✅ 已实现）**：侧栏"智能一键"分区提供 `自动曝光`（降采样算 mean_L 反推 EV 补偿）/ `自动白平衡`（灰点估计 temp/tint）/ `一键粉嫩`（启用 skin 健康粉嫩默认）/ `智能去雾`（按雾浓度算 dehaze）/ `全智能`（自动曝光+白平衡+去雾+粉嫩一键铺开）。所有滑块走感知映射 + 实时"实际 {}"读数，比传统软件更跟手。
- **几何 / 细节 / 高级** 三折叠区均已落地（M4b / M5 / 原 M6，✅ 已实现）：侧栏含「几何预处理」（旋转/透视纵/透视横 slider + 水平/垂直翻转 checkbox + 4 个 crop slider + 清除裁剪）、「细节后处理」（降噪/锐化/柔光）、「高级修图」（磨皮启用 + 强度/纹理保留/平滑度/蒙版羽化 + 金字塔启用 + 强度/细节倍率）。命令盘对应 `rotate/flip/crop/denoise/sharpen/diffuse/freqsep/pyramid`。

---

## 6. 依赖（crate 选型，均已调研）

| 用途 | crate | 说明 |
|---|---|---|
| OKLCH/OKLab 转换 | `palette` 0.7 | 已验证原生 OKLCH |
| 影调曲线 AgX/Filmic/ACES | `zentone` | SIMD 加速，safe Rust |
| 色域软裁切 | 自研（palette + 二分降 C） | OKLCH 降 C 不截断，零额外依赖（esoc-color 不存在，已验证 404） |
| JPG/TIFF 编解码 | `image` 0.25 | 已验证 |
| 几何变换（旋转/透视/重采样） | `image` + 自研双线性/双三次 | 透视纠正在 core 内实现 homography |
| CLI | `clap` | 派生宏 |
| 预设序列化 | `serde` + `toml` | TOML 声明式 |
| 并行 | `rayon` | 按行分块 |
| 原生文件对话框 | `rfd` 0.15 | M4 GUI 打开/保存/选预设 |
| UI | `egui/eframe` 0.29 | M4，纯 Rust 原生 UI，单二进制无 webview/DLL；rust-master-workflow 技能支撑 |

---

## 7. 双系统策略（Windows + macOS）

- 核心库纯 Rust + 标准 crate，**交叉 / 原生编译均无系统依赖**。
- **macOS**：`cargo build --release` + `mac-tauri-packaging` 出 `.app` / `.dmg`。
- **Windows**：同源码 `cargo build --release` 出 `.exe`；**无 LibRaw / DLL**，egui 单文件二进制（`cargo xwin` 交叉编译）。
- 建议 GitHub Actions 双平台矩阵做 release 产物。

---

## 8. 里程碑（v0.2 重排）

- ✅ **M0** 脚手架 + OKLCH roundtrip 零偏色（已完成并验证：单测通过 + 真实 4.6MB JPG 端到端 1.77s，PNG 无损 max diff=3）
- ✅ **M1** 曝光 + AgX/Filmic 影调映射（线性乘子 + zentone 肩部压缩）
- ✅ **M2a** 去假色（chroma decay + 天空/肤色约束 + 色域软裁切，**默认常开**）+ 调明暗(全局/对比/去雾/暗部/最暗) + 剪影纯黑保护
- ✅ **M2b** 调色彩风格：白平衡(temp/tint 线性增益) + 饱和度 + 活力 + 全局色相旋转 + 分离色调，CLI 旗标 + 12 单测全过 + 真实图泛化验证（16 张全光比无削波、hueMed 1.41°）
- ✅ **M2c** 分区色相 HSL：ACR 式 8 色相带（红/橙/黄/绿/青/蓝/紫/品红），每带独立 H/S/L；三角最近邻混权无缝、恒等短路保 M0 roundtrip；band 中心用 palette 实测 OKLCH 角对齐；CLI `--hsl <band>:<h>,<s>,<l>` 可重复；3 单测全过（蓝带增饱和无偏色 / 红带仅转红不转绿 / 恒等无偏）
- ✅ **M3** TOML 预设 + CLI（参数名对齐 darktable 模块）：`presets/factory.toml`(=photo_default)、`presets/summer_warm.toml`；CLI 二进制 `retouch-rs` 支持 `--preset`（基底）+ `Option` 旗标覆盖 + `dump` 子命令导出 TOML；`retouch_core::preset` 模块（serde+toml，暗调对齐）；2 单测（预设往返无损 / 恒等预设）；M0 roundtrip 不受影响（Adjustments 加 PartialEq）
- ✅ **M4** egui/eframe 桌面 UI（二进制 `retouch-rs-gui`）：ACR 滑块面板（曝光/影调·去假色·调明暗·白平衡·调色彩风格·分区 HSL 全字段）+ Cmd+K 命令盘（open/save/preset/factory/reset/dump + 各参数指令 + hsl <band>）+ 实时降采样预览 + Cmd+O/S/P 快捷键。**M4b 几何/构图（✅ 已实现）**：侧栏「几何预处理」折叠区（旋转/透视纵/透视横 slider + 水平/垂直翻转 checkbox + 4 个 crop slider + 清除裁剪按钮），命令盘 `rotate/flip/crop` 指令均已落地。
- ✅ **M5** 非纯线性感知修图（用户指令"一口气推进 M5–M7，智能符合人眼、减少纯线性生硬感"）+ **M5「细节」落地**：
  - **M5a 感知滑块映射** `perceptual.rs`：`CurveKind{Linear/SoftKnee/LogSat}` + 中心中和 + `tanh` 加权衰减 + 饱和度对数；3 单测（线性过中心/SoftKnee 中性且两端缓/LogSat 中心恒等）全过。
  - **M5b 管线新模块** `pipeline.rs`：粉嫩肤色 `SkinTone`（OKLCH 肤色概率遮罩 → 拉向健康粉嫩目标 + 非肤色保护）、多分区亮度融合 `ZoneGrade`（4 区高斯平滑无硬边）、胶片感 S 曲线 `film_curve`（正弦 toe+shoulder）、光比融合 `light_ratio`（多膝均值保持）；5 单测（skin 粉嫩且留蓝/skin 关=恒等/zone 提暗部/film 加反差/light_ratio 融合）全过。
  - **M5c 参数注册表** `params.rs`：`Field` 枚举 + `ParamSpec` + `registry()`，GUI/CLI 共用元数据，滑块曲线/范围/单位集中定义；`Clone` 已实现。
  - **M5 细节后处理（✅ 已实现）** `detail.rs`：降噪（亮度域 bilateral）/ 锐化（阈值保护 USM）/ 柔光（高光蒙版 glow），全部 `is_identity()` 零重采样短路；4 单测（identity_is_noop / denoise_reduces_noise_energy / sharpen_increases_edge_contrast / glow_only_softens_highlights_keeps_darks）全过；CLI `--denoise/--sharpen/--diffuse`、GUI「细节后处理」折叠区、参数注册表 3 项（SoftKnee 曲线）全接入。
- ✅ **M6** CLI 全量旗标（M5 模块的命令行入口）：`--film-curve --light-ratio --skin --skin-strength --skin-hue --skin-chroma --skin-light --skin-smooth --skin-protect --zone-shadows --zone-dark-mid --zone-light-mid --zone-highlights`；`resolve()` 启用 `--skin` 时回退到 `SkinTone::pink()` 健康粉嫩默认（避免勾选后目标值归零）；`dump` 已验证新字段正确落盘 TOML。**原 M6「高级修图」顺延部分（✅ 已实现）**：`advanced.rs` 频谱磨皮 `FreqSepSkin`（肤色蒙版限定的频率分离磨皮）+ 金字塔融合 `PyramidFusion`（4 级拉普拉斯式多层细节融合），均严格恒等短路；4 单测（advanced_identity_is_noop / freqsep_smooths_skin_pixels / pyramid_zero_is_identity / pyramid_changes_image_when_strength_nonzero）全过；CLI `--freqsep/--freqsep-*/--pyramid/--pyramid-*`、GUI「高级修图」折叠区、参数注册表 6 项全接入。
- ✅ **M7** 智能 GUI（egui，`retouch-rs-gui`）：`param_slider` 感知滑块 + 实时"实际 {}"读数；新增「粉嫩肤色 / 多分区亮度融合 / 智能一键 / 几何预处理 / 细节后处理 / 高级修图」分区；智能一键 = 自动曝光 / 自动白平衡 / 一键粉嫩 / 智能去雾 / 全智能；命令盘加 `skin/auto/autoexposure/autowb/autodehaze/film/lightratio/zone/rotate/flip/crop/denoise/sharpen/diffuse/freqsep/pyramid`；中文 + CJK 字体 + 实时渲染已修。
  - **暗角（vignette）按用户明确要求已取消**，全管线不再实现（"m 到 m7 暗角就不要了"）。
  - **M4b（几何）/ M5（细节）/ 原 M6（高级修图）三模块已全部代码落地 + 编译 + 单测 + CLI 端到端跑通**，本次会话收尾完成（release 构建通过、verify 往返 PASS、38 单测全过）。

---

## 9. 参考来源

- 自有技能 **`darktable-cli-retouch`**：19+ 模块反推知识（作为 OKLCH 模块功能对照，见 `MODULES_REFERENCE.md`）。
  - `skin_retouch.py` → M6 频谱磨皮算法源
  - `mask_blend.py` → M6 金字塔融合算法源
- Rust crate：`palette`(OKLCH)、`zentone`(AgX/Filmic)、`image`(JPG/TIFF/几何)；gamut 软裁切自研。
- 架构参考：**`agx-photo`**（声明式 preset + 始终从原图重渲）。

---

## 10. v0.2 评审结论（已确认）

1. 功能分类 = 四大类 + **几何构图**（裁剪/旋转/透视） + **高级修图**（频谱磨皮/金字塔融合）。
2. 去假色**默认常开**（chroma_decay≈0.1 + sky/skin + gamut softclip），UI 可关。
3. 细节类（降噪/锐化/柔光）**放 M3 之后**（M5）。
4. CLI/预设 **100% 对齐 darktable-cli 参数名**，便于 M7 零改动接入 retouch_app。
