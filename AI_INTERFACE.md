# retouch-rs AI 原生接口设计（B 阶段）

> 目标：让「任何 AI（人或模型）把一张图交给 retouch-rs，它看图后自己编号参数、直观展示结果」。
> 设计原则来自对 `retouch_app`（darktable 版）闭环的分析与**批判性继承**。

---

## 0. 为什么重做而不是复用 retouch_app

`retouch_app` 的流程（基线诊断 → 指标护栏 → AI 决策/微调 → 候选甜点搜索 → 定稿 → 视觉点评）**流程是对的，但引擎错了**：

- retouch_app 依赖 darktable-cli + Lua 守护进程 + LibRaw，体量大、跨平台难、参数语义要手写 `HARD_BOUNDS` 字典（已与真实模块漂移）。
- retouch-rs **从底层自研**（纯 Rust + OKLCH，无外部工具），同样的能力可以原生、轻量、零漂移地实现。
- 关键改进：**参数 schema 由代码单一来源自动生成**（`params::registry()`），AI 读到的范围永远是滑块真实范围，不存在第二份要维护的字典。

> 一句话：学 retouch_app 的「闭环方法论」，换掉它的「外部引擎」。

---

## 1. AI 要看/要写的，都是结构化 JSON

三个子命令构成完整闭环，全部机器可读：

### `retouch-rs analyze <img> --json`
把图量化成 OKLCH 感知指标（对标 retouch_app 的 Lab 指标，但用引擎原生空间）：
```json
{
  "width": 2310, "height": 540,
  "tone":   {"mean_l": 0.51, "std_l": 0.18, "min_l": 0.02, "max_l": 0.97},
  "color":  {"mean_c": 0.09, "mean_h_deg": 32.0, "hue_peakiness": 0.41,
             "per_hue_chroma": [0.12,0.08,0.05,0.03,0.02,0.04,0.07,0.10]},
  "skin":   {"ratio": 0.06, "mean_c": 0.05, "mean_h_deg": 28.0},
  "exposure":{"highlight_clip_pct": 0.4, "shadow_clip_pct": 0.1},
  "gamut":  {"clip_pct": 1.2, "max_c": 0.21},
  "cast":   {"hue_deg": 32.0, "chroma": 0.09},
  "dynamic_range": 0.95
}
```
这就是 AI「看」图的方式——不传像素，传客观度量。

### `retouch-rs schema --json`
返回**全部可调参数**的 schema，AI 据此知道能设什么、范围/单位/白话含义：
```json
[{"id":"exposure_ev","label":"曝光","description":"整体明暗，+提亮/-压暗","group":"exposure",
  "min":-0.8,"max":0.8,"default":0.0,"unit":"EV","curve":"soft_knee","bipolar":false},
 {"id":"skin_pinken","label":"加粉","description":"肤色往粉嫩方向推一点","group":"skin",
  "min":0.0,"max":1.0,"default":0.0,"unit":"","curve":"linear","bipolar":false}, ...]
```
`id` 即 AI 写参数时用的键。

### `retouch-rs render <img> <out> --params <json>`
AI 把决策写成 `{"exposure_ev":0.3,"vibrance":0.25,"skin_pinken":0.4}`，直接套用。
未知键/非数字会被告警跳过，且**所有值经护栏 clamp 到滑块安全区间**，AI 永远无法写出破坏性的参数。

### `retouch-rs auto <img> <out> --mode local [--json]`
跑完整自治闭环（见 §2）。`--json` 额外产出 `result.json`：
```json
{"metrics_before":{...},"metrics_after":{...},"guardrail_passed":true,
 "log":["[轮0] score=0.31 护栏=通过 | mean_l=0.51 std_l=0.20 mean_c=0.11", ...],
 "applied_params":{"exposure_ev":0.18,"contrast":0.18,"vibrance":0.25,"skin_pinken":0.3},
 "rounds":2}
```

---

## 2. 自治闭环（local 模式，零 key）

```
baseline = analyze(src)
adj = photo_default()                      # 好默认（去假色开）
adj += auto_correct(baseline)              # 规则启发式（见 §3）
clamp(adj)                                 # 护栏：限到滑块范围
for r in 0..rounds:
    proxy = render(src@1024)               # 小图快渲
    m = analyze(proxy)
    g = guardrail::check(m, baseline)      # 护栏：损伤拦截
    s = guardrail::score(m, baseline)      # 甜点打分
    if g.passed && s>best: best=adj        # 记住最安全且最优
    adj += auto_correct(m)                 # 再微调一轮
final = render(src@full, best + 细节/锐化/柔光)
```

逻辑完全对应 retouch_app 的 `decide.py` + `guardrail.py`，但**引擎换成我们自研的**，且规则在 OKLCH 空间表达。

---

## 3. auto_correct 启发式规则（对标 retouch_app LocalClient）

