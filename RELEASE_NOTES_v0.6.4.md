## 初色 Retouch v0.6.4

**一句话**：一款**完全本地运行、不联网、免费**的修图工具——帮你一键智能调色、无痕修复照片污点瑕疵，Mac 和 Windows 都能用，照片不出本机。

### 对你有什么用？

- **省心**：所有运算都在你电脑上跑，照片不上传任何服务器，隐私安全；软件免费，无订阅、无广告。
- **省事**：智能一键调色 + 污点修复（像专业软件的"修复画笔"那样，从附近自然取纹理补上，不是简单糊掉）；拖进来就能修。
- **省时**：导入 / 切图 / 导出全部后台处理，界面不卡顿；Windows 上污点画笔"松手才算"，拖着画一串也跟手流畅。

### 30 秒上手

1. 下载下面的 `Retouch-0.6.4-macOS.zip` 或 `Retouch-0.6.4-windows-x64.zip`，解压。
2. **第一次打开请先看压缩包里的 `首次打开必看-Retouch修图.txt`**（Mac 会提示"无法验证"、Win 会被 SmartScreen 拦，都是正常误报，照说明点一下即可）。
3. 把照片拖进窗口 → 左侧调色 / 选"污点"工具涂抹瑕疵 → 点导出。

### 主要特性

- 智能一键调色（本地规则 + 可选 AI 联网，API key 仅存内存）
- 污点修复三档：传统 / 自然 / 精修（Poisson 梯度域无缝融合，真实取纹理）
- 导入、切图、批量导出全异步，状态栏有进度和转圈提示
- 响应式布局：窗口拉窄自动折叠菜单、工具栏自动换行
- 纯本地、零依赖（Windows 无需安装 VC++；macOS 双击即用）

### 下载

- **macOS**（Apple 芯片 / Intel）：`Retouch-0.6.4-macOS.zip`
- **Windows 10 / 11（64 位）**：`Retouch-0.6.4-windows-x64.zip`

---

#### English

**Retouch v0.6.4** — a fully local, offline, free photo retouching tool. One-click intelligent color grading and seamless spot/healing repair (source-texture Poisson cloning), for macOS and Windows.

**Highlights**

- Smart one-click grading (local heuristics, optional AI via your own API key kept in memory only)
- Spot healing in 3 modes: Traditional / Natural / Pro (Poisson gradient-domain seamless cloning, real texture sampling — not smearing)
- Fully async import / switch / batch-export with progress UI
- Responsive layout: auto-collapsing panels and wrapping toolbar
- Zero-dependency static builds (no VC++ runtime on Windows; double-click on macOS)

**Build**: Rust + egui. `cargo build --release` (cross-compile Windows via `cargo xwin`).
