## 初色 Retouch v0.6.9（审计加固 + 污点修复 UX 重做收口）

**一句话**：把 v0.6.7 起的污点修复 UX 重做正式收口发版，并对 Mac / Windows 两端做了一次完整代码审计，修了 4 个真实隐患（预览与导出修复范围不一致、内容感知修复卡死、批量导出丢当前图改动、Windows 代理探测误 spawn），外加 3 个回归测试守住这些问题不再回潮。

### 审计修了什么（v0.6.9 重点）

#### 1. 修复范围「所见即所得」（半径随图缩放）
- 旧实现把修复半径写死 60px。预览短边约 900px 时够用，但导出全分辨率短边可达 4000px+，修复范围被截断——**预览看着修好了，导出却没修干净**。
- 改成按图像短边 5% 动态缩放并封顶 200px（`max_radius_px`），下限仍 60px 不退化。预览与导出走同一套半径逻辑，结果一致。
- 大半径按等成本下调迭代次数，避免 Poisson 成本爆炸。
- 回归测试：`radius_cap_scales_with_short_side`、`big_image_large_brush_is_not_truncated`。

#### 2. 内容感知修复不再卡死（空间聚簇）
- 旧实现把「所有污点笔触的并集 bbox」整块送进 PatchMatch，自动检测出上百个分散灰尘时，并集 bbox 被撑成接近整图，直接卡死。
- 改成按笔触中心距做**空间聚簇（并查集）**：连成一片的细线归一簇保连续，分散的污点各自独立成簇，每簇只算自己的小 bbox。
- 回归测试：`patchmatch_many_strokes_uses_per_stroke_path`（12 处分散缺陷分成 12 簇且全部修复）、`patchmatch_fills_a_thin_line`（连续细线不断裂）。

#### 3. 批量导出不再丢当前图改动
- 旧实现「编辑当前图直接点批量导出」时，工作副本的状态（影调 / 污点 / 标题）还没落回相册 slot，导出读的是上次切图时的旧参数，当前这张的导出结果不对。
- 抽出 `sync_active_slot`，在「切换图片」和「批量导出」前先把当前工作副本落回活跃 slot。

#### 4. Windows 代理探测不再误 spawn shell
- `detect_proxy` 在 Windows 上也会试图 spawn `/bin/zsh` / `/bin/bash`，必然失败还触发杀软进程创建告警。
- 给该探测循环加 `#[cfg(unix)]`，Windows 直接走系统代理 env，不碰 shell。

#### 5. 顺手清理 2 处 clippy
- `DetectParams` 构造改用 `..Default::default()`；`is_none_or` 替代 `map_or(true, ...)`。

### 包含在本次发版里的污点修复 UX 重做（v0.6.7 起累积）
- 自动检测不再翻倍：每笔污点带 `is_auto` 标记，检测前先清掉上次自动笔触，连点 100 次也只留最新结果。
- 「检测=选区、应用=修复」贴合 PS 心智：默认只显示红圈选区，点「✅ 应用修复」才在预览里真正愈合（变绿圈）。
- 每笔可各自选档位（传统 / 自然 / 精修 / 内容感知）；灵敏度滑块抑制天空等平滑背景的误检。
- 标注可隐藏、检测圈收紧；撤销一笔 / 清空 / 笔刷大小 / 前后对比 / 相册多图 / 全分辨率导出（MozJPEG 4:4:4）全部保留。

### 本次补强：商业软件标准 4 项（用户实测后拍板）

> 经真实使用对比商业修图软件，补齐 4 个体验短板（之前被标记为「待拍板」，本版全部落地）：

1. **关闭窗口有「未保存更改」提示** —— 有未保存改动时关窗会弹确认框（保存并退出 / 放弃并退出 / 取消），不再 silently 丢失工作。
2. **打开/拖入支持 WebP / HEIC** —— 之前只收 jpg/jpeg/png/tif；现在 WebP 原生解码、HEIC/HEIF 经 macOS `sips` 转临时 JPEG 回退（零 C 依赖、不碰 Windows 红线）。
3. **一键中性化不再削弱后续手动调整** —— 旧实现把风险图整体结果按 `mix<1` 与原图混合「防过曝」，副作用是后续拖曝光/对比被 `(1-mix)` 整体打折、像「调了没反应」。改为 `mix` 恒为 1.0，过曝防护交由 `run_auto` 的伪影护栏闭环负责；并加回归测试锁死该不变量。
4. **自动保存 / 崩溃恢复** —— 每 5 秒节流写 `~/.retouch/autosave.json`（仅未保存且有图时）；启动时若源文件仍在，弹「恢复 / 不恢复」对话框，崩了也能救回进度。

