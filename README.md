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

1. 到 [Releases](https://github.com/cscb603/retouch-rs/releases) 下载对应版本：
   - **macOS（分享版）**：`初色-0.6.9-macOS.zip`
   - **Windows 10/11 64 位（分享版，无 Key）**：`初色-0.6.9-windows-分享版.zip`
   - **Windows 10/11 64 位（自用版，含 Qwen Key）**：`初色-0.6.9-windows-自用版.zip`
2. **第一次打开请先看压缩包里的 `首次打开必看-初色.txt`**（Mac 会提示"无法验证"、Win 会被 SmartScreen 拦，都是正常误报，照说明点一下即可）。
3. 把照片拖进窗口 → 左侧调色 / 选"污点"工具涂抹瑕疵 → 点导出。
4. 需要后台批量 / 自动化？见下方「命令行（CLI）调用」。

## 主要特性

- 智能一键调色（本地规则 + 可选 AI 联网，API key 仅存内存）
- 污点修复**四档**，按场景选：
  - **传统（Telea）**：小污点兜底，最快
  - **自然（频率分离）**：源块高频纹理 + 目标邻域低频光照融合
  - **精修（Poisson 梯度域无缝克隆）**：完全无痕，默认档，适合小瑕疵 / 人像美肤
  - **内容感知（PatchMatch，v0.6.5 新增）**：细线 / 电线 / 杆 / 细缝去除专用——纹理连续区域比 Telea 更自然；大块物体去除仍建议用精修档
- 导入、切图、批量导出全异步，状态栏有进度和转圈提示
- 响应式布局：窗口拉窄自动折叠菜单、工具栏自动换行
- 纯本地、零依赖（Windows 无需安装 VC++；macOS 双击即用）

## 下载

| 平台 | 文件 | 说明 |
|---|---|---|
| macOS 11+（Apple 芯片 / Intel） | `初色-0.6.9-macOS.zip` | 分享版，右键打开即用 |
| Windows 10 / 11（64 位） | `初色-0.6.9-windows-分享版.zip` | 分享版，无 AI Key，免 VC++ 运行库 |
| Windows 10 / 11（64 位） | `初色-0.6.9-windows-自用版.zip` | 自用版，已内置 Qwen Key，AI 追色/命名可用 |

> 下载与首次打开的完整说明见压缩包内的 `首次打开必看-初色.txt`。
> 自用版与分享版**功能完全一致**，区别仅在是否附带 Qwen Key（Key 仅存本地、不联网上传）。

## 命令行（CLI）调用

初色同时提供无界面命令行引擎 `retouch-rs`，适合**后台批量处理、自动化流水线、AI Agent 调用**——GUI 里能做的调色/分析，命令行都能跑，且不占用桌面。

- **获取 CLI 二进制**：源码编译 `cargo build -p retouch-cli` 即可；Windows 分享包只含 GUI（`初色.exe`），CLI 需自行用源码编出 `retouch-rs`。
- **子命令速查**：

  | 命令 | 作用 |
  |---|---|
  | `retouch-rs render 原图.jpg 成品.jpg --exposure 0.3 --contrast 0.15` | 跑 OKLCH 管线渲染（参数可逐个覆盖） |
  | `retouch-rs analyze 原图.jpg --json` | 把图片量化成 AI 可读的 OKLCH 指标（Agent「看」图用，不传像素） |
  | `retouch-rs auto 原图.jpg 成品.jpg` | 一键智能调色（本地规则 / 可选 Qwen API） |
  | `retouch-rs name 原图.jpg --key <QWEN_KEY>` | 用 Qwen 给图片起名/评分 |
  | `retouch-rs schema --json` | 导出全部可调参数 id（供 Agent 程序化设参） |
  | `retouch-rs dump --preset x.toml 输出.toml` | 把 preset+CLI 参数固化成 TOML |
  | `retouch-rs verify 原图.jpg` | 自检：色彩保真 / 功能正确 / 性能基准 |

- **参数组合**：任意子命令都能 `--preset xxx.toml` 作底，再用 `--exposure` 等逐个覆盖；或直接 `--params '{"exposure":0.3}'` 按参数 id 设定。可用参数见 `retouch-rs schema --json`。
- **后台示例**（无窗口、可丢服务器跑）：

  ```bash
  # 批量把整目录照片按同一套参数渲染
  for f in raw/*.jpg; do
    retouch-rs render "$f" "out/$(basename "$f")" --preset my-grade.toml --sharpen 0.4
  done
  ```

> 完整参数见 `crates/retouch-cli` 源码；构建：Mac/Linux 直接 `cargo build -p retouch-cli`，Windows 见 `scripts/build_win.sh`（默认编 GUI，CLI 同源）。

## 技术

Rust + [egui](https://github.com/emilk/egui)。核心色彩管线基于 OKLCH 感知色彩空间；污点修复为源块取纹理 + 频率分离 / Poisson 梯度域无缝融合 + PatchMatch（Criminisi 式逐环边界填充）。零外部依赖，Windows 静态链接 MSVC CRT。

构建：`cargo build --release`（Windows 交叉编译见 `scripts/build_win.sh`）。

---

# Retouch (English)

*StarTAP Lab · Extreme speed, minimalist life*

A fully **local, offline, free** photo retouching tool. One-click intelligent color grading and seamless spot/healing repair, for macOS and Windows.

**Highlights**

- Smart one-click grading (local heuristics, optional AI via your own API key kept in memory only)
- Spot healing in **4 modes**: Traditional / Natural / Pro (Poisson) / Content-Aware (PatchMatch, new in v0.6.5 — best for thin wires / poles / seams)
- Fully async import / switch / batch-export with progress UI
- Responsive layout: auto-collapsing panels and wrapping toolbar
- Zero-dependency static builds (no VC++ runtime on Windows; double-click on macOS)

**Download**: see the Releases page. All processing runs on your machine; nothing leaves your PC.

Built with Rust + egui.

---

## 许可证 / License

本项目以 [MIT 许可证](LICENSE) 发布。版权所有 © 2026 星TAP。

Licensed under the [MIT License](LICENSE). Copyright © 2026 星TAP.
