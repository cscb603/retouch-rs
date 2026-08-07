# PatchMatch 内容感知移除模块白皮书（Plan A 实施版）

> 状态：已实施（Plan A，代码合并完成，待发布）  
> 目标项目：retouch-rs / 初色  
> 作者：小 d  
> 最后更新：2026-08-06

---

## 0. 决策摘要（务必先读）

- **方案 A 已选定并实施**：PatchMatch 定位为**「细线 / 电线 / 杆 / 细缝去除」专用档**，不是通用大块物体移除升级。
- **核心验证结论**（实测台 `/tmp/pm_bench`）：朴素 PatchMatch 仅在**细结构去除**上稳定优于 Telea（细电线场景 **+2.5dB PSNR**）；**大块物体/结构化背景**场景它**中性甚至偏弱**，该场景仍应推荐 Poisson（用户手动引导）。因此不做"通用 Content-Aware Fill"夸大宣发。
- **实现关键坑（已填）**：逐笔独立小圆盘填充会把连续线条当成结构「续接」回洞里（失败）。改为**所有笔画并集 mask 一次性填充**，与验证台一致，彻底解决。

---

## 1. 目标与边界（做什么 / 不做什么）

### 1.1 目标（Plan A 现实口径）
新增 **HealMode::PatchMatch**，提供类 Photoshop Content-Aware Fill 的**内容感知移除**能力，但**聚焦细结构**：
- 用户用污点画笔沿电线、杆、细缝、发丝状杂物涂抹；
- 算法从邻近区域采样相似纹理块，自动填充被移除区域；
- 完全本地运行、零模型权重、纯 Rust 实现。

### 1.2 不做（边界）
- ❌ 不引入任何神经网络模型或权重文件（拒绝 LaMa / SD / Firefly 路线）；
- ❌ 不引入 OpenCV / PyTorch / ONNX Runtime 等重型外部依赖；
- ❌ 不替换现有 Telea / FreqSep / Poisson，仅新增档位；
- ❌ 不保证大块物体（如整人、整车）自然结果——此类场景 UI 文案明确引导用 Poisson；
- ❌ 不处理视频、批量自动检测物体、人脸五官级修复。

---

## 2. 验证结论（决定方案边界的事实依据）

验证台 `/tmp/pm_bench/src/main.rs` 同图、同 mask、同指标对比 PatchMatch（Criminisi 边界环填充）与现有 Telea（`inpaint` crate）：

| 场景 | 背景 | 待移除物 | PatchMatch PSNR | Telea PSNR | 结论 |
|---|---|---|---|---|---|
| 渐变 + 竖电线 | 平滑渐变 | 4px 电线 | 高（续接被抑制） | 低 | **PatchMatch 胜 +2.5dB** |
| 渐变 + 方块 | 平滑渐变 | 40×40 块 | 中性 | 中性 | 持平 |
| 砖墙 + 方块 | 周期纹理 | 40×40 块 | 中性偏弱 | 中性 | 大块 PatchMatch 不占优 |

- **速度**：256×256 单线程 13–19ms（含边界环填充），远低于 2s 验收线，preview=2 迭代更流畅。
- **关键认知**：PatchMatch 的优势在于**纹理连续区的小尺度修复**，不是大洞重建。大洞它会模糊/续接错误。这决定了 Plan A 的聚焦范围。

---

## 3. 接口设计（实际落地，非设想）

### 3.1 枚举扩展（`crates/retouch-core/src/spot.rs`）
```rust
pub enum HealMode {
    Telea,      // 默认无此标注？见下方：Poisson 才是当前默认
    FreqSep,
    #[default]
    Poisson,    // 精修档默认
    PatchMatch, // 【新增】内容感知移除（细线/电线/杆/细缝）
}
```
- `HealMode` **未派生 Serialize**，因此新增变体对 `SpotFix` 预设/历史文件**零兼容性风险**。
- `SpotFix` 持久化字段 `mode` 不序列化 → 老预设读回时走 `#[default]` Poisson，无断裂。

### 3.2 调用路径（实际）
1. UI：`ToolMode::Spot` → 算法档位按钮组第 4 项「内容感知」（`ui/retouch-ui/src/main.rs`，`ui.columns(4, …)`）→ 涂抹画布 → `self.heal_mode` 写入 `SpotFix.mode`；
2. 核心分发：`heal.rs` `match spot.mode` 新增 `HealMode::PatchMatch => heal_patchmatch(img, spot, preview)`；
3. CLI / Agent：沿用现有 `SpotFix` schema，`heal_mode: "patchmatch"`（无需新字段）。

