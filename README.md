# 初色 Retouch

**星TAP实验室 · 极致速度，极简生活**

> 本地运行、不联网、免费的 AI 修图工具。一键智能调色 + 无痕污点修复，Mac / Windows 都能用。

## 这是什么

初色（Retouch）是一款**完全在你电脑上运行**的修图软件——照片不上传任何服务器，隐私安全；软件免费，无订阅、无广告。它能帮你：

- 智能一键调色（自动分析照片、套用自然好看的参数）
- 无痕修复污点瑕疵（像专业软件的"修复画笔"那样从附近取真实纹理补上，不是简单糊掉）
- 批量处理、随手导出

## 为什么用它

- **省心**：所有运算本地完成，照片不出本机；免费，不用担心隐私和费用。
- **省事**：拖进来就能修，不用学复杂的专业软件。
- **省时**：导入、切图、导出全后台异步，界面不卡；Windows 上污点画笔"松手才算"，拖着画也跟手。

## 30 秒上手

1. 到 [Releases](../../releases) 下载 `Retouch-0.6.4-macOS.zip`（Mac）或 `Retouch-0.6.4-windows-x64.zip`（Windows），解压。
2. **第一次打开请先看压缩包里的 `首次打开必看-Retouch修图.txt`**（Mac 会提示"无法验证"、Win 会被 SmartScreen 拦，都是正常误报，照说明点一下即可）。
3. 把照片拖进窗口 → 左侧调色 / 选"污点"工具涂抹瑕疵 → 点导出。

## 主要特性

- 智能一键调色（本地规则 + 可选 AI 联网，API key 仅存内存）
- 污点修复三档：传统 / 自然 / 精修（Poisson 梯度域无缝融合，真实取纹理）
- 导入、切图、批量导出全异步，状态栏有进度和转圈提示
- 响应式布局：窗口拉窄自动折叠菜单、工具栏自动换行
- 纯本地、零依赖（Windows 无需安装 VC++；macOS 双击即用）

## 下载

| 平台 | 文件 |
|---|---|
| macOS（Apple 芯片 / Intel） | `Retouch-0.6.4-macOS.zip` |
| Windows 10 / 11（64 位） | `Retouch-0.6.4-windows-x64.zip` |

> 下载与首次打开的完整说明见压缩包内的 `首次打开必看-Retouch修图.txt`。

## 技术

Rust + [egui](https://github.com/emilk/egui)。核心色彩管线基于 OKLCH 感知色彩空间；污点修复为源块取纹理 + 频率分离 / Poisson 梯度域无缝融合。零外部依赖，Windows 静态链接 MSVC CRT。

构建：`cargo build --release`（Windows 交叉编译见 `scripts/build_win.sh`）。

---

# Retouch (English)

*StarTAP Lab · Extreme speed, minimalist life*

A fully **local, offline, free** photo retouching tool. One-click intelligent color grading and seamless spot/healing repair (source-texture Poisson cloning), for macOS and Windows.

**Highlights**

- Smart one-click grading (local heuristics, optional AI via your own API key kept in memory only)
- Spot healing in 3 modes: Traditional / Natural / Pro (Poisson gradient-domain seamless cloning — real texture sampling, not smearing)
- Fully async import / switch / batch-export with progress UI
- Responsive layout: auto-collapsing panels and wrapping toolbar
- Zero-dependency static builds (no VC++ runtime on Windows; double-click on macOS)

**Download**: see the Releases page. All processing runs on your machine; nothing leaves your PC.

Built with Rust + egui.

---

## 许可证 / License

本项目以 [MIT 许可证](LICENSE) 发布。版权所有 © 2026 星TAP。

Licensed under the [MIT License](LICENSE). Copyright © 2026 星TAP.
