## 初色 Retouch v0.6.7（污点修复 UX 重做 + 翻倍 bug 修复）

**一句话**：v0.6.6 的「自动检测污点」有两个问题——连点会翻倍、且流程不清（以为必须保存才看得到效果）。v0.6.7 按 Photoshop「先选区、点应用才修」的心智重做，并修了翻倍。

### 修了什么

#### 1. 自动检测不再翻倍（bug 修复）
- 给每笔污点加 `is_auto` 标记；自动检测前先清掉**上一次**的自动笔触，再写入新的。手动笔触不受影响。
- 连点 100 次也只会保留最新一次的检测结果，不再累加到上千。
- 新增回归测试 `clear_auto_strokes_keeps_manual_ones`。

#### 2. 流程做清晰（贴合 PS）
- 默认**只显示选区（红圈），不立即修复**。点「✅ 应用修复」才在预览里真正愈合（变绿圈）。
- 这就成立「选区出来 → 点修复才看到效果」，不再「必须保存图片才见到」。
- 保存 / 导出**始终按全分辨率愈合**，和预览一致，结果不打折。
- 加「↩ 撤销应用」退回选区视图；加「实时预览修复」开关（开=标出即愈合，关=只显示红圈）。
- 选区**常驻标记**：即使已愈合也能看清修过哪里（红=待修，绿=已修）。
- 加**灵敏度滑块**（DetectParams.contrast_thr）：云天/平滑背景误检多就调高阈值，少检；想更灵敏就调低。

#### 3. 原有功能一个没丢
- 手动画笔、撤销一笔、清空、笔刷大小、四档算法（传统/自然/精修/内容感知）、
- 前后对比（按住反斜杠）、相册多图、保存、导出（MozJPEG 4:4:4 最高画质）、EXIF/XMP 保留——全部保留并验证通过。

### 推荐用法
1. 打开照片 → 左侧切到「污点」工具。
2. 点「✨ 自动检测污点」→ 画面出现红圈选区（连点不会翻倍）。
3. 误检多就调高「灵敏度」滑块，或手动「撤销一笔 / 清空」微调。
4. 点「✅ 应用修复」→ 预览里立刻看到修复结果（绿圈）。
5. 满意就「保存」或「导出」（全分辨率最终修复）。

### 下载
- **macOS（自用版，初色）**：`初色-0.6.7-macOS.zip`
- **macOS（分享版，Retouch）**：`Retouch-0.6.7-macOS.zip`
- **Windows 10 / 11（64 位）**：`初色-0.6.7-windows-x64.zip`

---

#### English

**Retouch v0.6.7** fixes two issues with auto spot detection from v0.6.6: clicking repeatedly doubled the spot count, and the workflow was unclear (users thought they had to save to see the result). Redesigned around the Photoshop "select, then apply" mental model.

**Fixes**
1. Auto-detect no longer doubles — each run clears previous auto-strokes (manual strokes kept); repeat clicks are safe. Regression test added.
2. Clear workflow: detection shows **red selection circles only** by default; "✅ 应用修复 / Apply Repair" heals in the preview (turns green). Save/Export always heals at full resolution. Added "↩ 撤销应用 / Undo apply", a "实时预览修复 / live-preview" toggle, persistent markers, and a **sensitivity slider** to cut false positives on smooth skies.
3. All existing features preserved and verified: manual brush, undo/clear, 4 heal modes, before/after compare, album, save, export (MozJPEG 4:4:4).

**Download**
- macOS (self-use, 初色): `初色-0.6.7-macOS.zip`
- macOS (share, Retouch): `Retouch-0.6.7-macOS.zip`
- Windows 10/11 (64-bit): `初色-0.6.7-windows-x64.zip`

---

### 许可证 / License
MIT。版权所有 © 2026 星TAP。