### 3.3 函数签名（实际）
```rust
// crates/retouch-core/src/heal.rs
pub fn heal_patchmatch(img: &RgbImage, spot: &SpotFix, preview: bool) -> RgbImage;
// 内部：fn patchmatch_fill(img: &mut RgbImage, mask: &GrayImage, patch_r: i32, inner_iters: u32) -> bool
```
- **无独立 `PatchMatchParams` 结构**（白皮书初版设想的 `patch_radius/iterations/random_search_ratio` 未采纳）：参数全部内部派生，避免 UI/CLI/agent 三处维护。
  - `patch_r = (max(bw,bh) / 32).clamp(3, 8)`（随并集尺寸缩放封顶，验证台 4 在 256² 足够）；
  - `inner_iters = preview ? 2 : 3`；
  - 随机搜索比例固定 0.5（xorshift64 确定性 RNG，结果可复现）。

### 3.4 输入输出
- 输入：`img: &RgbImage`、`spot: &SpotFix`（含若干 `SpotStroke{cx,cy,r_norm}`）；
- 输出：`RgbImage`（同尺寸，并集 mask 区域被填充，其余像素原样）。

---

## 4. 算法（Criminisi 边界环填充 + 并集 mask 一次性填充）

### 4.1 为什么是「并集 mask 一次性填充」而非「逐笔独立圆盘」
- ❌ **错误初版**：每笔刷取自己的小圆盘 bbox，独立跑 PatchMatch。
  - 现象：连续线条（电线）在每一笔的圆盘之外仍以「已知像素」存在，PatchMatch 把线当成结构**续接**回洞里（诊断看到 `[129,117,105]` 类混合色 = 线色与背景的平均）。单测 `patchmatch_fills_a_thin_line` 因此失败（dist=480=原线色未动）。
- ✅ **正确方案（已实现）**：把 spot 内所有笔画的圆形洞**合并成一张并集 mask**，在并集 bbox（含 `pad=max_r*3` 上下文）上**一次性**跑 PatchMatch。
  - 连续线整条都在 mask 内 → mask 外无已知线段 → PatchMatch 无法续接 → 填出背景。**与验证台一致**，单测复测通过。

### 4.2 算法流程（`patchmatch_fill`）
1. 初始化工作缓冲 `cur`(f32 RGB) + `known`(bool)，mask 非零 → `known=false`；
2. 随机初始化 NNF 偏移（xorshift64，64 次尝试找已知源块，确定性可复现）；
3. 每轮：
   - **inner_iters 次 PatchMatch**：前向/后向交替扫描，每洞像素 = min(自身偏移, 传播邻域偏移, 随机搜索偏移) 的 SSD；
   - **提交边界环**：仅填充「补丁内含有已知像素」的洞（= 当前边界），从边界向内逐环填充；重叠源补丁取平均；
   - 无新增填充 或 rounds>80 → 终止。
4. SSD 仅比较**已知像素**，空洞不参与（避免垃圾值污染匹配）。

### 4.3 Telea 兜底（绝不崩、绝不留黑洞）
- `patchmatch_fill` 返回 `all_filled`；若 rounds 用尽仍有残留洞 → 调 `inpaint_rgb(&sub, &submask, telea_r)`（telea_r=patch_r+1）兜底；
- 极端 bbox 越界 → 直接 `return img.clone()`，安全跳过该 spot。

---

## 5. 四档分工（UI 文案口径，已写入 `main.rs`）

| 模式 | 最佳场景 | 技术本质 | 速度 | UI 按钮文案 |
|---|---|---|---|---|
| **Telea** | 小点状污点、灰尘、痘痘 | 快速行进 + 加权平均 | 最快 | 传统 |
| **FreqSep** | 需保留皮肤/织物纹理 | 高低频分离分别填充 | 中 | 自然 |
| **Poisson** | 边缘渐变融合、精修接缝（大块物体引导） | 泊松方程引导向量场 | 中 | 精修（默认） |
| **PatchMatch** | 电线/杆/细缝/发丝状细结构 | 样本块最近邻 + 纹理合成 | 较慢 | 内容感知 |

**UI 引导文案（已落地）**：「内容感知移除（PatchMatch）：细线/电线/杆/细缝去除更自然，纹理连续；大块物体仍建议精修」。

---

## 6. 实现状态（截至本稿）

