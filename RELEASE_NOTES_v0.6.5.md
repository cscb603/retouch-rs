## 初色 Retouch v0.6.5

**一句话**：一款**完全本地运行、不联网、免费**的修图工具——帮你一键智能调色、无痕修复照片污点瑕疵，Mac 和 Windows 都能用，照片不出本机。

### v0.6.5 新增：内容感知移除（PatchMatch）
- 污点修复新增第 4 档「**内容感知**」：基于 Criminisi 式逐环边界填充（PatchMatch），
  专为**细线 / 电线 / 杆 / 细缝**去除设计，纹理连续区域比传统 Telea 更自然。
- 真机验收实测（合成电线图，512×512）：四档都能跑、不崩溃；内容感知档修复后
  电线区 PSNR **25.6 dB**（最高，Telea 24.2 / FreqSep 10.5 / Poisson 7.1），且对背景改动最小（63）。
- 分工建议：小瑕疵 / 人像美肤用「精修（Poisson）」；细电线 / 杆 / 细缝用「内容感知」；大块物体仍建议精修。

### 对你有什么用？

- **省心**：所有运算都在你电脑上跑，照片不上传任何服务器，隐私安全；软件免费，无订阅、无广告。
- **省事**：智能一键调色 + 污点修复（像专业软件的"修复画笔"那样，从附近自然取纹理补上，不是简单糊掉）；拖进来就能修。
- **省时**：导入 / 切图 / 导出全部后台处理，界面不卡顿；Windows 上污点画笔"松手才算"，拖着画一串也跟手流畅。

### 30 秒上手

1. 下载下面的 `初色-自用版-Mac-0.6.5.zip` 或 `初色-0.6.5-windows-x64.zip`，解压。
2. **第一次打开请先看压缩包里的 `首次打开必看-初色.txt`**（Mac 会提示"无法验证"、Win 会被 SmartScreen 拦，都是正常误报，照说明点一下即可）。
3. 把照片拖进窗口 → 左侧调色 / 选"污点"工具涂抹瑕疵 → 点导出。

### 主要特性

- 智能一键调色（本地规则 + 可选 AI 联网，API key 仅存内存）
- 污点修复四档：传统 / 自然 / 精修 / 内容感知（PatchMatch）
- 导入、切图、批量导出全异步，状态栏有进度和转圈提示
- 响应式布局：窗口拉窄自动折叠菜单、工具栏自动换行
- 纯本地、零依赖（Windows 无需安装 VC++；macOS 双击即用）

### 下载

- **macOS**（Apple 芯片 / Intel）：`初色-自用版-Mac-0.6.5.zip`
- **Windows 10 / 11（64 位）**：`初色-0.6.5-windows-x64.zip`

---

#### English

**Retouch v0.6.5** — a fully local, offline, free photo retouching tool. One-click intelligent color grading and seamless spot/healing repair (source-texture Poisson cloning + new Content-Aware PatchMatch), for macOS and Windows.

**What's new in v0.6.5**
- 4th healing mode **Content-Aware (PatchMatch)**: best for thin wires / poles / seams — more natural texture continuity than Telea.
- Verified on a synthetic wire image: all 4 modes run without crashing; Content-Aware reaches the highest wire-region PSNR (25.6 dB) with the least background disturbance.

**Highlights**
- Smart one-click grading (local heuristics, optional AI via your own API key kept in memory only)
- Spot healing in 4 modes: Traditional / Natural / Pro (Poisson) / Content-Aware
- Fully async import / switch / batch-export with progress UI
- Responsive layout: auto-collapsing panels and wrapping toolbar
- Zero-dependency static builds (no VC++ runtime on Windows; double-click on macOS)

**Build**: Rust + egui. `cargo build --release` (cross-compile Windows via `cargo xwin`).

---

### 许可证 / License

本项目以 MIT 许可证发布，完全免费、可自由使用与修改。版权所有 © 2026 星TAP。

Licensed under the MIT License — free to use and modify. Copyright © 2026 星TAP.

完整条款见仓库根目录 [`LICENSE`](https://github.com/cscb603/retouch-rs/blob/main/LICENSE)。