> 以上均经 `cargo test -p retouch-core`（81 passed，含新增 mix 不变量测试）与 `cargo check`（skin / 默认双版）验证。

### 命令行（CLI）引擎

初色**原生支持后台 CLI 调用**（`retouch-cli` crate，二进制名 `retouch-rs`）：
- `render` 跑 OKLCH 管线、`analyze --json` 量化 OKLCH 指标供 AI 读取、`auto` 一键调色、`name --key <QWEN>` 用 Qwen 起名、`schema --json` 导参数 id、`dump` 固化 preset、`verify` 自检。
- 全部参数可 `--preset` 作底 + 逐个 `--xxx` 覆盖，或 `--params '{"id":val}'` 按 id 设；适合无头服务器 / 自动化流水线 / Agent 调用。
- 详见 `README.md` 的「命令行（CLI）调用」与 `crates/retouch-cli`。

### 下载

| 平台 | 文件 | 说明 |
|---|---|---|
| macOS 11+（分享版） | `初色-0.6.9-macOS.zip` | 右键打开即用 |
| Windows 10/11 64 位（**分享版**） | `初色-0.6.9-windows-分享版.zip` | 无 Key，免 VC++ 运行库，双击即用 |
| Windows 10/11 64 位（**自用版**） | `初色-0.6.9-windows-自用版.zip` | 内置 Qwen Key，AI 追色/命名可用；Key 仅存本地 |

> 每个压缩包内都含「源码」快照（用 `git archive`，自动排除 Key/模型/构建产物），可单独编译研究。
> 自用版与分享版功能完全一致，区别仅在是否附带 Qwen Key；源码完全开源、不含任何 Key。

---

#### English

**Retouch v0.6.9** ships the spot-heal UX rework (since v0.6.7) and a full Mac/Windows code audit that fixed 4 real issues.

**Audit fixes (v0.6.9)**
1. **WYSIWYG heal radius** — radius was hard-coded at 60px, truncating the heal on full-res export (short side 4000px+). Now scales with 5% of the short side, capped at 200px; preview and export share one radius path. Regressions: `radius_cap_scales_with_short_side`, `big_image_large_brush_is_not_truncated`.
2. **Content-aware no longer hangs** — all spots' union bbox was fed to PatchMatch; dozens of scattered specks blew it up to near-full-image. Now clustered by proximity (union-find): connected strokes stay one cluster, scattered specks get independent small bboxes. Regressions: `patchmatch_many_strokes_uses_per_stroke_path`, `patchmatch_fills_a_thin_line`.
3. **Batch export keeps current image edits** — `sync_active_slot` now flushes the working copy (tone / spots / title) back to the active album slot before switching images and before batch export.
4. **Windows proxy probe** — `detect_proxy`'s shell spawn (`/bin/zsh`, `/bin/bash`) is now `#[cfg(unix)]`; Windows reads system proxy env only.
5. Two clippy cleanups (`..Default::default()`, `is_none_or`).

**Spot-heal UX (since v0.6.7)**: no auto-detect doubling (`is_auto` marker), PS-style "select then apply" (red selection → green healed), per-stroke heal mode, sensitivity slider, hideable markers, full-resolution MozJPEG 4:4:4 export.

**Downloads**
- macOS 11+ (share): `初色-0.6.9-macOS.zip`
- Windows 10/11 64-bit (**share, no Key**): `初色-0.6.9-windows-分享版.zip`
- Windows 10/11 64-bit (**self-use, with Qwen Key**): `初色-0.6.9-windows-自用版.zip`

Each zip bundles a `源码/` snapshot (via `git archive`, excludes Key/models/build artifacts). Self-use and share builds are functionally identical; the only difference is whether the Qwen Key is bundled. Source is fully open and contains no Key.

**CLI engine**: `retouch-rs` (from `retouch-cli`) supports headless invocation — `render`, `analyze --json`, `auto`, `name --key`, `schema --json`, `dump`, `verify`. See `README.md`.

---

### 许可证 / License
MIT。版权所有 © 2026 星TAP。