| 项 | 状态 | 位置 |
|---|---|---|
| `HealMode::PatchMatch` 枚举 | ✅ 已加 | `spot.rs` |
| `heal.rs` 分发 | ✅ 已加 line 29 | `heal.rs` |
| `heal_patchmatch` 并集填充 | ✅ 已加 | `heal.rs` ~492 |
| `patchmatch_fill` 核心算法 | ✅ 已加 | `heal.rs` ~580 |
| UI 第 4 档按钮 | ✅ 已加 | `ui/retouch-ui/src/main.rs` ~2138 |
| 单元测试 | ✅ 59 通过（含 `patchmatch_fills_a_thin_line` 复测通过） | `heal.rs` tests |
| `cargo build -p retouch-ui` | ✅ 通过 | — |
| Windows 交叉编译检查 | ⏳ 本次验证中 | `cargo xwin check --target x86_64-pc-windows-msvc` |
| 白皮书更新 | ✅ 本稿 | — |
| git commit | ⏳ 待执行 | — |

**未引入任何新 crate 依赖**，未下载任何模型权重。

---

## 7. 计划任务（细致排期，零坑清单）

### 7.1 已完成的硬编码收尾（本次会话）
- [x] 移除全部调试 `eprintln!`（`[pmf] start/exit/round`、`[pmh] sub_at_line`、`[diag]`）；
- [x] 删除废弃的 `patchmatch_heal`（逐笔圆盘版），消除 dead_code 警告；
- [x] 诊断误导测试 `patchmatch_fills_a_thin_line` 改为走真实 `heal_image` 并集路径；
- [x] `cargo fmt` 统一格式；
- [x] `cargo test -p retouch-core --lib` 59 通过，0 新增 warning（heal.rs 仅余历史 `iters`/`gt` 于 `patchmatch_source_center`，非本次代码）。

### 7.2 待执行（发布前）
- [ ] **Windows 交叉编译验证**：`cargo xwin check/build --target x86_64-pc-windows-msvc -p retouch-ui` 全绿（CRT 静态链接已配 `.cargo/config.toml` 的 `+crt-static`）；若缺 MSVC SDK 由 xwin 首次下载需网络。
- [ ] **clippy 全量**：`cargo clippy -p retouch-core -p retouch-ui` 确认无新增 lint（现有 `unused_parens` 等历史 warning 不计入本次）。
- [ ] **真实图像 before/after 样例**：至少 1 张含电线的实拍图，跑 PatchMatch 档，存 `/tmp` 或 `generated-images` 供用户肉眼验收（细线场景）。
- [ ] **git commit**：commit message 含「feat(heal): add HealMode::PatchMatch (Plan A, union-mask fill)」；推前先 `git fetch` 确认分支状态，避免 non-fast-forward 强推。
- [ ] **打包验证（可选但推荐）**：Mac `build_mac_app.sh` + Win `cargo xwin build --release` 出包，确认 PatchMatch 档在打包产物里可选、可撤销。

### 7.3 已知限制（写入发布说明，避免用户误用）
- 大块物体（整人/整车/大水域）去除不保证自然 → 引导用 Poisson（可多次手动引导）；
- 极细密网格 / 强透视结构可能续接错误 → 同 Telea 局限；
- preview 迭代=2、导出=3，导出更稳但更慢（仍 <<2s 于常规图）。

---

## 8. 验收标准（更新版）

- [x] `cargo test -p retouch-core --lib` 全绿（59 通过），细线单测复测通过；
- [x] `cargo build -p retouch-ui` 通过，无新增 warning（本次代码）；
- [ ] `cargo xwin check --target x86_64-pc-windows-msvc` 通过（待本次验证）；
- [x] 未引入新 crate 依赖；
- [x] 未生成/下载模型权重；
- [x] 现有 Telea / FreqSep / Poisson 行为零回归（仅新增分支，未改既有路径）；
- [x] UI 第 4 档可切换、可撤销（沿用 `SpotFix` 既有撤销链路）；
- [ ] 提供 ≥1 组真实电线 before/after 样例供肉眼验收。

---

## 9. 风险与备选（更新）

| 风险 | 影响 | 缓解（已落地/计划） |
|---|---|---|
| 连续线条被续接 | 高（初版实测失败） | ✅ 已用并集 mask 一次性填充彻底解决 |
| 大块物体效果不如 Poisson | 中 | UI 文案引导 + 不夸大宣发 |
| Windows CRT 链接 | 中 | ✅ `.cargo/config.toml` `+crt-static` 已配；xwin 验证中 |
| 速度在 4K 大并集 | 低 | 默认只处理圈选并集 bbox；rayon 并行 + rounds 封顶 80 |
| 与 HealMode 序列化兼容 | 无 | `HealMode` 未 Serialize，零风险 |

---

## 10. 评审结论

方案 A 已实施完毕并通过 Mac 侧编译/测试。剩余仅为 Windows 交叉编译验证与发布前样例/commit。
建议：**通过**，按 §7.2 收尾后发布。