| 症状（指标） | 动作 |
|---|---|
| `mean_l < 0.42` | `exposure_ev +=` 提亮 |
| `mean_l > 0.66` | `exposure_ev -=` 压暗 |
| `std_l < 0.12`（平） | `contrast +`、`dehaze +`（去灰/清晰） |
| `mean_c < 0.06`（淡） | `vibrance +` |
| 主色相偏暖/偏蓝/偏绿 | `wb_temp` / `wb_tint` 中和 |
| 肤色占比>4% 且 C 低 | `skin_strength`+`skin_pinken`（粉嫩） |
| 肤色 C 过高 | `skin_yellow_reduce`（去黄压饱和） |
| 死黑>0.5% | `deep_shadow_lift`（提亮黑位） |

每轮都过护栏，所以规则可以「激进」——护栏会拦下过火候选。

---

## 4. 护栏（两层，移植 retouch_app guardrail.py）

1. **硬边界 clamp**：每个字段限到 `ParamSpec` 的滑块范围（自同步）。
2. **指标护栏**：候选图相对原图基线判定——
   - 肤色 C 漂移 > ±7 拦截
   - 偏色强度增长 > 0.05 拦截
   - 新增过曝/死黑 > 1% 拦截
3. **甜点打分**：奖励提反差/拉回中调/适度加饱和，惩罚削波与偏色；取最优候选定稿。

---

## 5. B5 真实 API 接入（已落地 ✅）

`auto --mode api` 现已用 **方案 A（轻量直连）** 落地：新增独立的 `retouch-agent` crate（lib，被 CLI 与 GUI 共用），内部用 `ureq 2.x`（`features=["json"]`，macOS 默认 `native-tls`，**无需 OpenSSL**）直接调两个最便宜的端点。引擎、指标、schema、护栏**完全不变**——闭环仍是 `auto::run_auto_loop`，只把「决策源」从本地 `auto_correct` 启发式换成 DeepSeek。

### 5.1 两个模型（复用 retouch_app 已验证的最便宜组合，但引擎换成我们自研的）

- **决策（文本模型）** — DeepSeek `deepseek-v4-flash`
  - 只看 JSON 指标 + 人/前轮参数，返回 `cfg`（`{"<field_id>": <value>, ...}`）。
  - 端点 `https://api.deepseek.com/v1/chat/completions`，`response_format=json_object`。
  - 系统提示里**注入实时 `param_schema()`**（SSOT），模型永远按滑块真实范围决策，杜绝范围漂移。
  - 端点 `api.deepseek.com/v1`，≈¥0.001–0.005/张。
- **点评 / 命名 / 观察（视觉模型）** — 阿里百炼 `qwen3-vl-flash`
  - `observe`：看 512px 缩图，输出 `scene_desc`（场景固有色描述）+ 构图 `crop` 建议。
  - `review`：看成片 512px 缩图，输出 `title` / `title_en` / `comment` / `comment_en` 投稿卡片。
  - 端点 `https://dashscope.aliyuncs.com/compatible-mode/v1`，≈¥0.0001/次。

### 5.2 铁律（移植自 retouch_app 的 prompt 硬规则，已写进 DeepSeek 系统提示）

① 先调光、后调色（初诊轮不动任何色彩字段）；② 固有色保护（禁止全局色相/饱和平移去破坏红跑道/蓝天/绿树）；③ 肤色优先、宁小勿大；④ 参数克制宁欠勿过；⑤ 输出**完整绝对参数集**（非增量）、禁止数值叠加。

### 5.3 关键护栏：从 retouch_app 学到的「偏色」教训

`retouch_app` 的参考包**处理图片老偏色**，根因是其全局 SPLCC 偏色校正把场景固有色（红跑道、蓝天）也一起推掉了。我们**明确拒绝**这套全局校正，只保留了「安全」的肤色微调（与我们的 `SkinTone` 同源）。

取而代之的防偏色做法：视觉模型先 `observe` 出 `scene_desc`（哪些大面积是固有色、哪些区域可能偏色），再**回灌给文本模型做决策护栏**——DeepSeek 据此只做局部微调、绝不碰全局色相。模型输出再经 `Field::from_id → set → guardrail::clamp`，物理上越不出安全区间。

### 5.4 调用方式

- **CLI**：`retouch-rs auto <in> <out> --mode api [--json]`
  - 读 `DEEPSEEK_API_KEY`（必填）与 `DASHSCOPE_API_KEY`（可选）；缺失 key 给出友好提示并退出 2，不崩溃。
  - 闭环：`observe`（可选）→ `run_auto_loop`（DeepSeek 决策）→ `review`（可选）→ 写 `result.json`（含 `auto` / `review` / `scene_desc`）。
- **GUI**（侧栏「全自动闭环」+「AI 联网设置」）：
  - 「本地一键修图」= 零 key 跑 `run_auto`；「AI 联网一键修图」= 跑 DeepSeek 决策 + 可选 Qwen 护栏。
  - 密钥在「AI 联网设置」里填（只存内存、不写盘），留空则回退系统环境变量。
  - 后台线程执行，不卡 UI；每帧 `poll_auto()` 把采用参数自动套用并提示。
  - 命令盘（Cmd+K）也支持：`localauto` / `apiauto`。

> 无论本地还是联网，引擎、指标、schema、护栏都**不变**——这正是 retouch_app PLAN.md 里「方法可复用、参数按图定制」原则的工程兑现。
