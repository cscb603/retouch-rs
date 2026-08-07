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

### 下载（仅分享版：两个平台）
- **macOS（分享版，Retouch）**：`Retouch-0.6.9-macOS.zip` — 发给朋友，右键打开即可
- **Windows 10 / 11（64 位，分享版）**：`Retouch-0.6.9-windows-x64.zip` — 免 VC++ 运行库，双击即用

> 自用版（初色，含个人 Qwen Key）仅供本地自用，不在本仓库分发。源码完全开源、不含任何 Key。

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

**Download (share builds only: 2 platforms)**
- macOS (share, Retouch): `Retouch-0.6.9-macOS.zip`
- Windows 10/11 (64-bit, share): `Retouch-0.6.9-windows-x64.zip`

> Self-use builds (初色, with personal Qwen Key) are for local use only and not distributed here. Source is fully open and contains no Key.

---

### 许可证 / License
MIT。版权所有 © 2026 星TAP。
