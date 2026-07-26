//! retouch-rs desktop GUI (M4) — egui / eframe.
//!
//! ACR-style side panel (every Adjustments field as a slider) + a Cmd+K
//! command palette. Loads a JPG/TIFF, renders a downscaled preview in
//! real-time as you drag sliders, and exports full-resolution on Save.
//!
//! GUI-only binary (`retouch-rs-gui`); the engine is `retouch-core`.

// 在 Windows 上以 GUI 子系统链接（双击不弹控制台黑窗口）；
// 其它平台编译器自动忽略此属性。
#![windows_subsystem = "windows"]

use eframe::egui;
use image::GenericImageView;
use palette::{IntoColor, LinSrgb, Oklab};
use retouch_agent::{thumb_b64, QwenClient};
use retouch_core::analyze::{analyze, ImageMetrics};
use retouch_core::auto::{run_auto, AutoResult};
use retouch_core::auto_color::{auto_neutral_balance, film_presets};
use retouch_core::geometry::{apply_geometry, Geometry};
use retouch_core::params::{registry, Field, ParamSpec};
use retouch_core::pipeline::{
    render, smart_beauty_preset, Adjustments, HslRegions, SkinTone, ToneMapMode,
};
use retouch_core::preset::{dump_preset, load_preset, Preset};
use retouch_core::reference::run_reference_match;
use retouch_core::spot::{HealMode, SpotFix};
use retouch_core::tonemap::tonal_adjustments;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

mod tips;

const PREVIEW_MAX: u32 = 1400;

/// Background render job: preview is produced on a dedicated thread so the UI
/// stays interactive while heavy modules (OKLCH grade, multi-zone, skin) run.
struct RenderRequest {
    src: Arc<image::DynamicImage>,
    adj: Adjustments,
    preview_max: u32,
    need_before: bool,
}

struct RenderResult {
    after_rgb: Vec<u8>,
    after_size: [usize; 2],
    before_rgb: Option<Vec<u8>>,
    before_size: [usize; 2],
}

/// 后台导入消息：解码线程流式回传缩略图，主线程 poll 逐张追加进相册。
enum ImportMsg {
    Item {
        path: PathBuf,
        thumb: Option<Arc<image::DynamicImage>>,
    },
    Done,
}

/// 后台切图/打开：全分辨率解码 + 指标分析在后台线程完成，用 gen 令牌防竞态。
struct LoadMsg {
    gen: u64,
    path: PathBuf,
    result: Result<(image::DynamicImage, ImageMetrics), String>,
}

/// 后台批量导出：单张 job（主线程预生成名字避免冲突后交给线程）。
struct ExportJob {
    path: PathBuf,
    adj: Adjustments,
    spot: Option<SpotFix>,
    out_path: PathBuf,
}

/// 后台批量导出进度消息。
enum ExportMsg {
    Step { done: usize, ok: usize, fail: usize },
    Done { ok: usize, fail: usize },
}

struct RetouchApp {
    adj: Adjustments,
    src: Option<image::DynamicImage>,
    src_path: Option<PathBuf>,
    texture: Option<egui::TextureHandle>,
    before_texture: Option<egui::TextureHandle>,
    status: String,
    dirty: bool,
    show_cmd: bool,
    cmd: String,
    /// Perceptual control registry (drives all scalar sliders).
    params: Vec<ParamSpec>,
    zoom: f32,
    pan: egui::Vec2,
    is_panning: bool,
    compare_mode: CompareMode,
    split_pos: f32,
    /// OKLCH metrics of the *original* opened image (what the AI "sees").
    img_metrics: Option<ImageMetrics>,
    /// Background smart-correction state (local or reference match).
    auto_running: bool,
    auto_result: Arc<Mutex<Option<AutoResult>>>,
    /// 后台修图模式：决定结果如何套用（中性 / 参考匹配）。
    auto_mode: AutoMode,
    /// 参考图匹配：已导入的参考图路径 / 指标 / 缩略图。
    ref_path: Option<PathBuf>,
    ref_metrics: Option<ImageMetrics>,
    ref_texture: Option<egui::TextureHandle>,
    /// 匹配强度 0..=1（默认 0.8）：0=不变，1=完全贴合参考。
    match_strength: f32,
    /// 生成作品名（Qwen 视觉，可选）的异步结果通道 + 最近一次结果。
    title_result: Arc<Mutex<Option<String>>>,
    last_title: Option<String>,
    /// Qwen(DashScope) Key，仅用于「生成作品名」；填一次后写入本地文件记住，免重复输入。
    api_qwen_key: String,
    /// 作品名设置面板是否展开（默认折叠；记住 key 后无需每次展开）。
    qwen_open: bool,
    /// Async preview-render channel. render_tx sends jobs, render_rx receives results.
    render_tx: Sender<RenderRequest>,
    render_rx: Receiver<RenderResult>,
    render_pending: bool,
    /// 导出配置（保存对话框）
    show_export: bool,
    export_cfg: retouch_core::export::ExportConfig,
    /// Remember last-used directories for open / save dialogs.
    last_open_dir: Option<PathBuf>,
    last_save_dir: Option<PathBuf>,
    /// 自动中性化后是否启用场景感知补偿（默认开启）。
    smart_compensation: bool,
    /// 一键中性力度档位：0.5=弱 / 1.0=中（默认）/ 1.8=强。
    /// 只缩放增强类字段（反差/胶片/暗部/鲜艳/去雾），保护类（曝光位置/白平衡/色调映射）不动。
    neutral_strength: f32,
    /// 自动中性化的亮度基线（用于「还原亮度」滑块回退原始亮度，保留颜色校正）。
    auto_baseline: Option<Adjustments>,
    /// 曝光还原 0..1：0=完全自动曝光，1=尽量回退到原图亮度（颜色校正保留）。
    exposure_restore: f32,
    /// 一键展开/收起：Some(true)=本帧强制全部展开，Some(false)=全部收起，
    /// None=保持各自记忆状态。应用后立即清回 None。
    force_open: Option<bool>,
    /// 是否显示右侧相册栏。窄窗自动折叠、宽窗自动展开（阈值跨越时切换），
    /// 其间用户可用工具栏「相册」按钮手动覆盖。
    show_album: bool,
    /// 上一帧窗口是否处于窄屏（< 900px）。用于检测阈值跨越，只在跨越时自动切换。
    was_narrow: bool,
    /// 已校色「正立」基图（预览像素，RGB，3 通道）。仅颜色/色调参数变化时经
    /// 异步管线重算；几何（旋转/翻转/裁剪）只作用在它上面，绝不重跑重型管线。
    base_rgba: Option<Vec<u8>>,
    base_size: [usize; 2],
    /// 原图（未校色、正立）基图，用于 before 对比；同样施加几何以保证对比同向。
    before_rgba: Option<Vec<u8>>,
    before_size: [usize; 2],
    /// 仅几何变化（旋转/翻转/裁剪）：同步、微秒级重算预览，不触发异步颜色管线。
    dirty_geo: bool,
    /// 主题模式：自动（按时段）/ 深色 / 浅色
    theme_mode: ThemeMode,
    /// 当前显示的小技巧
    current_tip: &'static str,
    /// 相册（v0.6 轻量 Lightroom 化）：多图批处理，活跃索引见 album.active_idx。
    album: Album,
    /// 工具模式：调色 / 污点修复画笔。
    tool_mode: ToolMode,
    /// 当前活跃 slot 的污点层（工作副本；切换时落回 slot）。
    spot: Option<SpotFix>,
    /// 污点画笔半径（预览像素，2..50）。
    spot_brush: u32,
    /// 污点拖动起点笔画索引（Some=正在拖动）。用于「松手才算」：
    /// 拖动时只累积笔画并画红点预览，不逐帧 heal；松手才一次性愈合。
    spot_drag_base: Option<usize>,
    /// 智能美肤强度 0..1（默认 0.5），映射到 skin.strength + freqsep.strength。
    beauty_strength: f32,
    /// 污点修复算法档位（默认 Poisson 精修），存入 SpotFix 随图持久化。
    heal_mode: HealMode,

    // ── v0.6.3 响应性：把重解码全部移出主线程，界面不再冻结 ──
    /// 后台导入通道（Some=正在导入）。缩略图解码在线程里做，主线程流式追加。
    import_rx: Option<Receiver<ImportMsg>>,
    /// 本轮导入的起点参数（第一张到达时写入 slot0）。
    import_base_adj: Adjustments,
    import_total: usize,
    import_done: usize,
    /// 后台全分辨率解码通道（常驻）。切图/打开都走这里，gen 令牌区分请求。
    load_tx: Sender<LoadMsg>,
    load_rx: Receiver<LoadMsg>,
    /// 当前活跃图的载入代号：每次切图 +1，poll 只采纳最新代号的结果。
    load_gen: u64,
    /// 是否正在载入活跃图（显示"载入中…"）。
    loading: bool,
    /// 后台批量导出通道（Some=正在导出）。
    export_rx: Option<Receiver<ExportMsg>>,
    export_total: usize,
    export_done: usize,
}

/// 主题模式：自动（按时段切换深/浅）、始终深色、始终浅色
#[derive(Clone, Copy, Debug, PartialEq)]
enum ThemeMode {
    Auto,
    Dark,
    Light,
}

impl ThemeMode {
    fn label(self) -> &'static str {
        match self {
            ThemeMode::Auto => "自动",
            ThemeMode::Dark => "深色",
            ThemeMode::Light => "浅色",
        }
    }
    fn icon(self) -> &'static str {
        // 用汉字替代 emoji，杜绝跨平台基线偏移
        // auto=自, dark=暗, light=明
        match self {
            ThemeMode::Auto => "自",
            ThemeMode::Dark => "暗",
            ThemeMode::Light => "明",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
enum CompareMode {
    #[default]
    Off,
    Toggle,
    Split,
}

/// 后台修图模式：决定结果如何套用到当前参数。
#[derive(Clone, Copy, Debug, PartialEq, Default)]
enum AutoMode {
    /// 一键中性：结果直接覆盖到当前参数。
    #[default]
    Neutral,
    /// 参考图匹配：结果按 `match_strength` 从当前参数向匹配结果混合。
    Reference,
}

/// 工具模式：普通调色 vs 污点修复画笔（v0.6）。
#[derive(Clone, Copy, Debug, PartialEq, Default)]
enum ToolMode {
    #[default]
    Adjust,
    Spot,
}

/// 单图状态容器（相册中的一张）。
/// 内存纪律：仅持有 ≤512px 缩略图 + 参数 + 污点笔画（极轻）；
/// 原图 `src_full` 懒加载、用完即释放，保证 50 张内存 < 200MB。
struct Slot {
    path: PathBuf,
    /// ≤512px 缩略图，导入时生成。
    thumb: Option<Arc<image::DynamicImage>>,
    /// 该张独立参数（切图默认沿用上一张作起点，各自可独立改）。
    adj: Adjustments,
    /// 污点修复层（None=无）。
    spot: Option<SpotFix>,
    /// 缩略图纹理（右侧栏显示），懒缓存。
    thumb_tex: Option<egui::TextureHandle>,
    /// 自动起名结果（可选）。
    title: Option<String>,
    /// 批量导出是否选中（默认 true）。
    selected: bool,
}

impl Slot {
    fn new(path: PathBuf, thumb: Option<Arc<image::DynamicImage>>, adj: Adjustments) -> Self {
        Self {
            path,
            thumb,
            adj,
            spot: None,
            thumb_tex: None,
            title: None,
            selected: true,
        }
    }
}

/// 相册：管理多张 Slot、活跃索引、选中集（v0.6 轻量 Lightroom 化）。
struct Album {
    slots: Vec<Slot>,
    active_idx: usize,
}

impl Album {
    fn new() -> Self {
        Self {
            slots: Vec::new(),
            active_idx: 0,
        }
    }
    fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
    /// 基础导出文件名：作品名优先（提取《》内），否则源文件名；不含扩展名。
    fn base_name(&self, idx: usize) -> String {
        if let Some(slot) = self.slots.get(idx) {
            if let Some(t) = &slot.title {
                if let Some(s) = t.find('《') {
                    let start = s + 3;
                    if let Some(e) = t.find('》') {
                        let name = t[start..e].trim().to_string();
                        if !name.is_empty() {
                            return name;
                        }
                    }
                }
            }
            if let Some(stem) = slot.path.file_stem() {
                return stem.to_string_lossy().to_string();
            }
        }
        "photo".to_string()
    }
}

impl RetouchApp {
    fn new() -> Self {
        let (render_tx, render_rx_thread) = channel::<RenderRequest>();
        let (result_tx, result_rx) = channel::<RenderResult>();
        // 全分辨率解码常驻通道：切图/打开的解码都发到后台线程，回传走这里。
        let (load_tx, load_rx) = channel::<LoadMsg>();

        // Dedicated preview-render thread. It downscales the source once, then
        // runs the OKLCH pipeline on the downscaled image. Result is sent back
        // to the main thread for GPU upload. This keeps slider dragging smooth.
        std::thread::spawn(move || {
            while let Ok(req) = render_rx_thread.recv() {
                let (w, h) = req.src.dimensions();
                let scale = (req.preview_max as f32 / w.max(h) as f32).min(1.0);
                let tw = (w as f32 * scale) as u32;
                let th = (h as f32 * scale) as u32;
                let thumb = req
                    .src
                    .resize(tw, th, image::imageops::FilterType::Triangle);

                // 渲染出错（如极端参数导致 panic）不会炸掉整个 app：
                // catch_unwind 捕获后跳过本轮渲染，app 继续运行。
                let render_result = std::panic::catch_unwind({
                    let t = thumb.clone();
                    let a = req.adj.clone();
                    move || {
                        // 预览基图只跑「正立」颜色管线：几何剥离，单独作用在小缓冲上。
                        // 保证颜色模块永远只见正立图（与「加旋转前」完全一致），从根上
                        // 消除旋转后宽高互换触发底层外部异常导致崩溃的问题。
                        let mut base_adj = a;
                        base_adj.geometry = Geometry::default();
                        let after = render(&t, &base_adj);
                        let (aw, ah) = after.dimensions();
                        let after_rgb = after.into_raw();

                        let before = if req.need_before {
                            let b = render(&t, &Adjustments::identity());
                            let (bw, bh) = b.dimensions();
                            Some((b.into_raw(), [bw as usize, bh as usize]))
                        } else {
                            None
                        };

                        (after_rgb, [aw as usize, ah as usize], before)
                    }
                });

                match render_result {
                    Ok((after_rgb, after_size, before)) => {
                        if result_tx
                            .send(RenderResult {
                                after_rgb,
                                after_size,
                                before_rgb: before.as_ref().map(|(rgb, _)| rgb.clone()),
                                before_size: before.map(|(_, size)| size).unwrap_or([0, 0]),
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("[retouch-rs] render panic: {:?}", e);
                        // 不发送结果，跳过此帧
                    }
                }
            }
        });

        Self {
            adj: Adjustments::photo_default(),
            src: None,
            src_path: None,
            texture: None,
            before_texture: None,
            status: "按 Cmd+O 打开图片，Cmd+P 加载预设".into(),
            dirty: true,
            show_cmd: false,
            cmd: String::new(),
            params: registry(),
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            is_panning: false,
            compare_mode: CompareMode::Off,
            split_pos: 0.5,
            img_metrics: None,
            auto_running: false,
            auto_result: Arc::new(Mutex::new(None)),
            auto_mode: AutoMode::Neutral,
            ref_path: None,
            ref_metrics: None,
            ref_texture: None,
            match_strength: 0.8,
            title_result: Arc::new(Mutex::new(None)),
            last_title: None,
            api_qwen_key: Self::load_qwen_key(),
            qwen_open: false,
            render_tx,
            render_rx: result_rx,
            render_pending: false,
            show_export: false,
            export_cfg: Default::default(),
            last_open_dir: None,
            last_save_dir: None,
            smart_compensation: true,
            neutral_strength: 1.0,
            auto_baseline: None,
            exposure_restore: 0.0,
            force_open: None,
            show_album: true,
            was_narrow: false,
            base_rgba: None,
            base_size: [0, 0],
            before_rgba: None,
            before_size: [0, 0],
            dirty_geo: false,
            theme_mode: ThemeMode::Auto,
            current_tip: tips::random_tip(),
            album: Album::new(),
            tool_mode: ToolMode::Adjust,
            spot_drag_base: None,
            spot: None,
            spot_brush: 12,
            beauty_strength: 0.5,
            heal_mode: HealMode::Poisson,
            import_rx: None,
            import_base_adj: Adjustments::photo_default(),
            import_total: 0,
            import_done: 0,
            load_tx,
            load_rx,
            load_gen: 0,
            loading: false,
            export_rx: None,
            export_total: 0,
            export_done: 0,
        }
    }

    /// Look up a `ParamSpec` by field (cloned — cheap, ~40 small structs).
    fn spec(&self, f: Field) -> ParamSpec {
        self.params
            .iter()
            .find(|s| s.field == f)
            .cloned()
            .expect("field present in registry")
    }

    /// Render one perceptually-mapped slider as a clean vertical block:
    ///   1. Parameter name (above)
    ///   2. Full-width slider track (no built-in label/value so it really fills)
    ///   3. Live value readout (below the track, not touching it)
    ///   4. Color bar aligned with the track
    ///   5. Generous bottom spacing before the next slider
    ///
    /// This replaces the previous cramped layout where the label, value, bar and
    /// next slider were all stacked with tiny gaps.
    /// Render ONE smart, perceptually-mapped slider with a live effective-value
    /// readout. Layout (top to bottom): label → slider → value → color bar,
    /// each with equal, breathable spacing and the slider track filling the
    /// full available width.
    fn param_slider(&mut self, ui: &mut egui::Ui, f: Field) -> bool {
        let spec = self.spec(f);
        let raw = spec.field.get(&self.adj);
        let pos0 = spec.to_pos(raw);
        let mut pos = if spec.bipolar {
            pos0
        } else {
            (pos0 + 1.0) * 0.5
        };
        let range = if spec.bipolar { -1.0..=1.0 } else { 0.0..=1.0 };

        ui.vertical(|ui| {
            // Manual spacing so every gap is explicit.
            ui.spacing_mut().item_spacing.y = 0.0;

            // 1. Label above the slider.
            ui.label(egui::RichText::new(&spec.label).strong())
                .on_hover_text(spec.tooltip);

            // 2. Slider track: shortened to 2/3 of the available width so there
            // is breathing room on both sides. No built-in label/value so resp
            // is exactly the track.
            ui.add_space(6.0);
            ui.spacing_mut().slider_width = ui.available_width() * 0.67;
            let slider = egui::Slider::new(&mut pos, range).show_value(false);
            let resp = ui.add(slider).on_hover_text(spec.tooltip);
            let changed = resp.changed();
            if changed {
                let p = if spec.bipolar { pos } else { pos * 2.0 - 1.0 };
                let new_raw = spec.to_raw(p);
                spec.field.set(&mut self.adj, new_raw);
            }

            // 3. Live value readout — generous spacing from the slider.
            ui.add_space(8.0);
            let raw_after = spec.field.get(&self.adj);
            let value_str = spec.fmt(raw_after);
            let hint = field_hint(&spec.field, raw_after).unwrap_or_default();
            let readout = if hint.is_empty() {
                value_str
            } else {
                format!("{} · {}", value_str, hint)
            };
            ui.label(egui::RichText::new(readout).monospace());

            // 4. Color bar — drawn at the current cursor Y (not resp.rect.bottom,
            // which would be above the value text). Horizontally aligned with
            // the slider track via resp.rect.
            ui.add_space(12.0);
            let bar_top = ui.cursor().min.y;
            self.paint_bar(ui, &resp, &spec, raw_after, bar_top);

            // 5. Generous bottom spacing before the next parameter.
            ui.add_space(16.0);
            changed
        })
        .inner
    }

    /// Paint a gradient bar below a slider, horizontally aligned with the
    /// slider track (via `resp.rect`) but vertically at `bar_top` (the current
    /// UI cursor position, passed in from the caller so it never overlaps with
    /// the value readout above).
    ///   - Width from slider response rect
    ///   - Skip when < 20px (too narrow to be useful)
    ///   - 120 discrete segments for smooth gradient
    ///   - 8px tall, 2px corner radius
    ///   - White tick line marking current value position
    fn paint_bar(
        &self,
        ui: &mut egui::Ui,
        resp: &egui::Response,
        spec: &ParamSpec,
        raw: f32,
        bar_top: f32,
    ) {
        let Some(colors) = field_gradient(&spec.field, raw) else {
            return;
        };
        let bar_left = resp.rect.left();
        let bar_right = resp.rect.right();
        let painter = ui.painter();
        let bar_y = bar_top;
        let bar_h = 8.0;
        let bar = egui::Rect::from_min_max(
            egui::pos2(bar_left, bar_y),
            egui::pos2(bar_right, bar_y + bar_h),
        );
        // Guard: too narrow → skip entirely.
        if bar.width() < 20.0 {
            return;
        }
        // 120 discrete vertical strips (FilmRust-proven smoothness).
        let n = colors.len().max(16).min(120);
        let sw = bar.width() / n as f32;
        for i in 0..n {
            let ci = i * (colors.len() - 1) / n.max(1);
            let c = &colors[ci];
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(bar.left() + sw * i as f32, bar.top()),
                    egui::vec2(sw + 0.5, bar.height()),
                ),
                1.0,
                egui::Color32::from_rgb(
                    (c[0].clamp(0.0, 1.0) * 255.0) as u8,
                    (c[1].clamp(0.0, 1.0) * 255.0) as u8,
                    (c[2].clamp(0.0, 1.0) * 255.0) as u8,
                ),
            );
        }
        // White tick line at current value position (extends slightly beyond bar).
        let pos = spec.to_pos(raw);
        let t = (pos + 1.0) * 0.5; // normalize bipolar→[0,1]
        let tick_x = bar_left + t.clamp(0.0, 1.0) * bar.width();
        painter.line_segment(
            [
                egui::pos2(tick_x, bar.top() - 2.0),
                egui::pos2(tick_x, bar.bottom() + 2.0),
            ],
            (1.5, egui::Color32::WHITE),
        );
    }

    /// Analyze the open image (downscaled) for smart one-click controls:
    /// mean OKLCH L, mean OKLab a/b (color cast), and L std (haze proxy).
    fn analyze(&self) -> Option<(f32, f32, f32, f32)> {
        let src = self.src.as_ref()?;
        let (w, h) = src.dimensions();
        let scale = (512.0 / w.max(h) as f32).min(1.0);
        let tw = (w as f32 * scale) as u32;
        let th = (h as f32 * scale) as u32;
        let img = src
            .resize(tw, th, image::imageops::FilterType::Triangle)
            .to_rgb8();
        let mut n = 0u32;
        let mut sl = 0.0f64;
        let mut sa = 0.0f64;
        let mut sb = 0.0f64;
        let mut sl2 = 0.0f64;
        for p in img.pixels() {
            let lin = LinSrgb::new(
                srgb_to_linear(p[0]),
                srgb_to_linear(p[1]),
                srgb_to_linear(p[2]),
            );
            let ok: Oklab<f32> = lin.into_color();
            sl += ok.l as f64;
            sa += ok.a as f64;
            sb += ok.b as f64;
            sl2 += (ok.l as f64) * (ok.l as f64);
            n += 1;
        }
        if n == 0 {
            return None;
        }
        let mean_l = (sl / n as f64) as f32;
        let mean_a = (sa / n as f64) as f32;
        let mean_b = (sb / n as f64) as f32;
        let var = (sl2 / n as f64) - (sl / n as f64).powi(2);
        let l_std = (var.max(0.0) as f64).sqrt() as f32;
        Some((mean_l, mean_a, mean_b, l_std))
    }

    /// 智能 · 自动曝光：把平均亮度拉到 ~0.5 中间调。
    fn auto_exposure(&mut self) {
        if let Some((mean_l, _, _, _)) = self.analyze() {
            let ev = (0.5f32 / mean_l.max(1e-3)).log2().clamp(-2.0, 2.0);
            self.adj.exposure_ev = ev;
            self.dirty = true;
            self.status = format!("自动曝光 → EV {:.2}", ev);
        } else {
            self.status = "请先打开图片".into();
        }
    }

    /// 智能 · 自动白平衡：用平均 OKLab a/b 反向抵消整体色偏。
    fn auto_wb(&mut self) {
        if let Some((_, a, b, _)) = self.analyze() {
            self.adj.white_balance.temp = (-a * 3.0).clamp(-1.0, 1.0);
            self.adj.white_balance.tint = (-b * 3.0).clamp(-1.0, 1.0);
            self.dirty = true;
            self.status = format!(
                "自动白平衡 (色温 {:.2} / 色调 {:.2})",
                self.adj.white_balance.temp, self.adj.white_balance.tint
            );
        } else {
            self.status = "请先打开图片".into();
        }
    }

    /// 智能 · 智能去雾：低对比（雾化）图自动加去雾。
    fn auto_dehaze(&mut self) {
        if let Some((_, _, _, l_std)) = self.analyze() {
            let d = ((0.16 - l_std) * 5.0).clamp(0.0, 0.6);
            self.adj.grade.dehaze = d;
            self.dirty = true;
            self.status = format!("智能去雾 → {:.2}", d);
        } else {
            self.status = "请先打开图片".into();
        }
    }

    /// 一键中性（纯算法，零 key，本地闭环）：把图修到健康中性影调，不过曝。
    fn start_local_auto(&mut self) {
        if self.auto_running {
            return;
        }
        let Some(src) = self.src.clone() else {
            self.status = "请先打开图片".into();
            return;
        };
        let res = Arc::clone(&self.auto_result);
        let strength = self.neutral_strength;
        self.auto_mode = AutoMode::Neutral;
        self.auto_running = true;
        self.status = format!("一键中性修图中（力度 {:.1}，本地，零 key）…", strength);
        std::thread::spawn(move || {
            let (_img, result) = run_auto(&src, 1024, 2, strength);
            if let Ok(mut guard) = res.lock() {
                *guard = Some(result);
            }
        });
    }

    /// 参考图匹配（纯算法）：把当前图影调朝已导入的参考图靠拢。
    fn start_reference_match(&mut self) {
        if self.auto_running {
            return;
        }
        let (src, ref_m) = (self.src.clone(), self.ref_metrics.clone());
        let Some(src) = src else {
            self.status = "请先打开图片".into();
            return;
        };
        let Some(ref_m) = ref_m else {
            self.status = "请先导入一张参考图".into();
            return;
        };
        let res = Arc::clone(&self.auto_result);
        let match_strength = self.match_strength;
        self.auto_mode = AutoMode::Reference;
        self.auto_running = true;
        self.status = "参考图影调匹配中…".into();
        std::thread::spawn(move || {
            let (_img, result) = run_reference_match(&src, &ref_m, 1024, 3, match_strength);
            if let Ok(mut guard) = res.lock() {
                *guard = Some(AutoResult {
                    metrics_before: result.metrics_before,
                    metrics_after: result.metrics_after,
                    guardrail_passed: result.guardrail_passed,
                    log: result.log,
                    applied_params: result.applied_params,
                    rounds: result.rounds,
                    ref_metrics: Some(result.ref_metrics),
                    adjustments: result.adjustments,
                });
            }
        });
    }

    /// 导入参考图：分析其指标并显示缩略图（需 ctx 建纹理）。
    fn import_reference(&mut self, ctx: &egui::Context) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("图片", &["jpg", "jpeg", "png", "tif", "tiff"])
            .pick_file()
        else {
            return;
        };
        match image::open(&path) {
            Ok(img) => {
                self.ref_metrics = Some(analyze(&img));
                self.ref_path = Some(path.clone());
                // 生成 160px 缩略图纹理，便于在面板里直观确认参考。
                let thumb = img.resize(160, 160, image::imageops::FilterType::Triangle);
                let size = [thumb.width() as usize, thumb.height() as usize];
                let rgba = thumb.into_rgba8().into_raw();
                self.ref_texture = Some(ctx.load_texture(
                    "ref_thumb",
                    egui::ColorImage::from_rgba_unmultiplied(size, &rgba),
                    egui::TextureOptions::default(),
                ));
                self.status = format!("已导入参考图 {}", path.display());
            }
            Err(e) => self.status = format!("参考图打开失败: {}", e),
        }
    }

    /// 清除已导入的参考图。
    fn clear_reference(&mut self) {
        self.ref_path = None;
        self.ref_metrics = None;
        self.ref_texture = None;
        self.status = "已清除参考图".into();
    }

    /// 生成作品名（可选联网，仅 Qwen 视觉）：为当前图起名 + 点评。
    /// 不点则不联网、零 token。
    fn generate_title(&mut self) {
        if self.src_path.is_none() {
            self.status = "请先打开图片".into();
            return;
        }
        let key = if !self.api_qwen_key.trim().is_empty() {
            self.api_qwen_key.trim().to_string()
        } else {
            std::env::var("DASHSCOPE_API_KEY").unwrap_or_default()
        };
        if key.is_empty() {
            self.status =
                "生成作品名需 Qwen(DashScope) Key：填下方「作品名设置」或 export DASHSCOPE_API_KEY"
                    .into();
            return;
        }
        let path = self.src_path.clone().unwrap();
        let metrics = self.img_metrics.clone();
        let summary = "中性校正 + 影调优化".to_string();
        let out = Arc::clone(&self.title_result);
        self.status = "正在生成作品名（Qwen 视觉）…".into();
        std::thread::spawn(move || {
            let b64 = match thumb_b64(&path, 512) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("作品名缩略图失败: {}", e);
                    return;
                }
            };
            let mjson = metrics
                .map(|m| serde_json::to_string(&m).unwrap_or_default())
                .unwrap_or_default();
            match QwenClient::new(key).review(&b64, &mjson, &summary) {
                Ok(v) => {
                    let title = v
                        .get("title")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string();
                    let comment = v
                        .get("comment")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string();
                    let text = if title.is_empty() {
                        "（未获取到作品名）".to_string()
                    } else {
                        format!("《{}》— {}", title, comment)
                    };
                    if let Ok(mut g) = out.lock() {
                        *g = Some(text);
                    }
                }
                Err(e) => eprintln!("Qwen 作品名失败: {}", e),
            }
        });
    }

    /// 每帧检查作品名异步结果，落到 last_title 与状态栏。
    fn poll_title(&mut self) {
        let got = {
            let guard = self.title_result.lock();
            match guard {
                Ok(mut g) => g.take(),
                Err(e) => e.into_inner().take(),
            }
        };
        if let Some(t) = got {
            // 状态栏只显示简短作品名，不显示长点评
            let short = if let Some(s) = t.find('》') {
                format!("作品名：{}", &t[..s + 3])
            } else {
                format!("作品名：{}", t)
            };
            self.status = short;
            self.last_title = Some(t);
        }
    }

    /// 每帧轮询后台修图结果；完成则把采用参数套用到当前 adj。
    /// 即使后台线程 panic 毒化了 Mutex，也安全恢复（poison 保护）。
    /// 把自动结果混合进当前参数：以「完整目标参数」为基底（保留 tone_map /
    /// defake / mix / hsl / light_ratio / film_curve 等保命字段），仅对 registry
    /// 标量字段（曝光/对比/白平衡/饱和等）按强度 s 从 base 向 target 插值。
    /// s=1.0 → 完全采用目标（一键中性）；s<1.0 → 部分匹配（参考图匹配）。
    fn blend_adj(base: &Adjustments, target: &Adjustments, s: f32) -> Adjustments {
        let mut a = target.clone();
        for spec in registry() {
            let b = spec.field.get(base);
            let t = spec.field.get(target);
            spec.field.set(&mut a, b + (t - b) * s);
        }
        a
    }

    /// 替换整组参数时死保用户几何（旋转/裁剪/翻转）。一键美颜、一键中性、
    /// 亮度还原、应用预设等路径都应走它，否则 `to_adjustments()`(写死 geometry 默认)
    /// / `blend_adj()`(geometry 不在注册表) 会把几何清零，导致"调别的参数旋转又回来"。
    fn replace_adj_preserve_geo(&mut self, new: Adjustments) {
        self.adj = preserve_geometry(&self.adj, new);
    }

    fn poll_auto(&mut self) {
        if !self.auto_running {
            return;
        }
        // 先提取结果 + 模式，释放 MutexGuard，避免与后续 self 可变借用冲突。
        let got = {
            let guard = self.auto_result.lock();
            match guard {
                Ok(mut g) => g.take(),
                Err(e) => e.into_inner().take(),
            }
        };
        let auto_mode = self.auto_mode;
        if let Some(result) = got {
            let auto_adj = result.adjustments;
            // 一键中性模式：存亮度基线供「还原亮度」滑块使用；滑块新修图归零。
            if auto_mode == AutoMode::Neutral {
                self.auto_baseline = Some(auto_adj.clone());
                self.exposure_restore = 0.0;
            }
            let s = if auto_mode == AutoMode::Reference {
                self.match_strength.clamp(0.0, 1.0)
            } else {
                1.0
            };
            self.replace_adj_preserve_geo(Self::blend_adj(&self.adj, &auto_adj, s));
            // 一键中性模式：按曝光还原滑块重新调整亮度（保留颜色校正）
            if auto_mode == AutoMode::Neutral {
                self.reapply_exposure_restore();
            }
            self.auto_running = false;
            self.dirty = true;
            let mode = if auto_mode == AutoMode::Reference {
                "参考图匹配"
            } else {
                "一键中性"
            };
            self.status = format!(
                "{}完成：{} 轮，护栏{}",
                mode,
                result.rounds,
                if result.guardrail_passed {
                    "通过"
                } else {
                    "取最安全候选"
                }
            );
        }
    }

    /// 曝光还原：从 auto_baseline 出发，基于原图亮度中位数与自动结果的亮度缺口，
    /// 用非线性 `cubic\_ease` 做感知均匀的亮度回退（保留颜色/白平衡不变）。
    /// 调用时机：poll_auto 存好基线后 / exposure_restore 滑块变化时。
    fn reapply_exposure_restore(&mut self) {
        let Some(ref baseline) = self.auto_baseline.clone() else {
            return;
        };
        let r = self.exposure_restore.clamp(0.0, 1.0);
        if r <= 0.0 {
            self.replace_adj_preserve_geo(baseline.clone());
            self.dirty = true;
            return;
        }
        // cubic ease：人眼感知均匀的缓入缓出，避免前半程太猛、后半程太弱
        let r3 = r * r * (3.0 - 2.0 * r);
        // 按原图影调细分回退强度：低调(暗)→保住暗部氛围弱回退；常规→适中；高调(亮)→强回退
        let orig_med = self
            .img_metrics
            .as_ref()
            .map(|m| m.tone.median_l)
            .unwrap_or(0.5);
        let style_weight = if orig_med < 0.38 {
            0.45 // 低调：暗是氛围，弱回退
        } else if orig_med > 0.58 {
            0.88 // 高调：引擎可能提过头，强回退
        } else {
            0.65 // 中调：适中
        };
        let mul = (1.0 - r3 * style_weight).max(0.0);

        let mut a = baseline.clone();
        a.exposure_ev *= mul;
        a.grade.contrast *= mul.max(0.3); // 对比至少保留 30%
        a.grade.film_curve *= (1.0 - r3 * style_weight * 0.7).max(0.0);
        a.grade.dehaze *= (1.0 - r3 * style_weight * 0.6).max(0.0);
        a.grade.light_ratio *= (1.0 - r3 * style_weight * 0.6).max(0.0);
        a.grade.shadow_lift *= (1.0 - r3 * style_weight * 0.8).max(0.0);
        a.grade.deep_shadow_lift *= (1.0 - r3 * style_weight * 0.8).max(0.0);
        // 颜色/白平衡/色调映射等从基线保留不变
        self.replace_adj_preserve_geo(a);
        self.dirty = true;
    }

    /// Open the export dialog (toggles show_export).
    fn save(&mut self) {
        if self.src.is_none() {
            self.status = "请先打开图片再保存".into();
            return;
        }
        self.show_export = true;
    }

    /// Render the export dialog window (shown on demand).
    fn show_export_dialog(&mut self, ctx: &egui::Context) {
        let mut open = true;
        egui::Window::new("导出 / 保存")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                use retouch_core::export::*;
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 4.0); // 紧凑版
                ui.vertical(|ui| {
                    // ── 目标尺寸 ──
                    ui.label(egui::RichText::new("目标尺寸").strong());
                    ui.horizontal(|ui| {
                        let all = TargetSize::all_presets();
                        for ts in &all {
                            let sel = self.export_cfg.target_size.long_edge() == ts.long_edge();
                            if ui.selectable_label(sel, ts.label()).clicked() {
                                self.export_cfg.target_size = ts.clone();
                            }
                        }
                        if let TargetSize::Custom(v) = &mut self.export_cfg.target_size {
                            ui.add(
                                egui::DragValue::new(v)
                                    .range(100..=10000)
                                    .speed(10.0)
                                    .suffix(" px"),
                            );
                        } else if ui.button("自定义").clicked() {
                            self.export_cfg.target_size = TargetSize::Custom(3000);
                        }
                    });

                    ui.add_space(6.0);

                    // ── 输出格式 + JPEG 质量（同行） ──
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("格式").strong());
                        let fmts = [OutputFormat::Jpeg, OutputFormat::Png];
                        for fmt in &fmts {
                            let sel = std::mem::discriminant(&self.export_cfg.output_format)
                                == std::mem::discriminant(fmt);
                            if ui.selectable_label(sel, fmt.label()).clicked() {
                                self.export_cfg.output_format = *fmt;
                            }
                        }
                        if self.export_cfg.output_format == OutputFormat::Jpeg {
                            ui.add_space(8.0);
                            ui.label("JPEG");
                            ui.add(
                                egui::Slider::new(&mut self.export_cfg.quality, 50..=100).text(""),
                            );
                        }
                    });

                    ui.add_space(6.0);

                    // ── 边框 ──
                    ui.label(egui::RichText::new("边框").strong());
                    ui.horizontal(|ui| {
                        let borders = [
                            ("无", BorderStyle::None),
                            ("白边", BorderStyle::White { width_ratio: 0.03 }),
                            ("宝丽来", BorderStyle::Polaroid { width_ratio: 0.03 }),
                        ];
                        for (name, style) in &borders {
                            let curr = &self.export_cfg.border;
                            let is_cur = matches!(
                                (curr, style),
                                (BorderStyle::None, BorderStyle::None)
                                    | (BorderStyle::White { .. }, BorderStyle::White { .. })
                                    | (BorderStyle::Polaroid { .. }, BorderStyle::Polaroid { .. })
                            );
                            if ui.selectable_label(is_cur, *name).clicked() {
                                self.export_cfg.border = style.clone();
                            }
                        }
                    });
                    if !matches!(self.export_cfg.border, BorderStyle::None) {
                        if let BorderStyle::White { width_ratio }
                        | BorderStyle::Polaroid { width_ratio } = &mut self.export_cfg.border
                        {
                            let mut pct = (*width_ratio * 100.0) as i32;
                            ui.horizontal(|ui| {
                                ui.add(egui::Slider::new(&mut pct, 1..=15).text("宽度"));
                                ui.label(format!("{}%", pct));
                            });
                            *width_ratio = pct as f32 / 100.0;
                            let mut has_round = self.export_cfg.border_round.is_some();
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut has_round, "内角圆角");
                                if let Some(r) = &mut self.export_cfg.border_round {
                                    if has_round {
                                        ui.add(egui::Slider::new(r, 2.0..=30.0).text(""));
                                    }
                                }
                            });
                            if has_round && self.export_cfg.border_round.is_none() {
                                self.export_cfg.border_round = Some(8.0);
                            } else if !has_round {
                                self.export_cfg.border_round = None;
                            }
                        }
                    }

                    ui.add_space(4.0);

                    // ── 其他选项（紧凑） ──
                    ui.checkbox(&mut self.export_cfg.smart_sharpen, "智能锐化")
                        .on_hover_text("缩图后自适应锐化：人像护肤色，风景强纹理，纯色低强度");
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("DPI:{}", self.export_cfg.dpi))
                                .size(12.0)
                                .color(egui::Color32::from_gray(140)),
                        );
                        ui.label(
                            egui::RichText::new("sRGB")
                                .size(12.0)
                                .color(egui::Color32::from_gray(140)),
                        );
                    });

                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(4.0);

                    // ── 保存按钮 + 取消（同行） ──
                    ui.horizontal(|ui| {
                        if ui
                            .add(egui::Button::new("保存").min_size(egui::vec2(120.0, 32.0)))
                            .clicked()
                        {
                            let ext = self.export_cfg.output_format.ext();
                            let default_name = {
                                if let Some(t) = &self.last_title {
                                    let start = t.find('《').map(|i| i + 3).unwrap_or(0);
                                    let end = t.find('》').unwrap_or(t.len());
                                    let title = &t[start..end.min(t.len())];
                                    title.trim().to_string()
                                } else if let Some(p) = &self.src_path {
                                    p.file_stem()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .to_string()
                                } else {
                                    "photo".to_string()
                                }
                            };
                            let file_name = format!("{}.{}", default_name, ext);
                            let mut dialog = rfd::FileDialog::new()
                                .set_title("选择保存位置")
                                .set_file_name(&file_name)
                                .add_filter(self.export_cfg.output_format.label(), &[ext]);
                            if let Some(dir) = &self.last_save_dir {
                                dialog = dialog.set_directory(dir);
                            }
                            if let Some(path) = dialog.save_file() {
                                if let Some(parent) = path.parent() {
                                    self.last_save_dir = Some(parent.to_path_buf());
                                }
                                self.status = self.do_export(&path);
                                self.show_export = false;
                            }
                        }
                        if ui.button("取消").clicked() {
                            self.show_export = false;
                        }
                    });
                });
            });
        if !open {
            self.show_export = false;
        }
    }

    /// Execute the full export pipeline (render + resize + sharpen + border +
    /// sRGB + EXIF + DPI) and write to disk. Returns a status message.
    fn do_export(&self, path: &std::path::Path) -> String {
        let src = match &self.src {
            Some(s) => s,
            None => return "错误: 无源图".into(),
        };

        let data = retouch_core::export::export_image(
            src,
            &self.adj,
            &self.export_cfg,
            self.src_path.as_deref(),
            self.spot.as_ref(),
        );
        match std::fs::write(path, &data) {
            Ok(()) => {
                format!(
                    "✅ 已导出 -> {}（{} KB）",
                    path.display(),
                    data.len() / 1024
                )
            }
            Err(e) => format!("❌ 保存失败: {}", e),
        }
    }

    /// 导入（多选）：相册批处理的入口。选 1..N 张，仅生成 ≤512px 缩略图，
    /// 不全解码原图进内存。导入即用「上一张参数」作起点（首张用照片默认）。
    fn open(&mut self) {
        let mut dialog =
            rfd::FileDialog::new().add_filter("图片", &["jpg", "jpeg", "png", "tif", "tiff"]);
        if let Some(dir) = &self.last_open_dir {
            dialog = dialog.set_directory(dir);
        }
        if let Some(paths) = dialog.pick_files() {
            if let Some(parent) = paths.first().and_then(|p| p.parent()) {
                self.last_open_dir = Some(parent.to_path_buf());
            }
            self.import_paths(paths);
        }
    }

    /// 把一组路径导入为相册：每张仅解码到 ≤512px 缩略图，参数默认沿用上一张。
    /// 异步导入：缩略图解码放到后台线程流式回传，主线程 `poll_import` 逐张追加，
    /// 界面全程不冻结，并显示"导入中 x/n"进度。第一张到达即载入活跃图。
    fn import_paths(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        // 防重复触发：上一轮导入未结束时忽略新请求。
        if self.import_rx.is_some() {
            self.status = "正在导入，请稍候…".into();
            return;
        }
        if paths.len() > 50 {
            self.status = format!(
                "已选 {} 张，超出 50 张上限，仅导入前 50 张（其余请分批）",
                paths.len()
            );
        }
        let capped: Vec<PathBuf> = paths.into_iter().take(50).collect();
        let total = capped.len();
        // 起点参数：沿用当前工作 adj（若已有图），否则照片默认。
        self.import_base_adj = if self.album.is_empty() {
            Adjustments::photo_default()
        } else {
            self.adj.clone()
        };
        // 清空相册，准备流式导入。
        self.album = Album {
            slots: Vec::new(),
            active_idx: 0,
        };
        self.import_total = total;
        self.import_done = 0;
        self.status = format!("导入中 0/{}…", total);
        let (tx, rx) = channel::<ImportMsg>();
        self.import_rx = Some(rx);
        std::thread::spawn(move || {
            for path in capped {
                // 仅解码到 ≤512px 缩略图，不全解码原图。
                let thumb = image::open(&path).ok().map(|img| {
                    let (w, h) = img.dimensions();
                    let scale = (512.0 / w.max(h) as f32).min(1.0);
                    let tw = (w as f32 * scale) as u32;
                    let th = (h as f32 * scale) as u32;
                    Arc::new(img.resize(tw, th, image::imageops::FilterType::Triangle))
                });
                if tx.send(ImportMsg::Item { path, thumb }).is_err() {
                    return; // 主线程已丢弃接收端（极少见），线程安静退出。
                }
            }
            let _ = tx.send(ImportMsg::Done);
        });
    }

    /// 每帧轮询后台导入结果：把到达的缩略图逐张追加进相册并更新进度。
    fn poll_import(&mut self) {
        // 先把本帧到达的消息 drain 到本地，避免与后续可变借用冲突。
        let mut items: Vec<(PathBuf, Option<Arc<image::DynamicImage>>)> = Vec::new();
        let mut done = false;
        if let Some(rx) = &self.import_rx {
            loop {
                match rx.try_recv() {
                    Ok(ImportMsg::Item { path, thumb }) => items.push((path, thumb)),
                    Ok(ImportMsg::Done) => {
                        done = true;
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        done = true;
                        break;
                    }
                }
            }
        } else {
            return;
        }
        for (path, thumb) in items {
            let was_empty = self.album.slots.is_empty();
            self.album
                .slots
                .push(Slot::new(path, thumb, self.import_base_adj.clone()));
            self.import_done += 1;
            if was_empty {
                // 第一张到达即载入，用户立刻看到图（异步全解码）。
                // 对齐工作副本到起点参数，避免 switch_to 存回时污染 slot0。
                self.adj = self.import_base_adj.clone();
                self.spot = None;
                self.last_title = None;
                self.switch_to(0);
            }
        }
        if self.import_total > 0 && !done {
            self.status = format!("导入中 {}/{}…", self.import_done, self.import_total);
        }
        if done {
            self.import_rx = None;
            self.status = format!("已导入 {} 张（上限 50）", self.album.slots.len());
        }
    }

    /// 保存当前工作副本到「原活跃 slot」，载入目标 slot 到工作副本
    /// （参数/污点/作品名各自独立）。这是相册「逐张独立 + 切换沿用上一张」的核心。
    fn switch_to(&mut self, idx: usize) {
        if self.album.is_empty() {
            return;
        }
        let idx = idx.min(self.album.slots.len() - 1);
        // 1) 落回当前工作副本到原 slot。
        if let Some(slot) = self.album.slots.get_mut(self.album.active_idx) {
            slot.adj = self.adj.clone();
            slot.spot = self.spot.clone();
            slot.title = self.last_title.clone();
        }
        self.album.active_idx = idx;
        // 2) 载入目标 slot 到工作副本（参数/污点等轻量状态瞬时切换）。
        if let Some(slot) = self.album.slots.get(idx) {
            let path = slot.path.clone();
            self.adj = slot.adj.clone();
            self.spot = slot.spot.clone();
            // 同步档位显示：取该图污点层的算法档位（无污点则用当前默认）。
            self.heal_mode = slot.spot.as_ref().map_or(self.heal_mode, |s| s.mode);
            self.last_title = slot.title.clone();
            // 清后台修图状态。
            self.auto_running = false;
            if let Ok(mut guard) = self.auto_result.lock() {
                *guard = None;
            }
            self.auto_baseline = None;
            self.exposure_restore = 0.0;
            // 原图全解码放到后台线程（切图不再冻结界面），带 gen 令牌防竞态。
            self.spawn_load(path);
        }
    }

    /// 后台全分辨率解码：切图/打开统一入口。瞬时清掉旧预览显示"载入中…"，
    /// 解码 + 指标分析在线程里完成，带 gen 令牌——poll 只采纳最新一次的结果，
    /// 用户快速连点缩略图时旧结果自动丢弃，绝不错图。
    fn spawn_load(&mut self, path: PathBuf) {
        self.load_gen = self.load_gen.wrapping_add(1);
        let gen = self.load_gen;
        self.loading = true;
        self.src = None;
        self.texture = None; // 清旧预览，画布显示"载入中…"占位
        self.base_rgba = None;
        self.before_rgba = None;
        self.dirty = false;
        self.dirty_geo = false;
        self.src_path = Some(path.clone());
        self.status = "载入中…".into();
        let tx = self.load_tx.clone();
        std::thread::spawn(move || {
            let result = match image::open(&path) {
                Ok(img) => {
                    let metrics = analyze(&img);
                    Ok((img, metrics))
                }
                Err(e) => Err(e.to_string()),
            };
            let _ = tx.send(LoadMsg { gen, path, result });
        });
    }

    /// 每帧轮询后台解码结果：只采纳与当前 load_gen 一致的最新结果，其余丢弃。
    fn poll_load(&mut self) {
        loop {
            match self.load_rx.try_recv() {
                Ok(msg) => {
                    if msg.gen != self.load_gen {
                        continue; // 过期结果（用户已切到别的图），直接丢弃
                    }
                    self.loading = false;
                    match msg.result {
                        Ok((img, metrics)) => {
                            self.img_metrics = Some(metrics);
                            self.src = Some(img);
                            self.src_path = Some(msg.path.clone());
                            self.dirty = true;
                            self.base_rgba = None;
                            self.before_rgba = None;
                            self.dirty_geo = false;
                            self.status = format!("已打开 {}", msg.path.display());
                        }
                        Err(e) => {
                            self.src = None;
                            self.status = format!("打开失败: {}", e);
                        }
                    }
                }
                Err(_) => break,
            }
        }
    }

    /// 智能美肤 A（零模型）：一键粉嫩肤色 + 温和频谱磨皮，按强度缩放。
    /// 仅并入 skin + advanced.freqsep 两个字段，其余修图不动；
    /// 眼唇/背景由 skin_prob 天然的 skin 低概率保护（无需额外代码）。
    fn apply_smart_beauty(&mut self, strength: f32) {
        let s = strength.clamp(0.0, 1.0);
        let mut a = smart_beauty_preset();
        let k = 0.4 + 0.6 * s; // 0.4..1.0 区间缩放，最弱不重、最强不糊
        a.skin.strength = (a.skin.strength * k).clamp(0.0, 1.0);
        a.advanced.freqsep.strength = (a.advanced.freqsep.strength * k).clamp(0.0, 1.0);
        self.adj.skin = a.skin;
        self.adj.advanced.freqsep = a.advanced.freqsep;
        self.dirty = true;
        self.status = format!("智能美肤 A（强度 {}%）", (s * 100.0) as i32);
    }

    /// 批量导出：遍历选中 slot，各自解原图→apply(adj)→inpaint(spot)→写盘。
    /// 单张失败不中断，末了报告成功/失败数。文件名：作品名优先→源文件名→冲突加序号。
    fn batch_export(&mut self, dir: PathBuf) {
        if self.album.is_empty() {
            self.status = "相册为空，先导入图片".into();
            return;
        }
        // 防重复触发。
        if self.export_rx.is_some() {
            self.status = "正在导出，请稍候…".into();
            return;
        }
        let cfg = self.export_cfg.clone();
        let ext = cfg.output_format.ext().to_string();
        // 主线程预生成 job（含去重文件名），把重活（解码/导出/写盘）交给后台线程。
        let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut jobs: Vec<ExportJob> = Vec::new();
        let n = self.album.slots.len();
        for idx in 0..n {
            let slot = &self.album.slots[idx];
            if !slot.selected {
                continue;
            }
            let base = self.album.base_name(idx);
            let mut name = base.clone();
            let mut counter = 1;
            while used.contains(&format!("{}.{}", name, ext)) {
                name = format!("{}_{}", base, counter);
                counter += 1;
            }
            used.insert(format!("{}.{}", name, ext));
            jobs.push(ExportJob {
                path: slot.path.clone(),
                adj: slot.adj.clone(),
                spot: slot.spot.clone(),
                out_path: dir.join(format!("{}.{}", name, ext)),
            });
        }
        let total = jobs.len();
        if total == 0 {
            self.status = "没有选中的图片（勾选缩略图前的复选框）".into();
            return;
        }
        self.export_total = total;
        self.export_done = 0;
        self.status = format!("导出中 0/{}…", total);
        let (tx, rx) = channel::<ExportMsg>();
        self.export_rx = Some(rx);
        std::thread::spawn(move || {
            let mut ok = 0usize;
            let mut fail = 0usize;
            for (i, job) in jobs.into_iter().enumerate() {
                // 单张 catch_unwind：任何一张的极端参数/写盘异常都不拖垮整批。
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                    || -> Result<(), String> {
                        let src = image::open(&job.path).map_err(|e| e.to_string())?;
                        let data = retouch_core::export::export_image(
                            &src,
                            &job.adj,
                            &cfg,
                            Some(&job.path),
                            job.spot.as_ref(),
                        );
                        std::fs::write(&job.out_path, &data).map_err(|e| e.to_string())?;
                        Ok(())
                    },
                ));
                match res {
                    Ok(Ok(())) => ok += 1,
                    _ => fail += 1,
                }
                if tx
                    .send(ExportMsg::Step {
                        done: i + 1,
                        ok,
                        fail,
                    })
                    .is_err()
                {
                    return;
                }
            }
            let _ = tx.send(ExportMsg::Done { ok, fail });
        });
    }

    /// 每帧轮询批量导出进度：更新"导出中 x/n"，完成后报告成功/失败数。
    fn poll_export(&mut self) {
        let mut finished: Option<(usize, usize)> = None;
        if let Some(rx) = &self.export_rx {
            loop {
                match rx.try_recv() {
                    Ok(ExportMsg::Step { done, ok, fail }) => {
                        self.export_done = done;
                        self.status = format!(
                            "导出中 {}/{}（成功 {} / 失败 {}）…",
                            done, self.export_total, ok, fail
                        );
                    }
                    Ok(ExportMsg::Done { ok, fail }) => {
                        finished = Some((ok, fail));
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        finished = Some((self.export_done, 0));
                        break;
                    }
                }
            }
        } else {
            return;
        }
        if let Some((ok, fail)) = finished {
            self.export_rx = None;
            self.status = format!("批量导出完成：成功 {} / 失败 {}", ok, fail);
        }
    }

    /// 是否有后台重任务在跑（用于显示 spinner 与持续请求重绘）。
    fn is_busy(&self) -> bool {
        self.import_rx.is_some()
            || self.export_rx.is_some()
            || self.loading
            || self.render_pending
            || self.auto_running
    }

    /// 统一的图片加载入口：文件对话框与拖拽都走这里，避免重复逻辑。
    /// 加载成功后刷新指标、标记脏、清空后台智能修图状态。
    fn load_image(&mut self, path: &std::path::Path) {
        // 清后台修图状态，然后异步全解码（不冻结界面）。
        self.auto_running = false;
        if let Ok(mut guard) = self.auto_result.lock() {
            *guard = None;
        }
        self.auto_baseline = None;
        self.exposure_restore = 0.0;
        self.spawn_load(path.to_path_buf());
    }

    fn load_preset_file(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("TOML 预设", &["toml"])
            .pick_file()
        {
            match load_preset(&path) {
                Ok(p) => {
                    self.replace_adj_preserve_geo(p.to_adjustments());
                    self.dirty = true;
                    self.dirty_geo = true;
                    self.status = format!("已加载预设 {}", path.display());
                }
                Err(e) => self.status = e,
            }
        }
    }

    /// Save the *current* parameters as a TOML preset the user can re-load later.
    /// This is what makes 预设 actually useful (previously only loading worked).
    fn save_preset_file(&mut self) {
        let mut dlg = rfd::FileDialog::new()
            .add_filter("TOML 预设", &["toml"])
            .set_file_name("我的预设.toml");
        if let Some(dir) = &self.last_save_dir {
            dlg = dlg.set_directory(dir);
        }
        if let Some(path) = dlg.save_file() {
            let p: Preset = self.adj.to_preset();
            self.status = match dump_preset(&p, &path) {
                Ok(()) => {
                    if let Some(parent) = path.parent() {
                        self.last_save_dir = Some(parent.to_path_buf());
                    }
                    format!("已保存预设 -> {}", path.display())
                }
                Err(e) => e,
            };
        }
    }

    /// Parse and run a single command-palette command. Returns true if the
    /// palette should close.
    fn run_command(&mut self, ctx: &egui::Context, line: &str) -> bool {
        let t: Vec<&str> = line.split_whitespace().collect();
        if t.is_empty() {
            return true;
        }
        match t[0].to_ascii_lowercase().as_str() {
            "open" => self.open(),
            "save" => self.save(),
            "preset" => self.load_preset_file(),
            "factory" => {
                self.adj = Adjustments::photo_default();
                self.dirty = true;
                self.status = "已应用照片默认".into();
            }
            "reset" => {
                self.adj = Adjustments::default();
                self.dirty = true;
                self.status = "已重置 (恒等)".into();
            }
            "dump" => {
                let p: Preset = self.adj.to_preset();
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("TOML", &["toml"])
                    .save_file()
                {
                    self.status = match dump_preset(&p, &path) {
                        Ok(()) => format!("已导出 -> {}", path.display()),
                        Err(e) => e,
                    };
                }
            }
            "exposure" | "exp" => {
                self.status =
                    Self::set_f32(&mut self.adj.exposure_ev, t.get(1).copied(), -3.0, 3.0);
                self.dirty = true;
            }
            "contrast" => {
                self.status =
                    Self::set_f32(&mut self.adj.grade.contrast, t.get(1).copied(), -1.0, 1.0);
                self.dirty = true;
            }
            "brightness" => {
                self.status = Self::set_f32(
                    &mut self.adj.grade.brightness_lift,
                    t.get(1).copied(),
                    0.0,
                    1.0,
                );
                self.dirty = true;
            }
            "dehaze" => {
                self.status =
                    Self::set_f32(&mut self.adj.grade.dehaze, t.get(1).copied(), 0.0, 1.0);
                self.dirty = true;
            }
            "shadow" => {
                self.status =
                    Self::set_f32(&mut self.adj.grade.shadow_lift, t.get(1).copied(), 0.0, 1.0);
                self.dirty = true;
            }
            "deepshadow" => {
                self.status = Self::set_f32(
                    &mut self.adj.grade.deep_shadow_lift,
                    t.get(1).copied(),
                    0.0,
                    1.0,
                );
                self.dirty = true;
            }
            "wb" | "temp" => {
                self.status = Self::set_f32(
                    &mut self.adj.white_balance.temp,
                    t.get(1).copied(),
                    -1.0,
                    1.0,
                );
                self.dirty = true;
            }
            "tint" => {
                self.status = Self::set_f32(
                    &mut self.adj.white_balance.tint,
                    t.get(1).copied(),
                    -1.0,
                    1.0,
                );
                self.dirty = true;
            }
            "sat" | "saturation" => {
                self.status =
                    Self::set_f32(&mut self.adj.color.saturation, t.get(1).copied(), 0.0, 3.0);
                self.dirty = true;
            }
            "vibrance" => {
                self.status =
                    Self::set_f32(&mut self.adj.color.vibrance, t.get(1).copied(), -1.0, 1.0);
                self.dirty = true;
            }
            "hue" | "huerotate" => {
                self.status = Self::set_f32(
                    &mut self.adj.color.hue_rotate,
                    t.get(1).copied(),
                    -180.0,
                    180.0,
                );
                self.dirty = true;
            }
            "splitshadow" => {
                self.status = Self::set_f32(
                    &mut self.adj.color.split_shadow,
                    t.get(1).copied(),
                    -180.0,
                    180.0,
                );
                self.dirty = true;
            }
            "splithighlight" => {
                self.status = Self::set_f32(
                    &mut self.adj.color.split_highlight,
                    t.get(1).copied(),
                    -180.0,
                    180.0,
                );
                self.dirty = true;
            }
            "skin" | "粉嫩" => {
                self.adj.skin = SkinTone::pink();
                self.dirty = true;
                self.status = "已应用粉嫩肤色".into();
            }
            "localauto" | "本地一键" => {
                self.start_local_auto();
                self.dirty = true;
            }
            "auto" | "autoall" | "全智能" => {
                self.auto_exposure();
                self.auto_wb();
                self.auto_dehaze();
            }
            "autoexposure" | "ae" => self.auto_exposure(),
            "autowb" => self.auto_wb(),
            "autodehaze" => self.auto_dehaze(),
            "film" | "胶片" => {
                self.status = Self::set_f32(
                    &mut self.adj.grade.film_curve,
                    t.get(1).copied(),
                    -0.25,
                    0.35,
                );
                self.dirty = true;
            }
            "lightratio" | "ratio" | "光比" => {
                self.status = Self::set_f32(
                    &mut self.adj.grade.light_ratio,
                    t.get(1).copied(),
                    -0.6,
                    0.6,
                );
                self.dirty = true;
            }
            "zone" => {
                if t.len() >= 3 {
                    let idx = match t[1] {
                        "shadows" => Some(0),
                        "dark_mid" => Some(1),
                        "light_mid" => Some(2),
                        "highlights" => Some(3),
                        _ => None,
                    };
                    match idx {
                        Some(i) => match t[2].parse::<f32>() {
                            Ok(v) => {
                                self.adj.zones.lift[i] = v.clamp(-0.4, 0.4);
                                self.dirty = true;
                                self.status =
                                    format!("分区 {} → {:.2}", t[1], self.adj.zones.lift[i]);
                            }
                            Err(_) => self.status = "数值无效".into(),
                        },
                        None => {
                            self.status = "未知分区 (shadows|dark_mid|light_mid|highlights)".into()
                        }
                    }
                } else {
                    self.status = "用法: zone <分区> <值>".into();
                }
            }
            "rotate" => {
                self.status = Self::set_f32(
                    &mut self.adj.geometry.rotate_deg,
                    t.get(1).copied(),
                    -180.0,
                    180.0,
                );
                self.dirty = true;
            }
            "flip" => {
                // flip v -> 垂直翻转；否则默认水平翻转（不再同时翻两轴）。
                if t.get(1)
                    .map(|s| s.eq_ignore_ascii_case("v"))
                    .unwrap_or(false)
                {
                    self.adj.geometry.flip_v = !self.adj.geometry.flip_v;
                    self.status = "已垂直翻转".into();
                } else {
                    self.adj.geometry.flip_h = !self.adj.geometry.flip_h;
                    self.status = "已水平翻转".into();
                }
                self.dirty = true;
            }
            "crop" => {
                if t.len() >= 5 {
                    match (
                        t[1].parse::<f32>(),
                        t[2].parse::<f32>(),
                        t[3].parse::<f32>(),
                        t[4].parse::<f32>(),
                    ) {
                        (Ok(a), Ok(b), Ok(c), Ok(d)) => {
                            self.adj.geometry.crop = Some((
                                a.clamp(0.0, 1.0),
                                b.clamp(0.0, 1.0),
                                c.clamp(0.0, 1.0),
                                d.clamp(0.0, 1.0),
                            ));
                            self.dirty = true;
                            self.status = format!("裁剪 {:?}", self.adj.geometry.crop.unwrap());
                        }
                        _ => self.status = "数值无效".into(),
                    }
                } else {
                    self.status = "用法: crop <x> <y> <w> <h>".into();
                }
            }
            "denoise" => {
                self.status =
                    Self::set_f32(&mut self.adj.detail.denoise, t.get(1).copied(), 0.0, 1.0);
                self.dirty = true;
            }
            "sharpen" => {
                self.status =
                    Self::set_f32(&mut self.adj.detail.sharpen, t.get(1).copied(), 0.0, 1.0);
                self.dirty = true;
            }
            "diffuse" | "柔光" => {
                self.status =
                    Self::set_f32(&mut self.adj.detail.diffuse, t.get(1).copied(), 0.0, 1.0);
                self.dirty = true;
            }
            "freqsep" | "磨皮" => {
                if let Some(v) = t.get(1).and_then(|s| s.parse::<f32>().ok()) {
                    let v = v.clamp(0.0, 1.0);
                    self.adj.advanced.freqsep.strength = v;
                    self.adj.advanced.freqsep.enabled = v > 0.0;
                    self.dirty = true;
                    self.status = format!("频谱磨皮 → {:.2}", v);
                } else {
                    self.status = "用法: freqsep <0..1>".into();
                }
            }
            "pyramid" | "金字塔" => {
                // 金字塔融合已从 UI 移除：容易把整图糊掉。保留命令但提示废弃。
                self.status = "金字塔融合已移除：请用多分区亮度融合/柔光替代".into();
            }
            "hsl" => self.cmd_hsl(&t),
            "help" | "?" => {
                self.status = "命令: open 打开 | save 保存 | preset 加载预设 | factory 照片默认 | reset 重置 | dump 导出 | exposure <v> 曝光 | contrast <v> 对比 | brightness <v> 提亮 | dehaze <v> 去雾 | shadow <v> 暗部 | wb <v> 色温 | tint <v> 色调 | sat <v> 饱和度 | vibrance <v> 鲜艳度 | hue <v> 色相 | splitshadow <v> 暗部染色 | splithighlight <v> 高光染色 | skin 粉嫩 | auto 全智能 | localauto 本地一键修图 | apiauto 联网一键修图 | film <v> 胶片 | ratio <v> 光比 | zone <分区> <v> | hsl <band> <h> <s> <l> | rotate <deg> 旋转 | flip 翻转 | crop <x> <y> <w> <h> 裁剪 | denoise <v> 降噪 | sharpen <v> 锐化 | diffuse <v> 柔光 | freqsep <v> 磨皮".into();
            }
            other => self.status = format!("未知命令: {}", other),
        }
        let _ = ctx;
        true
    }

    /// Parse a single numeric token, clamp it into `field`, and return a status
    /// string. Free of `&mut self` so it can run while borrowing a single field.
    fn set_f32(field: &mut f32, v: Option<&str>, lo: f32, hi: f32) -> String {
        match v {
            Some(s) => match s.parse::<f32>() {
                Ok(x) => {
                    *field = x.clamp(lo, hi);
                    format!("已设为 {:.3}", *field)
                }
                Err(_) => format!("数值无效: {}", s),
            },
            None => "缺少数值".into(),
        }
    }

    /// `hsl <band> <hue> <sat> <light>` — mirror of the CLI `--hsl`.
    fn cmd_hsl(&mut self, t: &[&str]) {
        if t.len() < 5 {
            self.status = "用法: hsl <band> <hue> <sat> <light>".into();
            return;
        }
        let name = t[1];
        match HslRegions::band_index(name) {
            Some(i) => {
                if let (Ok(h), Ok(s), Ok(l)) = (
                    t[2].parse::<f32>(),
                    t[3].parse::<f32>(),
                    t[4].parse::<f32>(),
                ) {
                    self.adj.hsl.hue_shift[i] = h;
                    self.adj.hsl.sat_mult[i] = s.max(0.0);
                    self.adj.hsl.light_mult[i] = l.max(0.0);
                    self.dirty = true;
                    self.status = format!("已设置 hsl {}", name);
                } else {
                    self.status = "数值无效".into();
                }
            }
            None => self.status = format!("未知分区: {}", name),
        }
    }

    /// 带细线框的折叠分组，把左侧参数列表变成商业软件风格的设置面板。
    /// 每个分组一个浅色底 + 0.5px 边框的卡片，标题用 CollapsingHeader。
    /// Qwen(DashScope) Key 本地记忆：写入 `~/.retouch/qwen_key`，免每次输入。
    fn qwen_key_path() -> std::path::PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let mut p = std::path::PathBuf::from(home);
        p.push(".retouch");
        p.push("qwen_key");
        p
    }
    fn load_qwen_key() -> String {
        std::fs::read_to_string(Self::qwen_key_path())
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }
    fn save_qwen_key(key: &str) {
        if let Some(parent) = Self::qwen_key_path().parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(Self::qwen_key_path(), key.trim());
    }
    fn forget_qwen_key() {
        let _ = std::fs::remove_file(Self::qwen_key_path());
    }

    /// 可记忆展开/折叠状态的分组卡片（默认折叠用）：点击标题切换 `open`。
    fn collapsing_section_state(
        title: &str,
        open: &mut bool,
        ui: &mut egui::Ui,
        content: impl FnOnce(&mut egui::Ui),
    ) {
        let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
        let available = ui.available_width();
        ui.set_min_width(available);
        ui.set_max_width(available);
        egui::Frame::group(ui.style())
            .fill(ui.visuals().faint_bg_color)
            .stroke(egui::Stroke::new(0.5, stroke.color))
            .rounding(egui::Rounding::same(2.0))
            .inner_margin(egui::Margin::same(6.0))
            .outer_margin(egui::Margin::same(2.0))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.set_max_width(ui.available_width());
                let r = egui::CollapsingHeader::new(title)
                    .open(Some(*open))
                    .show(ui, content);
                if r.header_response.clicked() {
                    *open = !*open;
                }
            });
    }

    fn collapsing_section(
        title: &str,
        open: Option<bool>,
        ui: &mut egui::Ui,
        content: impl FnOnce(&mut egui::Ui),
    ) {
        let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
        let available = ui.available_width();
        // 强制卡片占满侧栏可用宽度，避免各分组长短不一。
        ui.set_min_width(available);
        ui.set_max_width(available);
        egui::Frame::group(ui.style())
            .fill(ui.visuals().faint_bg_color)
            .stroke(egui::Stroke::new(0.5, stroke.color))
            .rounding(egui::Rounding::same(2.0))
            .inner_margin(egui::Margin::same(6.0))
            .outer_margin(egui::Margin::same(2.0))
            .show(ui, |ui| {
                // 让 CollapsingHeader 在卡片内部也横向撑满，保持标题和展开内容对齐。
                ui.set_min_width(ui.available_width());
                ui.set_max_width(ui.available_width());
                egui::CollapsingHeader::new(title)
                    .open(open)
                    .show(ui, content);
            });
    }
    /// 顶部工具栏里一个常规按钮：文字在按钮背景中水平垂直居中。
    fn toolbar_btn(ui: &mut egui::Ui, label: &str, tooltip: &str) -> bool {
        ui.add(
            egui::Button::new(egui::RichText::new(label).size(14.0))
                .min_size(egui::vec2(0.0, 28.0))
                .rounding(8.0),
        )
        .on_hover_text(tooltip)
        .clicked()
    }

    /// 带细线框的工具栏分组：左侧弱化的分组名标签 + 按钮组，
    /// 所有文字使用同一套中文字体、无 emoji/图标，避免 fallback 字体造成高低错落。
    fn toolbar_group<F>(ui: &mut egui::Ui, label: &str, content: F)
    where
        F: FnOnce(&mut egui::Ui),
    {
        let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
        egui::Frame::group(ui.style())
            .fill(ui.visuals().faint_bg_color)
            .stroke(egui::Stroke::new(0.5, stroke.color))
            .rounding(egui::Rounding::same(2.0))
            .inner_margin(egui::Margin {
                left: 8.0,
                right: 8.0,
                top: 4.0,
                bottom: 4.0,
            })
            .outer_margin(egui::Margin::same(2.0))
            .show(ui, |ui| {
                // 整行交叉轴居中，保证标签和按钮垂直中线对齐。
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    // 分组名：纯文字标签，固定与按钮一致的高度，文字在分配空间里居中。
                    ui.add_sized(
                        egui::vec2(0.0, 28.0),
                        egui::Label::new(
                            egui::RichText::new(label)
                                .size(12.0)
                                .weak()
                                .color(ui.visuals().widgets.noninteractive.fg_stroke.color),
                        )
                        .selectable(false),
                    );
                    ui.add_space(6.0);
                    content(ui);
                });
            });
    }

    /// 按 ThemeMode 设置深色/浅色主题
    fn apply_theme(ctx: &egui::Context, mode: ThemeMode) {
        let is_dark = match mode {
            ThemeMode::Dark => true,
            ThemeMode::Light => false,
            ThemeMode::Auto => {
                // UTC+8 时间段：19:00–06:00 深色，其余浅色
                let secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let cst_hour = ((secs / 3600) % 24 + 8) % 24;
                cst_hour >= 19 || cst_hour < 6
            }
        };
        if is_dark {
            ctx.set_visuals(egui::style::Visuals::dark());
        } else {
            ctx.set_visuals(egui::style::Visuals::light());
        }
    }

    fn side_panel(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        // 一键展开/收起本帧的强制状态（None=各自记忆）。在此复制成局部值，
        // 避免闭包里再次借用 self。
        let force_open = self.force_open;

        // ═══ 污点修复控制（v0.6，仅在 Spot 模式显示）═══
        if self.tool_mode == ToolMode::Spot {
            ui.group(|ui| {
                ui.label(egui::RichText::new("污点修复画笔").strong());
                // 算法档位：传统(Telea) / 自然(频率分离) / 精修(Poisson)。
                ui.horizontal_wrapped(|ui| {
                    for (mode, label) in [
                        (HealMode::Telea, "传统"),
                        (HealMode::FreqSep, "自然"),
                        (HealMode::Poisson, "精修"),
                    ] {
                        let selected = self.heal_mode == mode;
                        if ui.selectable_label(selected, label).clicked() {
                            self.heal_mode = mode;
                            if let Some(s) = &mut self.spot {
                                s.mode = mode;
                            }
                            // 换档只需重合成（几何+污点），不必重跑颜色管线。
                            self.dirty_geo = true;
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("笔刷");
                    ui.add(egui::Slider::new(&mut self.spot_brush, 2..=50).suffix(" px"));
                });
                ui.horizontal(|ui| {
                    if ui.button("撤销一笔").clicked() {
                        if let Some(s) = &mut self.spot {
                            s.strokes.pop();
                            if s.is_empty() {
                                self.spot = None;
                            }
                        }
                        self.dirty_geo = true;
                    }
                    if ui.button("清空").clicked() {
                        self.spot = None;
                        self.dirty_geo = true;
                    }
                });
                let n = self.spot.as_ref().map_or(0, |s| s.strokes.len());
                ui.label(
                    egui::RichText::new(format!("已标记 {} 处", n))
                        .weak()
                        .size(12.0),
                );
            });
            ui.separator();
        }
        // 顶部主操作：一键中性化（纯算法、零 key）。把原先藏在「一键调色」里的
        // 中性化按钮提到最前，去掉 "retouch-rs" 字样；旁边放「智能补偿」开关。
        ui.horizontal_wrapped(|ui| {
            if ui
                .button("自动中性化")
                .on_hover_text(
                    "纯算法、零 key：按直方图影调把图修到健康中性（不过曝），\n\
                     可叠加下面的手动调整。默认力度「中」，可在底部「一键智能」切换弱/中/强。",
                )
                .clicked()
            {
                if let Some(src) = &self.src {
                    let bal = auto_neutral_balance(src, self.smart_compensation);
                    self.status = bal.summary.clone();
                    self.replace_adj_preserve_geo(bal.to_adjustments());
                    // 存亮度基线供「还原亮度」滑块使用
                    self.auto_baseline = Some(self.adj.clone());
                    self.exposure_restore = 0.0;
                    changed = true;
                } else {
                    self.status = "请先打开图片".into();
                }
            }
            ui.checkbox(&mut self.smart_compensation, "智能补偿")
                .on_hover_text(
                    "默认开启。中性化后参考原图主色、辅助色与直方图类型，\n\
                     对色彩浓度与光比做微量补偿，避免校正后变淡变平。",
                );
        });
        // 曝光还原滑块：自动中性化以后颜色好看但曝光偏亮，拖此滑块回退亮度保留颜色。
        if self.auto_baseline.is_some() {
            ui.horizontal(|ui| {
                ui.label("还原亮度");
                let old = self.exposure_restore;
                ui.add(egui::Slider::new(&mut self.exposure_restore, 0.0..=1.0).text(""));
                if (old - self.exposure_restore).abs() > 1e-4 {
                    self.reapply_exposure_restore();
                    changed = true;
                }
            });
        }
        ui.separator();

        // ═══ 智能美肤 A（v0.6，零模型）═══
        Self::collapsing_section("智能美肤", force_open, ui, |ui| {
            ui.label("一键粉嫩肤色 + 温和频谱磨皮（纯算法，零 AI）。");
            ui.horizontal(|ui| {
                ui.label("强度");
                ui.add(egui::Slider::new(&mut self.beauty_strength, 0.0..=1.0).suffix("%"));
            });
            if ui.button("一键美肤").clicked() {
                self.apply_smart_beauty(self.beauty_strength);
            }
        });
        ui.separator();

        // 图像分析：把原图量化成 OKLCH 指标，让用户"看见"影调/色偏/肤色。
        Self::collapsing_section("图像分析", force_open, ui, |ui| {
            if let Some(m) = &self.img_metrics {
                ui.label(format!(
                    "亮度 均值 {:.2}  反差 {:.2}  范围 {:.2}",
                    m.tone.mean_l, m.tone.std_l, m.dynamic_range
                ));
                ui.label(format!(
                    "色彩 平均彩度 {:.3}  主色相 {:.0}°  集中度 {:.2}",
                    m.color.mean_c, m.color.mean_h_deg, m.color.hue_peakiness
                ));
                ui.label(format!(
                    "削波 高光 {:.2}%  暗部 {:.2}%",
                    m.exposure.highlight_clip_pct, m.exposure.shadow_clip_pct
                ));
                ui.label(format!(
                    "色域外 {:.2}%  色偏强度 {:.3}",
                    m.gamut.clip_pct, m.cast.chroma
                ));
                if m.skin.ratio > 0.03 {
                    ui.label(format!(
                        "肤色 占比 {:.1}%  彩度 {:.3}  色相 {:.0}°",
                        m.skin.ratio * 100.0,
                        m.skin.mean_c,
                        m.skin.mean_h_deg
                    ));
                } else {
                    ui.label("肤色 未检出");
                }
            } else {
                ui.label("打开图片后显示量化指标");
            }
        });

        // 参考图匹配：导入喜欢的图，把当前图影调朝它靠拢（纯算法，零 AI）。
        let ctx2 = ctx.clone();
        Self::collapsing_section("参考图匹配", force_open, ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("参考图");
                ui.add_space(4.0);
                if let Some(tex) = &self.ref_texture {
                    ui.image(tex);
                } else {
                    ui.label("（未导入）");
                }
            });
            if let Some(p) = &self.ref_path {
                ui.label(format!(
                    "{}",
                    p.file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default()
                ));
            }
            ui.horizontal(|ui| {
                if ui
                    .button("导入参考图")
                    .on_hover_text("选一张你喜欢的图作为影调目标")
                    .clicked()
                {
                    self.import_reference(&ctx2);
                }
                if ui.button("清除").on_hover_text("移除参考图").clicked() {
                    self.clear_reference();
                }
                ui.add_enabled_ui(self.ref_metrics.is_some() && self.src.is_some(), |ui| {
                    if ui
                        .button("匹配影调")
                        .on_hover_text("把当前图影调朝参考图靠拢")
                        .clicked()
                    {
                        self.start_reference_match();
                    }
                });
            });
            ui.add_enabled_ui(self.ref_metrics.is_some(), |ui| {
                ui.add(
                    egui::Slider::new(&mut self.match_strength, 0.0..=1.0)
                        .text("匹配强度")
                        .fixed_decimals(1),
                );
                ui.label("强度 0=不变，1=完全贴合（默认 0.8）");
            });
        });

        // 胶片 / 风格预设（纯算法，无 AI，无额外依赖）
        Self::collapsing_section("胶片 / 风格预设", force_open, ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for preset in film_presets() {
                    if ui
                        .button(preset.name)
                        .on_hover_text(preset.description)
                        .clicked()
                    {
                        if preset.is_engine_based {
                            // 引擎基数：先跑影调感知引擎（颜色正确+影调正确），
                            // 再叠加本预设的风格增量——自适应原图而非死数值。
                            if let Some(ref src) = self.src {
                                let engine = tonal_adjustments(src, self.smart_compensation, 1.0);
                                self.replace_adj_preserve_geo(Self::blend_adj(
                                    &engine,
                                    &preset.adj,
                                    0.7,
                                ));
                            } else {
                                self.replace_adj_preserve_geo(preset.adj);
                            }
                        } else {
                            self.replace_adj_preserve_geo(preset.adj);
                        }
                        self.auto_baseline = Some(self.adj.clone());
                        self.status = format!("已应用预设：{}", preset.name);
                        changed = true;
                    }
                }
            });
            ui.add_space(4.0);
        });

        // 曝光 / 影调
        Self::collapsing_section("曝光 / 影调", force_open, ui, |ui| {
            changed |= self.param_slider(ui, Field::ExposureEv);
            let old_mode = self.adj.tone_map;
            let mut mode = match old_mode {
                ToneMapMode::None => 0,
                ToneMapMode::Agx => 1,
                ToneMapMode::Filmic => 2,
            };
            ui.horizontal(|ui| {
                ui.label("影调映射").on_hover_text(
                    "决定超过屏幕亮度范围的高光/暗部如何被压缩回 SDR。\n\
                     常见术语解释：\n\
                     • 肩部 Shoulder：高光区域被压弯下来的部分，防止过曝刺眼\n\
                     • 趾部 Toe：暗部被轻轻抬起的部分，避免死黑并保留层次\n\
                     • 中间调 Mid-tones：保留线性、最自然的亮度区间\n\
                     • 饱和度保护：映射时自动抑制高光假色（荧光蓝/洋红）\n\
                     三种模式差异：\n\
                     • 无：直接输出，推曝光时高光容易裁切发白\n\
                     • AgX：电影感映射， shoulder 更平滑，暗部偏沉，适合人像/日常\n\
                     • Filmic：经典 Hable 胶片曲线，灰阶油润、过渡柔和，适合风景/胶片感",
                );
                egui::ComboBox::from_label("")
                    .selected_text(match mode {
                        0 => "无",
                        1 => "AgX",
                        _ => "Filmic",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut mode, 0, "无")
                            .on_hover_text("关闭影调压缩。适合原图曝光已很准、不想改变反差的情况；推曝光时高光容易过曝。");
                        ui.selectable_value(&mut mode, 1, "AgX")
                            .on_hover_text("Academy Color Encoding System 启发的映射：高光 shoulder 宽、暗部 toe 沉，整体对比自然，肤色和天空过渡最不容易出假色。");
                        ui.selectable_value(&mut mode, 2, "Filmic")
                            .on_hover_text("基于 Hable Filmic 的经典胶片曲线：灰阶柔顺、亮部略带 roll-off，容易得到胶片/电影油润感。");
                    });
            });
            self.adj.tone_map = match mode {
                1 => ToneMapMode::Agx,
                2 => ToneMapMode::Filmic,
                _ => ToneMapMode::None,
            };
            changed |= self.adj.tone_map != old_mode;
        });

        // 去假色
        Self::collapsing_section("去假色", force_open, ui, |ui| {
            changed |= ui.checkbox(&mut self.adj.defake.enabled, "启用").changed();
            changed |= ui
                .add(
                    egui::Slider::new(&mut self.adj.defake.chroma_decay, 0.0..=1.0)
                        .text("亮度联动衰减"),
                )
                .changed();
            changed |= ui
                .checkbox(&mut self.adj.defake.fix_sky, "天空修正")
                .changed();
            changed |= ui
                .checkbox(&mut self.adj.defake.protect_skin, "肤色保护")
                .changed();
        });

        // 胶片感 / 光比（核心智能控制：全部走人眼感知曲线，非纯线性）
        Self::collapsing_section("胶片感 / 光比", force_open, ui, |ui| {
            changed |= self.param_slider(ui, Field::FilmCurve);
            changed |= self.param_slider(ui, Field::LightRatio);
            changed |= self.param_slider(ui, Field::BrightnessLift);
            changed |= self.param_slider(ui, Field::Contrast);
            changed |= self.param_slider(ui, Field::Dehaze);
            changed |= self.param_slider(ui, Field::ShadowLift);
            changed |= self.param_slider(ui, Field::DeepShadowLift);
        });

        // 白平衡
        Self::collapsing_section("白平衡", force_open, ui, |ui| {
            changed |= self.param_slider(ui, Field::WBTemp);
            changed |= self.param_slider(ui, Field::WBTint);
        });

        // 色彩风格
        Self::collapsing_section("色彩风格", force_open, ui, |ui| {
            changed |= self.param_slider(ui, Field::Saturation);
            changed |= self.param_slider(ui, Field::Vibrance);
            changed |= self.param_slider(ui, Field::HueRotate);
            changed |= self.param_slider(ui, Field::SplitShadow);
            changed |= self.param_slider(ui, Field::SplitHighlight);
        });

        // HSL 分区
        Self::collapsing_section("HSL 分区", force_open, ui, |ui| {
            let names = ["红", "橙", "黄", "绿", "青", "蓝", "紫", "品红"];
            for (i, name) in names.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(*name).strong());
                    ui.label("  ");
                });
                changed |= self.param_slider(ui, Field::HslHue(i));
                changed |= self.param_slider(ui, Field::HslSat(i));
                changed |= self.param_slider(ui, Field::HslLight(i));
                ui.add_space(4.0);
            }
        });

        // 粉嫩肤色：high-level 意图滑块，后台自动解析成 OKLCH 目标。
        Self::collapsing_section("粉嫩肤色", force_open, ui, |ui| {
            changed |= ui
                .checkbox(&mut self.adj.skin.enabled, "启用肤色优化")
                .changed();
            let mut skin_touched = false;
            skin_touched |= self.param_slider(ui, Field::SkinStrength);
            skin_touched |= self.param_slider(ui, Field::SkinYellowReduce);
            skin_touched |= self.param_slider(ui, Field::SkinLighten);
            skin_touched |= self.param_slider(ui, Field::SkinRedden);
            skin_touched |= self.param_slider(ui, Field::SkinPinken);
            // 动一下任意肤色滑块就自动启用，不用先勾选（默认启用体验）。
            if skin_touched {
                self.adj.skin.enabled = true;
                changed = true;
            }
            if ui
                .button("一键粉嫩")
                .on_hover_text("自动设置去黄+提亮+加粉，健康自然")
                .clicked()
            {
                self.adj.skin = SkinTone::pink();
                changed = true;
            }
        });

        // 多分区亮度融合（4 区高斯平滑融合，无硬边）
        Self::collapsing_section("多分区亮度融合", force_open, ui, |ui| {
            for i in 0..4 {
                changed |= self.param_slider(ui, Field::Zone(i));
            }
        });

        // 几何预处理（M4b）：裁剪 / 旋转 / 翻转 / 透视
        // 设计：几何是「预览显示变换」，与重型颜色管线彻底解耦。旋转/翻转/裁剪
        // 只作用在已校色的小基图上（rebuild_preview，同步、微秒级），绝不重新跑
        // OKLCH/多分区/肤色等模块；因此旋转永远不可能触发底层外部异常崩溃。
        // 所有几何改动统一路由到 dirty_geo，不碰颜色管线（不污染 self.dirty）。
        Self::collapsing_section("几何预处理", force_open, ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button("↺ 左转 90°")
                    .on_hover_text("逆时针旋转 90°（无损，可与下方微调独立叠加）")
                    .clicked()
                {
                    // 逆时针 = 3 个顺时针 quarter-turn
                    self.adj.geometry.quarter_turns = (self.adj.geometry.quarter_turns + 3) % 4;
                    self.dirty_geo = true;
                }
                if ui
                    .button("↻ 右转 90°")
                    .on_hover_text("顺时针旋转 90°（无损，可与下方微调独立叠加）")
                    .clicked()
                {
                    self.adj.geometry.quarter_turns = (self.adj.geometry.quarter_turns + 1) % 4;
                    self.dirty_geo = true;
                }
                let fh = self.adj.geometry.flip_h;
                if ui
                    .button(if fh {
                        "水平翻转 ✓"
                    } else {
                        "水平翻转"
                    })
                    .on_hover_text("镜像左右（可叠加旋转；再次点击还原）")
                    .clicked()
                {
                    self.adj.geometry.flip_h = !fh;
                    self.dirty_geo = true;
                }
                let fv = self.adj.geometry.flip_v;
                if ui
                    .button(if fv {
                        "垂直翻转 ✓"
                    } else {
                        "垂直翻转"
                    })
                    .on_hover_text("镜像上下（可叠加旋转；再次点击还原）")
                    .clicked()
                {
                    self.adj.geometry.flip_v = !fv;
                    self.dirty_geo = true;
                }
            });
            // 以下微调滑块只改几何 → 路由到 dirty_geo（不触发颜色管线）
            let mut g_changed = false;
            g_changed |= self.param_slider(ui, Field::GeomRotate);
            g_changed |= self.param_slider(ui, Field::GeomPerspV);
            g_changed |= self.param_slider(ui, Field::GeomPerspH);
            // 裁剪：直观的「上下左右」边缘裁切（替代 X/Y/W/H 数字）
            let crop = self.adj.geometry.crop.unwrap_or((0.0, 0.0, 1.0, 1.0));
            let (cx, cy, cw, ch) = crop;
            let mut left = cx;
            let mut top = cy;
            let mut right = (1.0 - (cx + cw)).max(0.0);
            let mut bottom = (1.0 - (cy + ch)).max(0.0);
            let mut r = ui.add(egui::Slider::new(&mut left, 0.0..=0.5).text("左裁"));
            r = r.on_hover_text("从左边裁掉多少，0 = 不裁");
            g_changed |= r.changed();
            let mut r = ui.add(egui::Slider::new(&mut right, 0.0..=0.5).text("右裁"));
            r = r.on_hover_text("从右边裁掉多少，0 = 不裁");
            g_changed |= r.changed();
            let mut r = ui.add(egui::Slider::new(&mut top, 0.0..=0.5).text("上裁"));
            r = r.on_hover_text("从上面裁掉多少，0 = 不裁");
            g_changed |= r.changed();
            let mut r = ui.add(egui::Slider::new(&mut bottom, 0.0..=0.5).text("下裁"));
            r = r.on_hover_text("从下面裁掉多少，0 = 不裁");
            g_changed |= r.changed();
            // 四边相加不能超过画面，留出至少 5% 余量
            let sum_lr = left + right;
            let sum_tb = top + bottom;
            if sum_lr > 0.95 {
                let k = 0.95 / sum_lr;
                left *= k;
                right *= k;
            }
            if sum_tb > 0.95 {
                let k = 0.95 / sum_tb;
                top *= k;
                bottom *= k;
            }
            let crop_none = left < 1e-3 && right < 1e-3 && top < 1e-3 && bottom < 1e-3;
            self.adj.geometry.crop = if crop_none {
                None
            } else {
                Some((
                    left,
                    top,
                    (1.0 - left - right).max(0.05),
                    (1.0 - top - bottom).max(0.05),
                ))
            };
            // 常用宽高比：居中按目标比例裁切。基于「正立基图」画面比例换算
            // （裁剪作用在正立基图上，避免用已旋转的 texture 算比例产生偏差）。
            let base_dims = self.base_size;
            if base_dims[0] > 0 && base_dims[1] > 0 {
                let img_ar = base_dims[0] as f32 / base_dims[1] as f32;
                ui.horizontal(|ui| {
                    ui.label("常用比例");
                    for (name, ar) in [
                        ("1:1", 1.0f32),
                        ("3:2", 3.0 / 2.0),
                        ("4:3", 4.0 / 3.0),
                        ("16:9", 16.0 / 9.0),
                    ] {
                        if ui
                            .small_button(name)
                            .on_hover_text("按此比例居中裁切")
                            .clicked()
                        {
                            let target = ar / img_ar; // 相对画面的宽高比
                            if target >= 1.0 {
                                let tb = (1.0 - 1.0 / target) / 2.0;
                                self.adj.geometry.crop =
                                    Some((0.0, tb, 1.0, (1.0 - 2.0 * tb).max(0.05)));
                            } else {
                                let lr = (1.0 - target) / 2.0;
                                self.adj.geometry.crop =
                                    Some((lr, 0.0, (1.0 - 2.0 * lr).max(0.05), 1.0));
                            }
                            g_changed = true;
                        }
                    }
                });
            }
            if ui
                .button("重置裁剪")
                .on_hover_text("恢复完整画面")
                .clicked()
            {
                self.adj.geometry.crop = None;
                g_changed = true;
            }
            if g_changed {
                self.dirty_geo = true;
            }
        });

        // 细节后处理（M5）：降噪 / 锐化 / 柔光
        Self::collapsing_section("细节后处理", force_open, ui, |ui| {
            changed |= self.param_slider(ui, Field::DetailDenoise);
            changed |= self.param_slider(ui, Field::DetailSharpen);
            changed |= self.param_slider(ui, Field::DetailDiffuse);
        });

        // 高级修图（原 M6）：频谱磨皮
        Self::collapsing_section("高级修图", force_open, ui, |ui| {
            changed |= ui
                .checkbox(&mut self.adj.advanced.freqsep.enabled, "频谱磨皮")
                .on_hover_text("人像专用：分离纹理与色块，只柔化色块保留细节。日常照片不建议开")
                .changed();
            let mut fs_touched = false;
            fs_touched |= self.param_slider(ui, Field::FreqSepStrength);
            fs_touched |= self.param_slider(ui, Field::FreqSepTexture);
            fs_touched |= self.param_slider(ui, Field::FreqSepSmooth);
            fs_touched |= self.param_slider(ui, Field::FreqSepFeather);
            // 动一下磨皮滑块就自动启用（默认启用体验）。
            if fs_touched {
                self.adj.advanced.freqsep.enabled = true;
                changed = true;
            }
            // 金字塔融合已移除：实测容易把整图糊掉，且与多分区亮度融合重叠。
        });

        // 智能一键：比原软件更省心
        Self::collapsing_section("智能一键", force_open, ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("自动曝光").clicked() {
                    self.auto_exposure();
                    changed = true;
                }
                if ui.button("自动白平衡").clicked() {
                    self.auto_wb();
                    changed = true;
                }
            });
            ui.horizontal(|ui| {
                if ui.button("一键粉嫩").clicked() {
                    self.adj.skin = SkinTone::pink();
                    changed = true;
                }
                if ui.button("智能去雾").clicked() {
                    self.auto_dehaze();
                    changed = true;
                }
            });
            if ui.button("全智能（曝光+白平衡+去雾）").clicked() {
                self.auto_exposure();
                self.auto_wb();
                self.auto_dehaze();
                changed = true;
            }
        });

        // 作品名设置（可选联网）：仅「生成作品名」用 Qwen 视觉；不点则不联网、零 token。
        // 默认折叠：Key 已本地记住，无需每次展开；点标题可展开编辑/清除。
        // 用局部变量承接展开状态，避免 &mut self.qwen_open 与闭包内 &mut self 冲突。
        let mut qopen = self.qwen_open;
        Self::collapsing_section_state(
            "作品名设置（可选 · 已记忆 Key）",
            &mut qopen,
            ui,
            |ui| {
                ui.label(
                    egui::RichText::new(
                        "仅「生成作品名」用 Qwen 视觉；不填则不联网、零 token。Key 已本地记住。",
                    )
                    .size(11.0)
                    .weak(),
                );
                let mut key = self.api_qwen_key.clone();
                ui.horizontal(|ui| {
                    ui.label("Qwen Key");
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut key)
                            .password(true)
                            .hint_text("粘贴 DashScope / Qwen Key"),
                    );
                    if r.changed() {
                        self.api_qwen_key = key.trim().to_string();
                        Self::save_qwen_key(&self.api_qwen_key);
                        changed = true;
                    }
                });
                ui.horizontal(|ui| {
                    if ui
                        .button("生成作品名")
                        .on_hover_text("用 Qwen 视觉为当前图起名 + 点评（需 Key）")
                        .clicked()
                    {
                        self.generate_title();
                    }
                    if ui
                        .button("清除 Key")
                        .on_hover_text("删除本地记住的 Key（下次需重填）")
                        .clicked()
                    {
                        self.api_qwen_key.clear();
                        Self::forget_qwen_key();
                        changed = true;
                    }
                });
                if let Some(t) = &self.last_title {
                    ui.separator();
                    // 标题：大号粗体
                    let title = if let Some(s) = t.find('》') {
                        &t[..s + 3] // "《春日》" 部分
                    } else {
                        &t[..]
                    };
                    ui.add(egui::Label::new(
                        egui::RichText::new(title).size(15.0).strong(),
                    ));
                    // 点评内容：如有则在可滚动区域显示
                    let review = if let Some(s) = t.find('—') {
                        &t[s + 2..]
                    } else if let Some(s) = t.find('》') {
                        &t[s + 3..]
                    } else {
                        ""
                    };
                    if !review.trim().is_empty() {
                        ui.add_space(2.0);
                        egui::ScrollArea::vertical()
                            .id_salt("review_scroll")
                            .max_height(120.0)
                            .show(ui, |ui| {
                                ui.add(egui::Label::new(
                                    egui::RichText::new(review.trim())
                                        .size(13.0)
                                        .color(egui::Color32::from_gray(180)),
                                ));
                            });
                    }
                }
            },
        );
        self.qwen_open = qopen;

        // 一键智能：纯算法闭环，后台线程跑，不卡 UI。
        Self::collapsing_section("一键智能", force_open, ui, |ui| {
            ui.label(egui::RichText::new("一键中性：纯算法、零 key，把图修到健康中性影调（不过曝）。选力度后点「应用」：").size(11.0).weak());
            ui.horizontal_wrapped(|ui| {
                for (label, val, tip) in [
                    (
                        "弱",
                        0.5f32,
                        "轻度：保留中性化的颜色好处，只做克制的反差/鲜艳微调",
                    ),
                    ("中", 1.0f32, "标准：明显修过且不毁图，适合大多数照片"),
                    (
                        "强",
                        1.8f32,
                        "增强：按影调类型针对性加强反差/暗部/鲜艳，仍保护高光与纯黑",
                    ),
                ] {
                    let selected = (self.neutral_strength - val).abs() < 1e-3;
                    if ui
                        .selectable_label(selected, label)
                        .on_hover_text(tip)
                        .clicked()
                    {
                        self.neutral_strength = val;
                    }
                }
                if ui
                    .button("应用一键中性")
                    .on_hover_text("用当前选中力度修图（可重复修 / 确认效果）")
                    .clicked()
                {
                    self.start_local_auto();
                }
                if self.auto_running {
                    ui.label("修图进行中…");
                }
            });
        });

        ui.separator();
        egui::Frame::group(ui.style())
            .fill(ui.visuals().faint_bg_color)
            .stroke(egui::Stroke::new(
                0.5,
                ui.visuals().widgets.noninteractive.bg_stroke.color,
            ))
            .rounding(egui::Rounding::same(6.0))
            .inner_margin(egui::Margin::same(6.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .button("重置（归零）")
                        .on_hover_text("所有参数归零（纯原图，无任何调整）")
                        .clicked()
                    {
                        self.adj = Adjustments::default();
                        changed = true;
                    }
                    if ui
                        .button("照片默认")
                        .on_hover_text("推荐初始参数（适合大多数照片的起点）")
                        .clicked()
                    {
                        self.adj = Adjustments::photo_default();
                        changed = true;
                    }
                });
            });
        changed
    }

    /// Custom image viewer with zoom, pan, and before/after comparison.
    /// `show_before_key` is true while the backslash key is held.
    fn image_viewer(
        &mut self,
        ui: &mut egui::Ui,
        after: &egui::TextureHandle,
        before: Option<&egui::TextureHandle>,
        show_before_key: bool,
    ) {
        let avail = ui.available_size();
        let panel_rect = egui::Rect::from_min_size(ui.available_rect_before_wrap().min, avail);
        let size = after.size_vec2();
        if size.x <= 0.0 || size.y <= 0.0 {
            return;
        }
        let fit_scale = (avail.x / size.x).min(avail.y / size.y).min(1.0);
        let draw_size = size * fit_scale * self.zoom;
        let center = panel_rect.center() + self.pan;
        let image_rect = egui::Rect::from_center_size(center, draw_size);

        let sense = egui::Sense::click_and_drag().union(egui::Sense::hover());
        let resp = ui.allocate_rect(panel_rect, sense);

        // ═══ 污点修复画笔（v0.6）：仅在 Spot 模式下、在图像区域内点/拖添加笔画 ═══
        // 坐标直接取自「显示矩形」的归一化位置——因为污点在几何之后施加（见
        // rebuild_preview / export_image），显示坐标 == 内容坐标，无需反解几何。
        let mut spot_cursor: Option<egui::Pos2> = None;
        let mut spot_brush_px = 0.0f32;
        if self.tool_mode == ToolMode::Spot {
            // 方括号快捷键调整笔刷大小（PS 同款交互）。
            ui.ctx().input_mut(|i| {
                if i.consume_key(egui::Modifiers::NONE, egui::Key::OpenBracket) {
                    self.spot_brush = self.spot_brush.saturating_sub(2).max(2);
                }
                if i.consume_key(egui::Modifiers::NONE, egui::Key::CloseBracket) {
                    self.spot_brush = (self.spot_brush + 2).min(50);
                }
            });
            // 拖动开始：记录起点笔画索引，之后累积的笔画画成红点预览（松手才愈合）。
            if resp.drag_started() {
                self.spot_drag_base = Some(self.spot.as_ref().map_or(0, |s| s.strokes.len()));
            }
            if let Some(pos) = resp.hover_pos() {
                let inside = pos.x >= image_rect.min.x
                    && pos.x <= image_rect.max.x
                    && pos.y >= image_rect.min.y
                    && pos.y <= image_rect.max.y;
                if inside {
                    let cx = ((pos.x - image_rect.min.x) / image_rect.width()).clamp(0.0, 1.0);
                    let cy = ((pos.y - image_rect.min.y) / image_rect.height()).clamp(0.0, 1.0);
                    let r_norm = self.spot_brush as f32 / 1000.0;
                    let dragging = resp.dragged();
                    let clicked = resp.clicked();
                    // 拖动/点按都累积笔画（去重：与上一笔间距足够才落点，避免重复）。
                    if dragging || clicked {
                        let add = match &self.spot {
                            Some(s) => s.strokes.last().map_or(true, |last| {
                                let dx = last.cx - cx;
                                let dy = last.cy - cy;
                                (dx * dx + dy * dy).sqrt() > r_norm * 0.5
                            }),
                            None => true,
                        };
                        if add {
                            if self.spot.is_none() {
                                let mut nf = SpotFix::new();
                                nf.mode = self.heal_mode;
                                self.spot = Some(nf);
                            }
                            if let Some(s) = &mut self.spot {
                                s.add_stroke(cx, cy, r_norm);
                            }
                            // 单击：立即愈合一次（走轻量 dirty_geo：只重合成几何+污点，
                            // 不触发整条颜色管线）。拖动：只累积，松手才算（见下方 drag_stopped）。
                            if clicked {
                                self.dirty_geo = true;
                            }
                        }
                    }
                    spot_cursor = Some(pos);
                    spot_brush_px = (r_norm * image_rect.width().min(image_rect.height())).max(2.0);
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                }
            }
            // 松手：一次性愈合本次拖动累积的所有笔画（松手才算，Win 也流畅）。
            if resp.drag_stopped() {
                self.spot_drag_base = None;
                self.dirty_geo = true;
            }
        }

        // Decide which texture to draw.
        let show_before =
            show_before_key || (self.compare_mode == CompareMode::Toggle && before.is_some());

        if self.compare_mode == CompareMode::Split {
            if let Some(before) = before {
                let split_x =
                    panel_rect.left() + panel_rect.width() * self.split_pos.clamp(0.1, 0.9);
                let left_rect =
                    egui::Rect::from_min_max(panel_rect.min, egui::pos2(split_x, panel_rect.max.y));
                let right_rect =
                    egui::Rect::from_min_max(egui::pos2(split_x, panel_rect.min.y), panel_rect.max);
                ui.painter().with_clip_rect(left_rect).image(
                    before.id(),
                    image_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
                ui.painter().with_clip_rect(right_rect).image(
                    after.id(),
                    image_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
                // Draggable divider with a generous hit area and resize cursor.
                let div_handle_w = 8.0f32;
                let divider = egui::Rect::from_center_size(
                    egui::pos2(split_x, panel_rect.center().y),
                    egui::vec2(div_handle_w, panel_rect.height()),
                );
                let div_resp = ui.interact(divider, ui.id().with("split"), egui::Sense::drag());
                if div_resp.dragged() {
                    self.split_pos = ((div_resp.interact_pointer_pos().unwrap_or(center).x
                        - panel_rect.left())
                        / panel_rect.width())
                    .clamp(0.05, 0.95);
                }
                if div_resp.hovered() || div_resp.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                ui.painter()
                    .rect_filled(divider, 0.0, egui::Color32::from_gray(180));
            } else {
                ui.painter().image(
                    after.id(),
                    image_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
        } else if show_before {
            if let Some(before) = before {
                ui.painter().image(
                    before.id(),
                    image_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
        } else {
            ui.painter().image(
                after.id(),
                image_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }

        // Zoom with scroll wheel, keeping the point under the cursor stable.
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                if let Some(hover_pos) = resp.hover_pos() {
                    let old_zoom = self.zoom;
                    // Zoom range 0.5×–200% (2.0×) is plenty for retouch review.
                    let new_zoom = (self.zoom * 1.1f32.powf(scroll / 50.0)).clamp(0.5, 2.0);
                    // Adjust pan so that hover_pos stays over the same image point.
                    let zoom_ratio = new_zoom / old_zoom;
                    let center_to_hover = hover_pos - center;
                    self.pan = self.pan + center_to_hover * (1.0 - zoom_ratio);
                    self.zoom = new_zoom;
                }
            }
        }

        // Pan by dragging (middle mouse or holding Shift).
        let space_down = ui.input(|i| i.modifiers.shift);
        if resp.dragged_by(egui::PointerButton::Middle) || (resp.dragged() && space_down) {
            self.pan += resp.drag_delta();
            self.is_panning = true;
        } else if resp.drag_stopped() {
            self.is_panning = false;
        }

        // Double-click to fit.
        if resp.double_clicked() {
            self.zoom = 1.0;
            self.pan = egui::Vec2::ZERO;
        }

        // 拖动中「待修复」红点覆盖层：松手才愈合，拖动过程用半透明红点标出即将修复的区域，
        // 让用户拖动时就能看到画笔轨迹覆盖（不必等松手才有反馈）。
        if self.tool_mode == ToolMode::Spot {
            if let (Some(base), Some(spot)) = (self.spot_drag_base, &self.spot) {
                let side = image_rect.width().min(image_rect.height());
                for s in spot.strokes.iter().skip(base) {
                    let center = egui::pos2(
                        image_rect.min.x + s.cx * image_rect.width(),
                        image_rect.min.y + s.cy * image_rect.height(),
                    );
                    let rad = (s.r_norm * side).max(2.0);
                    ui.painter().circle_filled(
                        center,
                        rad,
                        egui::Color32::from_rgba_unmultiplied(255, 90, 90, 70),
                    );
                }
            }
        }

        // 污点画笔圆圈预览（最后画，覆盖在图像之上）。
        if let (Some(p), ToolMode::Spot) = (spot_cursor, self.tool_mode) {
            ui.painter().circle_stroke(
                p,
                spot_brush_px,
                egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 90, 90)),
            );
        }

        // Hint overlay.
        if resp.hovered() && self.tool_mode == ToolMode::Adjust {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
        }
    }

    /// 预览重建：把「几何变换」作用到已校色的小基图上，生成最终预览像素并上传 GPU。
    /// 90° 旋转是转置（O(n)、微秒级），任意角走双线性仿射；输出尺寸由
    /// `apply_geometry` 严格保证（输入 w×h×3 → 输出 w'×h'×3），`ColorImage::from_rgb`
    /// 不可能尺寸不符——结构上杜绝了旋转崩溃。几何改动 100% 走这里，绝不触发重型颜色管线。
    fn rebuild_preview(&mut self, ctx: &egui::Context) {
        let (base, bsize) = match (&self.base_rgba, self.base_size) {
            (Some(b), [w, h]) if w > 0 && h > 0 => (b.clone(), [w, h]),
            _ => return,
        };
        let (bw, bh) = (bsize[0] as u32, bsize[1] as u32);
        // 正立基图 → 应用几何 → 旋转/翻转/裁剪后的预览
        let base_img = match image::RgbImage::from_raw(bw, bh, base) {
            Some(i) => i,
            None => return,
        };
        let dyn_img = image::DynamicImage::ImageRgb8(base_img);
        let out = apply_geometry(dyn_img, &self.adj.geometry);
        let out_rgb = out.to_rgb8();
        let (ow, oh) = out_rgb.dimensions();
        if ow == 0 || oh == 0 {
            return;
        }
        // v0.6 污点修复：在「几何之后」的最终预览图上 inpaint（坐标即显示坐标，
        // 无需反解几何，所见即所得）。与导出 export_image 的施加点完全一致。
        let out_rgb = if let Some(spot) = &self.spot {
            if !spot.is_empty() {
                // 预览走 preview=true（Poisson 降到 80 迭代，交互流畅）；导出仍满 250。
                spot.heal(&out_rgb, true)
            } else {
                out_rgb
            }
        } else {
            out_rgb
        };
        let out_raw = out_rgb.into_raw(); // 长度 == ow*oh*3
        let after_img = egui::ColorImage::from_rgb([ow as usize, oh as usize], &out_raw);
        self.texture =
            Some(ctx.load_texture("preview", after_img, egui::TextureOptions::default()));

        // before：原图（未校色、正立）同样施加几何，保证对比时两者同向。
        if let (Some(br), [bw2, bh2]) = (&self.before_rgba, self.before_size) {
            if bw2 > 0 && bh2 > 0 {
                if let Some(bimg) = image::RgbImage::from_raw(bw2 as u32, bh2 as u32, br.clone()) {
                    let bdyn = image::DynamicImage::ImageRgb8(bimg);
                    let bout = apply_geometry(bdyn, &self.adj.geometry).to_rgb8();
                    let (ow2, oh2) = bout.dimensions();
                    if ow2 == 0 || oh2 == 0 {
                        return;
                    }
                    let br2 = bout.into_raw();
                    let before_img = egui::ColorImage::from_rgb([ow2 as usize, oh2 as usize], &br2);
                    self.before_texture = Some(ctx.load_texture(
                        "before",
                        before_img,
                        egui::TextureOptions::default(),
                    ));
                }
            }
        }
    }

    /// 相册面板（v0.6）：右侧竖栏。顶部「导入 / 上一张 / 下一张 / 批量导出」；
    /// 下方竖排缩略图，点击切换活跃图（参数/污点各自独立）。当前活跃项高亮描边。
    /// 每个缩略图前的复选框控制「批量导出是否选中」。缩略图纹理懒缓存，仅生成一次。
    fn album_panel(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let n = self.album.slots.len();

        // ═══ 顶部小工具条 ═══
        ui.horizontal_wrapped(|ui| {
            ui.add_space(4.0);
            if Self::toolbar_btn(ui, "导入", "多选导入图片到相册（上限 50）") {
                self.open();
            }
            if n > 0 {
                ui.add_space(2.0);
                if Self::toolbar_btn(ui, "←", "上一张") {
                    let prev = (self.album.active_idx + n - 1) % n;
                    self.switch_to(prev);
                }
                if Self::toolbar_btn(ui, "→", "下一张") {
                    let next = (self.album.active_idx + 1) % n;
                    self.switch_to(next);
                }
                ui.add_space(2.0);
                if Self::toolbar_btn(ui, "批量导出", "把选中图导出到文件夹") {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        self.batch_export(dir);
                    }
                }
            }
        });
        ui.separator();

        if n == 0 {
            ui.centered_and_justified(|ui| {
                ui.label("相册为空。点「导入」多选图片，或拖拽到画布。");
            });
            return;
        }

        // 选中计数
        let sel = self.album.slots.iter().filter(|s| s.selected).count();
        ui.label(
            egui::RichText::new(format!("相册 {} 张 · 选中 {} 张", n, sel))
                .size(13.0)
                .weak(),
        );
        ui.add_space(4.0);

        // ═══ 缩略图竖排（点击切换；本帧收集目标，循环外再 switch 避开借用冲突）═══
        let mut go_to: Option<usize> = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            let active = self.album.active_idx;
            for (i, slot) in self.album.slots.iter_mut().enumerate() {
                let is_active = i == active;
                // 缩略图纹理懒缓存（仅在首次建纹理）。
                let tex = match &slot.thumb_tex {
                    Some(t) => Some(t.clone()),
                    None => {
                        if let Some(img) = &slot.thumb {
                            let rgb = img.to_rgb8();
                            let (w, h) = rgb.dimensions();
                            let ci = egui::ColorImage::from_rgb(
                                [w as usize, h as usize],
                                &rgb.into_raw(),
                            );
                            let t = ctx.load_texture(
                                format!("thumb_{}", i),
                                ci,
                                egui::TextureOptions::default(),
                            );
                            slot.thumb_tex = Some(t.clone());
                            Some(t)
                        } else {
                            None
                        }
                    }
                };

                let frame = egui::Frame::none()
                    .stroke(egui::Stroke::new(
                        if is_active { 3.0 } else { 1.0 },
                        if is_active {
                            egui::Color32::from_rgb(80, 160, 255)
                        } else {
                            egui::Color32::from_gray(70)
                        },
                    ))
                    .rounding(8.0)
                    .inner_margin(4.0);

                let resp = frame
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            // 批量导出选中框
                            let mut sel = slot.selected;
                            if ui.checkbox(&mut sel, "").changed() {
                                slot.selected = sel;
                            }
                            if let Some(t) = &tex {
                                let sz = t.size_vec2();
                                let ar = sz.x / sz.y.max(0.01);
                                let (tw, th) = if ar >= 1.0 {
                                    (60.0f32, 60.0f32 / ar)
                                } else {
                                    (60.0f32 * ar, 60.0f32)
                                };
                                let st = egui::load::SizedTexture::new(t.id(), egui::vec2(tw, th));
                                ui.add(egui::Image::new(st));
                            }
                        });
                        if let Some(name) = slot.path.file_name().and_then(|s| s.to_str()) {
                            ui.label(egui::RichText::new(name).size(12.0).weak());
                        }
                        if let Some(title) = &slot.title {
                            ui.label(
                                egui::RichText::new(title)
                                    .size(11.0)
                                    .color(egui::Color32::from_gray(150)),
                            );
                        }
                    })
                    .response;

                // 让整个缩略图卡片（除复选框外）可点击切换，避免只有边框无 sense 导致点击无效。
                let thumb_resp = ui.interact(
                    resp.rect,
                    egui::Id::new(("album_thumb", i)),
                    egui::Sense::click(),
                );
                if thumb_resp.clicked() {
                    go_to = Some(i);
                }
                ui.add_space(4.0);
            }
        });
        if let Some(idx) = go_to {
            self.switch_to(idx);
        }
    }
}

impl eframe::App for RetouchApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 兜底：任何一帧里的 panic（尤其是几何旋转导致尺寸互换等路径）都绝不应
        // 炸掉整个 app。捕获后跳过本帧并打日志，下一帧 egui 立即重建，继续运行。
        // 这是对「点左转/右转就崩溃」最稳的根治——即便未来新增模块有隐藏 panic，
        // 也不会再让 app 直接 SIGABRT 退出。
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.update_inner(ctx, _frame);
        }));
        if let Err(e) = result {
            eprintln!(
                "[retouch-rs] update 捕获到 panic（app 继续运行，已跳过本帧）: {:?}",
                e
            );
            // 强制请求下一帧重绘，尽快从异常帧恢复。
            ctx.request_repaint();
        }
    }
}

impl RetouchApp {
    fn update_inner(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 全局字体与间距：侧栏 body 25px（用户说够大了），按钮和 monospace 稍小。
        // 滑竿宽度每根单独设（available_width * 0.67 缩短 1/3）。
        ctx.all_styles_mut(|style| {
            use egui::{FontFamily, FontId, TextStyle};
            style.text_styles.insert(
                TextStyle::Small,
                FontId::new(16.0, FontFamily::Proportional),
            );
            style
                .text_styles
                .insert(TextStyle::Body, FontId::new(25.0, FontFamily::Proportional));
            style.text_styles.insert(
                TextStyle::Button,
                FontId::new(22.0, FontFamily::Proportional),
            );
            style.text_styles.insert(
                TextStyle::Heading,
                FontId::new(32.0, FontFamily::Proportional),
            );
            style.text_styles.insert(
                TextStyle::Monospace,
                FontId::new(22.0, FontFamily::Monospace),
            );
            style.spacing.slider_rail_height = 9.0;
            style.spacing.item_spacing = egui::vec2(8.0, 4.0);
            style.spacing.button_padding = egui::vec2(12.0, 6.0);
            // 圆弧角：让所有按钮/控件用流畅圆角，视觉更柔和现代。
            let r = egui::Rounding::same(9.0);
            for w in [
                &mut style.visuals.widgets.noninteractive,
                &mut style.visuals.widgets.inactive,
                &mut style.visuals.widgets.hovered,
                &mut style.visuals.widgets.active,
                &mut style.visuals.widgets.open,
            ] {
                w.rounding = r;
            }
            // 保留 egui 原始深色主题配色，不加自定义颜色覆盖。
        });

        // 主题：按 ThemeMode 切换深色/浅色
        Self::apply_theme(ctx, self.theme_mode);

        // 当前指针为空时初始化技巧（启动后第一帧）
        if self.current_tip.is_empty() {
            self.current_tip = tips::random_tip();
        }

        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::K)) {
            self.show_cmd = !self.show_cmd;
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::O)) {
            self.open();
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::P)) {
            self.load_preset_file();
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S)) {
            self.save();
        }
        // Drag-and-drop: accept image files dropped anywhere on the window.
        if !ctx.input(|i| i.raw.dropped_files.is_empty()) {
            let dropped = ctx.input(|i| i.raw.dropped_files.clone());
            for file in &dropped {
                if let Some(path) = &file.path {
                    let ext = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_lowercase();
                    if ["jpg", "jpeg", "png", "tif", "tiff"].contains(&ext.as_str()) {
                        // Remember directory from drop too.
                        if let Some(parent) = path.parent() {
                            self.last_open_dir = Some(parent.to_path_buf());
                        }
                        self.load_image(path);
                        break; // Only take first valid image
                    }
                }
            }
        }
        // Backslash temporarily shows the original image (before state).
        let show_before_key = ctx.input(|i| i.key_down(egui::Key::Backslash));

        // 响应式折叠：窗口宽度跨越 900px 阈值时自动切换相册栏显隐——
        // 变窄→自动折叠给画布让位；变宽→自动展开。跨越之间用户可手动覆盖。
        let narrow = ctx.screen_rect().width() < 900.0;
        if narrow != self.was_narrow {
            self.show_album = !narrow;
            self.was_narrow = narrow;
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            // 整体垂直留白，不贴边
            ui.add_space(6.0);

            // ═══════════════════════════════════════════
            // Row 1: 品牌名 + 厂商 + 主题切换 + 换一句
            // ═══════════════════════════════════════════
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                // 品牌名：大一点，有存在感
                ui.label(egui::RichText::new("初色").size(18.0).strong());
                ui.label(egui::RichText::new("· 所见即所忆").size(14.0).weak());
                // 厂商信息：极小灰字，不抢眼但看得到
                ui.add(egui::Label::new(
                    egui::RichText::new("星TAP实验室  cscb603@qq.com")
                        .size(10.0)
                        .color(egui::Color32::from_gray(130)),
                ));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(8.0); // 右侧留白，不贴边
                                       // 换一句按钮
                    if Self::toolbar_btn(ui, "换一句", "换一条修图小技巧") {
                        self.current_tip = tips::next_tip(self.current_tip);
                    }
                    ui.add_space(4.0);
                    // 主题切换按钮
                    let theme_icon = self.theme_mode.icon();
                    let theme_label = self.theme_mode.label();
                    if Self::toolbar_btn(
                        ui,
                        &format!("{} {}", theme_icon, theme_label),
                        "点击循环：自动 → 深色 → 浅色",
                    ) {
                        self.theme_mode = match self.theme_mode {
                            ThemeMode::Auto => ThemeMode::Dark,
                            ThemeMode::Dark => ThemeMode::Light,
                            ThemeMode::Light => ThemeMode::Auto,
                        };
                    }
                });
                ui.add_space(8.0);
            });

            // ═══════════════════════════════════════════
            // Row 2: 小技巧 + 状态文字（同排右）
            // ═══════════════════════════════════════════
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                // 技巧文字：占据剩余空间
                ui.add(egui::Label::new(
                    egui::RichText::new(format!("— {}", self.current_tip))
                        .size(13.0)
                        .weak(),
                ));
                // 状态文字：右对齐 + 右侧留白 16px；太长自动裁剪（悬停看全）
                let status_text = self.status.clone();
                let busy = self.is_busy();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(16.0); // 右留白
                                        // 忙碌时显示旋转指示器，让用户明确知道后台正在处理。
                    if busy {
                        ui.add(egui::Spinner::new().size(14.0));
                        ui.add_space(6.0);
                    }
                    // 隐去滚动条，文本裁剪自然溢出
                    let resp = ui.add(egui::Label::new(
                        egui::RichText::new(status_text)
                            .size(13.0)
                            .color(egui::Color32::from_gray(160)),
                    ));
                    resp.on_hover_ui(|ui| {
                        ui.label(egui::RichText::new(&self.status).size(12.0));
                    });
                });
            });
            ui.add_space(2.0);

            // 细分割线
            ui.separator();

            // ═══════════════════════════════════════════
            // Row 3: 工具栏按钮组（可换行：窄窗时分组自动折到下一行，绝不溢出屏外）
            // ═══════════════════════════════════════════
            ui.horizontal_wrapped(|ui| {
                ui.add_space(4.0);

                Self::toolbar_group(ui, "文件", |ui| {
                    if Self::toolbar_btn(ui, "打开", "从文件系统打开图片 (Cmd+O)") {
                        self.open();
                    }
                    if Self::toolbar_btn(ui, "载入预设", "加载 TOML 预设参数 (Cmd+P)") {
                        self.load_preset_file();
                    }
                    if Self::toolbar_btn(ui, "存预设", "把当前所有参数保存为 TOML 预设")
                    {
                        self.save_preset_file();
                    }
                    if Self::toolbar_btn(ui, "保存图", "导出/保存图片 (Cmd+S)") {
                        self.save();
                    }
                    if Self::toolbar_btn(ui, "作品名", "Qwen 视觉生成作品名（默认不联网）")
                    {
                        self.generate_title();
                    }
                });

                Self::toolbar_group(ui, "校正", |ui| {
                    if Self::toolbar_btn(ui, "一键中性", "本地零 key 中性校正（闭环，不过曝）")
                    {
                        self.start_local_auto();
                    }
                    if Self::toolbar_btn(ui, "参考匹配", "导入喜欢的图，一键克隆它的影调")
                    {
                        if self.ref_metrics.is_none() {
                            self.import_reference(ctx);
                        } else {
                            self.start_reference_match();
                        }
                    }
                });

                Self::toolbar_group(ui, "视图", |ui| {
                    if Self::toolbar_btn(ui, "命令", "打开命令盘 (Cmd+K)") {
                        self.show_cmd = !self.show_cmd;
                    }
                    if Self::toolbar_btn(ui, "全展开", "展开所有参数分组") {
                        self.force_open = Some(true);
                    }
                    if Self::toolbar_btn(ui, "全收起", "收起所有参数分组") {
                        self.force_open = Some(false);
                    }
                    let album_label = if self.show_album {
                        "●相册"
                    } else {
                        "相册"
                    };
                    if Self::toolbar_btn(ui, album_label, "显示/隐藏右侧相册栏（窄窗自动折叠）")
                    {
                        self.show_album = !self.show_album;
                    }
                });

                Self::toolbar_group(ui, "画布", |ui| {
                    if Self::toolbar_btn(ui, "适应", "缩放图片到刚好填满画布") {
                        self.zoom = 1.0;
                        self.pan = egui::Vec2::ZERO;
                    }
                    if Self::toolbar_btn(ui, "1:1", "按实际像素显示") {
                        if let Some(tex) = &self.texture {
                            let avail = ui.available_size();
                            let size = tex.size_vec2();
                            let fit = (avail.x / size.x).min(avail.y / size.y).min(1.0);
                            self.zoom = 1.0 / fit.max(1e-3);
                            self.pan = egui::Vec2::ZERO;
                        }
                    }
                    let cmp_label = match self.compare_mode {
                        CompareMode::Off => "对比原图",
                        CompareMode::Toggle => "对比：原图",
                        CompareMode::Split => "对比：分屏",
                    };
                    if Self::toolbar_btn(
                        ui,
                        cmp_label,
                        "点击循环：关 → 整图显示原图 → 左右分屏对比（也可按住 \\ 键临时看原图）",
                    ) {
                        self.compare_mode = match self.compare_mode {
                            CompareMode::Off => CompareMode::Toggle,
                            CompareMode::Toggle => CompareMode::Split,
                            CompareMode::Split => CompareMode::Off,
                        };
                    }
                });

                Self::toolbar_group(ui, "工具", |ui| {
                    let adj_on = self.tool_mode == ToolMode::Adjust;
                    if Self::toolbar_btn(
                        ui,
                        if adj_on { "●调色" } else { "调色" },
                        "普通调色模式（画笔/缩放/对比）",
                    ) {
                        self.tool_mode = ToolMode::Adjust;
                    }
                    let spot_on = self.tool_mode == ToolMode::Spot;
                    if Self::toolbar_btn(
                        ui,
                        if spot_on { "●污点" } else { "污点" },
                        "污点修复画笔模式：在画布上点/拖修复瑕疵",
                    ) {
                        self.tool_mode = ToolMode::Spot;
                    }
                });

                ui.add_space(4.0);
            });
        });

        egui::SidePanel::left("controls")
            .resizable(true)
            .default_width(380.0)
            .min_width(260.0)
            .max_width(680.0)
            .show(ctx, |ui| {
                let resp = egui::ScrollArea::vertical().show(ui, |ui| self.side_panel(ctx, ui));
                self.dirty |= resp.inner;
            });
        // 一键展开/收起只在触发的那一帧强制覆盖，之后恢复各分组的记忆状态。
        self.force_open = None;

        // 右侧相册竖栏（v0.6 轻量 Lightroom 化）：缩略图 + 导航 + 批量导出入口。
        // 窄窗自动折叠（show_album=false），画布独占；工具栏「相册」按钮可随时手动开关。
        if self.show_album {
            egui::SidePanel::right("album")
                .resizable(true)
                .default_width(220.0)
                .min_width(170.0)
                .max_width(320.0)
                .show(ctx, |ui| {
                    self.album_panel(ctx, ui);
                });
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(tex) = self.texture.clone() {
                let before = self.before_texture.clone();
                self.image_viewer(ui, &tex, before.as_ref(), show_before_key);
            } else if self.loading {
                ui.centered_and_justified(|ui| {
                    ui.vertical_centered(|ui| {
                        ui.add(egui::Spinner::new().size(28.0));
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("载入中…").size(16.0).weak());
                    });
                });
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label("按 Cmd+O 打开图片，或 Cmd+P 加载预设。");
                });
            }
        });

        if self.show_cmd {
            let mut close = false;
            egui::Window::new("命令盘 (Cmd+K)")
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    let enter = ui.text_edit_singleline(&mut self.cmd).lost_focus()
                        && ctx.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("执行").clicked() || enter {
                        let cmd = self.cmd.clone();
                        close = self.run_command(ctx, &cmd);
                        self.cmd.clear();
                    }
                    ui.label("输入 help 查看命令。");
                });
            if close {
                self.show_cmd = false;
            }
        }

        // 每帧轮询后台智能修图结果；完成时把采用参数套用到当前 adj 并触发重绘。
        self.poll_auto();
        // 每帧检查「生成作品名」异步结果，落到状态栏。
        self.poll_title();
        // v0.6.3：轮询后台导入 / 切图解码 / 批量导出，界面全程不冻结。
        self.poll_import();
        self.poll_load();
        self.poll_export();

        // 异步预览渲染：把重管线丢到后台线程，主线程只负责上传结果到 GPU。
        // 如果用户在渲染过程中继续拖动滑竿，旧的完成后会自动追加一次最新状态。
        if self.dirty && !self.render_pending {
            if let Some(src) = self.src.clone() {
                if self
                    .render_tx
                    .send(RenderRequest {
                        src: Arc::new(src),
                        adj: self.adj.clone(),
                        preview_max: PREVIEW_MAX,
                        need_before: self.before_rgba.is_none(),
                    })
                    .is_ok()
                {
                    self.render_pending = true;
                }
            }
            self.dirty = false;
        }

        if self.render_pending {
            if let Ok(result) = self.render_rx.try_recv() {
                // 护栏：after/before 的「尺寸×3」必须与 rgb 缓冲长度严格一致，
                // 否则 ColorImage::from_rgb 会 panic。几何旋转会让尺寸互换，
                // 这里双保险——不一致就丢弃本帧结果，等待下一帧正确数据，绝不崩。
                let [aw, ah] = result.after_size;
                let need = aw.saturating_mul(ah).saturating_mul(3);
                if result.after_rgb.len() != need {
                    eprintln!(
                        "[retouch-rs] 预览结果尺寸不符（{}×{}×3={} != {}），丢弃本帧",
                        aw,
                        ah,
                        need,
                        result.after_rgb.len()
                    );
                } else {
                    // 基图入库：颜色管线只产出「正立」结果，几何稍后单独施加。
                    self.base_rgba = Some(result.after_rgb);
                    self.base_size = result.after_size;
                }
                if let Some(rgb) = result.before_rgb {
                    let [bw, bh] = result.before_size;
                    let bneed = bw.saturating_mul(bh).saturating_mul(3);
                    if rgb.len() == bneed {
                        self.before_rgba = Some(rgb);
                        self.before_size = result.before_size;
                    }
                }
                self.render_pending = false;

                // 用「最新几何」把基图转成最终预览（同步、微秒级、不碰颜色管线）。
                self.rebuild_preview(ctx);

                // 渲染期间参数又变了，立即追加一次新渲染（基图重算）。
                if self.dirty {
                    if let Some(src) = self.src.clone() {
                        if self
                            .render_tx
                            .send(RenderRequest {
                                src: Arc::new(src),
                                adj: self.adj.clone(),
                                preview_max: PREVIEW_MAX,
                                need_before: self.before_rgba.is_none(),
                            })
                            .is_ok()
                        {
                            self.render_pending = true;
                        }
                    }
                    self.dirty = false;
                }
            }
        }

        // 仅几何变化（旋转/翻转/裁剪）：同步重算预览，绝不触发异步颜色管线。
        // 这是「点旋转即时预览」的核心——转置 O(n)、尺寸由 apply_geometry 严格
        // 保证，ColorImage::from_rgb 不可能尺寸不符，结构上杜绝旋转崩溃。
        if self.dirty_geo {
            if self.base_rgba.is_some() {
                self.rebuild_preview(ctx);
            }
            self.dirty_geo = false;
        }

        // 导出配置对话框（show_export = true 时显示）
        if self.show_export {
            self.show_export_dialog(ctx);
        }

        // 及时刷新反馈：egui 默认只在有输入时重绘，后台渲染/智能修图完成的
        // 结果不会立刻显示。只要还有未取回的后台任务，就持续请求重绘，让
        // 预览图和状态文字第一时间刷新（不空转，任务结束即停止）。
        if self.render_pending
            || self.auto_running
            || self.dirty
            || self.import_rx.is_some()
            || self.export_rx.is_some()
            || self.loading
        {
            ctx.request_repaint();
        }
    }
}

#[inline]
fn srgb_to_linear(u: u8) -> f32 {
    let c = u as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn main() -> eframe::Result {
    let icon = load_app_icon();
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        // 窗口标题栏图标（Dock 图标由 .app/Contents/Resources/AppIcon.icns 提供，
        // 但标题栏左侧的小图标必须由 eframe 的 viewport.with_icon 注入，否则显示
        // egui 默认的「e」）。这里用内置 PNG 转 egui::IconData 注入。
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_icon(Arc::new(icon)),
        ..Default::default()
    };
    eframe::run_native(
        "初色",
        options,
        Box::new(|cc| {
            setup_cjk_fonts(&cc.egui_ctx);
            Ok(Box::new(RetouchApp::new()))
        }),
    )
}

/// 从内置 PNG 资源生成 egui 窗口图标（标题栏 + 任务切换器显示）。
/// 编译期用 include_bytes! 嵌入，零运行时文件依赖。
fn load_app_icon() -> egui::IconData {
    let bytes = include_bytes!("../assets/app-icon-512.png");
    let img = image::load_from_memory(bytes).expect("内置 app-icon-512.png 必须是合法图片");
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    egui::IconData {
        rgba: rgba.into_raw(),
        width: w,
        height: h,
    }
}

/// 跨平台 CJK 字体加载（混合路线 D）。
///
/// 优先级：
/// 1. 系统字体 —— macOS 苹方/Hiragino/STHeiti/Songti；Windows 微软雅黑/宋体；
///    Linux Noto Sans CJK / 文泉驿。每个系统路径都按平台裁剪。
/// 2. 随包兜底 —— 思源黑体（OFL，放在 `Retouch.app/Contents/Resources/fonts`
///    或可执行文件旁的 `fonts/`、`assets/fonts/`）。任何系统都读不到系统字体时
///    回退到这里，保证「通用」绝不出现豆腐块。
///
/// 字体一律从**文件运行时读取**，不 `include_bytes!` 编进二进制；换字体只需替换
/// 资源文件，无需重编。
fn setup_cjk_fonts(ctx: &egui::Context) {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    let bundled = bundled_font_candidates();

    // ---- Windows：用户指定优先用随包思源黑体 ----
    // 与 Mac 质感接近、随包保证存在、0 依赖；系统雅黑/宋体作为二次兜底。
    // 系统路径用 WINDIR 环境变量拼，不硬编码 C:\Windows。
    if cfg!(target_os = "windows") {
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_string());
        let wf = |name: &str| -> std::path::PathBuf {
            std::path::Path::new(&windir).join("Fonts").join(name)
        };
        candidates.extend(bundled);
        candidates.extend([
            wf("msyh.ttc"),
            wf("msyh.ttf"),
            wf("simsun.ttc"),
            wf("simsun.ttf"),
            wf("msjh.ttc"),
        ]);
    } else if cfg!(target_os = "macos") {
        candidates.extend([
            "/System/Library/Fonts/PingFang.ttc".into(),
            "/System/Library/Fonts/Supplemental/PingFang.ttc".into(),
            "/Library/Fonts/PingFang.ttc".into(),
            "/System/Library/Fonts/Hiragino Sans GB.ttc".into(),
            "/System/Library/Fonts/STHeiti Medium.ttc".into(),
            "/System/Library/Fonts/STHeiti Light.ttc".into(),
            "/System/Library/Fonts/Songti.ttc".into(),
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf".into(),
        ]);
        candidates.extend(bundled);
    } else if cfg!(target_os = "linux") {
        candidates.extend([
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc".into(),
            "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc".into(),
            "/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf".into(),
            "/usr/share/fonts/truetype/noto/NotoSansSC-Regular.otf".into(),
            "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc".into(),
            "/usr/share/fonts/truetype/arphic/uming.ttc".into(),
        ]);
        candidates.extend(bundled);
    }

    for path in &candidates {
        match std::fs::read(path) {
            Ok(bytes) => {
                let mut fonts = egui::FontDefinitions::default();
                fonts
                    .font_data
                    .insert("cjk_fallback".to_owned(), egui::FontData::from_owned(bytes));
                // CJK 排最前（不改基线），不清除默认字体（保留 emoji/拉丁 fallback）。
                let prop = fonts
                    .families
                    .get_mut(&egui::FontFamily::Proportional)
                    .unwrap();
                prop.insert(0, "cjk_fallback".to_owned());
                fonts
                    .families
                    .get_mut(&egui::FontFamily::Monospace)
                    .unwrap()
                    .insert(0, "cjk_fallback".to_owned());
                ctx.set_fonts(fonts);
                // UI 样式统一基础：按钮内边距、图标宽度、控件间距
                // 注意：字体尺寸由 update_inner 每帧统一设置，不在此设避免冲突。
                let mut style = (*ctx.style()).clone();
                // 统一按钮内边距：水平 12px、垂直 4px（行业标准）
                style.spacing.button_padding = egui::vec2(12.0, 4.0);
                // 去掉图标预留宽度，避免中文按钮内部偏移
                style.spacing.icon_width = 0.0;
                // 控件之间水平间距 6px、垂直 4px
                style.spacing.item_spacing = egui::vec2(6.0, 4.0);
                ctx.set_style(style);
                eprintln!("[font] CJK 字体已加载：{}", path.display());
                return;
            }
            Err(_) => continue,
        }
    }
    eprintln!("[font] 警告：未找到任何 CJK 字体，中文将显示为豆腐块 (□)");
}

/// 解析随包字体的可能位置（跨平台）。
/// - .app bundle: `Contents/MacOS/<exe>` → `../../Resources/fonts`
/// - 独立可执行文件: `<exe_dir>/fonts` 或 `<exe_dir>/assets/fonts`
/// - 开发运行: 工作目录下的 `assets/fonts`
fn bundled_font_candidates() -> Vec<std::path::PathBuf> {
    let file_name = "NotoSansSC-VF.ttf";
    let mut out = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            // .app: Contents/MacOS -> ../../Resources/fonts
            if let Some(contents) = exe_dir.parent() {
                out.push(contents.join("Resources").join("fonts").join(file_name));
            }
            out.push(exe_dir.join("fonts").join(file_name));
            out.push(exe_dir.join("assets").join("fonts").join(file_name));
            if let Some(parent) = exe_dir.parent() {
                out.push(parent.join("assets").join("fonts").join(file_name));
            }
        }
    }
    // 开发运行：相对工作目录
    out.push(std::path::PathBuf::from("assets/fonts").join(file_name));
    out
}

// ---------------------------------------------------------------------------
// Color-bar helpers for parameter sliders.
// ---------------------------------------------------------------------------

/// HSL (h in degrees, s/l in 0..1) → sRGB 0..1. Used to build gradient bars.
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [f32; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - ((hp % 2.0) - 1.0).abs());
    let (r1, g1, b1) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    [r1 + m, g1 + m, b1 + m]
}

/// Sample a smooth gradient between `stops` into `n` evenly spaced colors.
/// `stops` are the endpoint colours; the interpolation is spread across the
/// whole bar width, so e.g. [blue, orange] really does go from blue on the
/// left to orange on the right.
fn lerp_gradient(stops: &[[f32; 3]], n: usize) -> Vec<[f32; 3]> {
    if stops.len() < 2 || n == 0 {
        return stops.to_vec();
    }
    let segs = (stops.len() - 1) as f32;
    (0..n)
        .map(|k| {
            // Normalise pixel index to [0, segs] so the gradient spans every
            // stop exactly once. The previous code accidentally used [0, n-1]
            // and clamped to segs, which collapsed nearly the whole bar to the
            // last colour.
            let t = (k as f32 / (n as f32 - 1.0).max(1.0)) * segs;
            let i0 = t.floor().clamp(0.0, segs) as usize;
            let f = t - i0 as f32;
            let i1 = (i0 + 1).min(stops.len() - 1);
            let a = stops[i0];
            let b = stops[i1];
            [
                a[0] + (b[0] - a[0]) * f,
                a[1] + (b[1] - a[1]) * f,
                a[2] + (b[2] - a[2]) * f,
            ]
        })
        .collect()
}

/// Full hue wheel (red at both ends so the bar reads as a loop).
fn hue_wheel(n: usize) -> Vec<[f32; 3]> {
    (0..=n)
        .map(|k| hsl_to_rgb(360.0 * k as f32 / n as f32, 0.85, 0.55))
        .collect()
}

/// Skin-tone band: pink → red → orange (the healthy 粉嫩 range).
fn skin_hue_wheel(n: usize) -> Vec<[f32; 3]> {
    (0..=n)
        .map(|k| hsl_to_rgb(350.0 + 40.0 * k as f32 / n as f32, 0.55, 0.62))
        .collect()
}

/// Linear interpolation between two sRGB colors (0..1).
fn lerp_color(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// Return the gradient stops for a color-related field, or `None` for fields
/// that have no meaningful color direction (so no bar is drawn).
///
/// Gradient bars show a **fixed** two-endpoint color spectrum across the full
/// slider range (FilmRust / Lightroom style). The white tick mark in
/// `paint_bar()` shows where the current value sits within that range.
///
/// Key design rules:
/// - 色温 (WBTemp): blue ↔ yellow-orange, evenly split, neutral at centre
/// - 色调 (WBTint): green ↔ magenta, evenly split, neutral at centre
/// - Saturation/Vibrance: gray ↔ saturated colour
/// - Hue wheels: full spectral cycle
fn field_gradient(f: &Field, raw: f32) -> Option<Vec<[f32; 3]>> {
    const BAND_HUE: [f32; 8] = [0.0, 30.0, 60.0, 120.0, 180.0, 240.0, 280.0, 320.0];
    const NEUTRAL: [f32; 3] = [0.92, 0.92, 0.92];

    match f {
        Field::WBTemp => {
            // 50/50 two-colour split: pure blue on the left, pure orange on the
            // right. The white tick marks the current value; neutral (raw=0)
            // sits exactly at the 50 % boundary, so both colours occupy equal
            // visual width across the full slider range.
            let blue = [0.22, 0.42, 1.0]; // FilmRust-style cool
            let orange = [1.0, 0.58, 0.15]; // FilmRust-style warm
            Some(lerp_gradient(&[blue, orange], 120))
        }
        Field::WBTint => {
            // 50/50 two-colour split: pure green on the left, pure magenta on
            // the right. Neutral (raw=0) is the 50 % mix point.
            let green = [0.30, 0.95, 0.40];
            let magenta = [0.95, 0.30, 0.90];
            Some(lerp_gradient(&[green, magenta], 120))
        }
        Field::Saturation => {
            // Fixed: desaturated (gray) left → neutral centre → oversaturated (red) right.
            let gray = [0.55, 0.55, 0.55];
            let red = [1.0, 0.28, 0.28];
            Some(lerp_gradient(&[gray, NEUTRAL, red], 120))
        }
        Field::Vibrance => {
            // Fixed: gray left → neutral centre → vivid (teal/cyan) right.
            let gray = [0.55, 0.55, 0.55];
            let teal = [0.25, 0.88, 0.75];
            Some(lerp_gradient(&[gray, NEUTRAL, teal], 120))
        }
        Field::SplitShadow => {
            // Shadow tint: colour on left → neutral on right.
            let hue = 180.0; // default cyan shadow (most common)
            let shadow = hsl_to_rgb(hue, 0.85, 0.45);
            Some(lerp_gradient(&[shadow, NEUTRAL], 120))
        }
        Field::SplitHighlight => {
            // Highlight tint: neutral on left → colour on right.
            let hue = 45.0; // default warm highlight
            let highlight = hsl_to_rgb(hue, 0.85, 0.55);
            Some(lerp_gradient(&[NEUTRAL, highlight], 120))
        }
        Field::SkinChroma => {
            let t = (raw / 0.2).clamp(0.0, 1.0);
            let pale = [0.86, 0.72, 0.66];
            let pink = [0.96, 0.58, 0.55];
            Some(lerp_gradient(&[pale, lerp_color(pale, pink, t), pink], 120))
        }
        Field::HslLight(_) => Some(lerp_gradient(
            &[[0.05, 0.05, 0.05], NEUTRAL, [0.95, 0.95, 0.95]],
            120,
        )),
        Field::HueRotate => Some(hue_wheel(120)),
        Field::HslHue(_) => Some(hue_wheel(120)),
        Field::SkinHue => Some(skin_hue_wheel(120)),
        Field::HslSat(i) => {
            let h = BAND_HUE[*i.min(&7)];
            let gray = [0.6, 0.6, 0.6];
            let sat = hsl_to_rgb(h, 0.85, 0.55);
            Some(lerp_gradient(&[gray, sat], 120))
        }
        _ => None,
    }
}

/// Dynamic one-word hint that follows the slider, e.g. "偏暖" / "加饱和".
fn field_hint(f: &Field, raw: f32) -> Option<String> {
    match f {
        Field::WBTemp => {
            if raw < -0.05 {
                Some("偏冷".into())
            } else if raw > 0.05 {
                Some("偏暖".into())
            } else {
                Some("中性".into())
            }
        }
        Field::WBTint => {
            if raw < -0.05 {
                Some("偏绿".into())
            } else if raw > 0.05 {
                Some("偏品红".into())
            } else {
                Some("中性".into())
            }
        }
        Field::Saturation => {
            if raw < 0.97 {
                Some("减饱和".into())
            } else if raw > 1.03 {
                Some("加饱和".into())
            } else {
                Some("原图".into())
            }
        }
        Field::Vibrance => {
            if raw < -0.05 {
                Some("减鲜艳".into())
            } else if raw > 0.05 {
                Some("加鲜艳".into())
            } else {
                Some("原图".into())
            }
        }
        Field::HueRotate => {
            if raw < -5.0 {
                Some("左旋".into())
            } else if raw > 5.0 {
                Some("右旋".into())
            } else {
                Some("无旋转".into())
            }
        }
        Field::SplitShadow => {
            if raw.abs() > 5.0 {
                Some(format!("阴影 {}°", raw.round() as i32))
            } else {
                Some("无阴影染色".into())
            }
        }
        Field::SplitHighlight => {
            if raw.abs() > 5.0 {
                Some(format!("高光 {}°", raw.round() as i32))
            } else {
                Some("无高光染色".into())
            }
        }
        Field::SkinStrength => {
            if raw > 0.05 {
                Some("肤色优化中".into())
            } else {
                Some("未启用".into())
            }
        }
        Field::SkinYellowReduce => {
            if raw > 0.05 {
                Some("去黄".into())
            } else {
                Some("无".into())
            }
        }
        Field::SkinLighten => {
            if raw > 0.05 {
                Some("提亮".into())
            } else {
                Some("无".into())
            }
        }
        Field::SkinRedden => {
            if raw > 0.05 {
                Some("加红".into())
            } else {
                Some("无".into())
            }
        }
        Field::SkinPinken => {
            if raw > 0.05 {
                Some("加粉".into())
            } else {
                Some("无".into())
            }
        }
        Field::SkinHue => {
            if raw > 0.05 {
                Some(format!("{:.0}°", raw))
            } else {
                Some("无".into())
            }
        }
        Field::SkinChroma => {
            if raw > 0.005 {
                Some("红润".into())
            } else {
                Some("无".into())
            }
        }
        Field::SkinLight => {
            if raw > 0.005 {
                Some("提亮".into())
            } else {
                Some("无".into())
            }
        }
        Field::HslHue(_) => {
            if raw < -5.0 {
                Some("左偏".into())
            } else if raw > 5.0 {
                Some("右偏".into())
            } else {
                Some("无".into())
            }
        }
        Field::HslSat(_) => {
            if raw < 0.97 {
                Some("减饱和".into())
            } else if raw > 1.03 {
                Some("加饱和".into())
            } else {
                Some("原图".into())
            }
        }
        Field::HslLight(_) => {
            if raw < 0.97 {
                Some("压暗".into())
            } else if raw > 1.03 {
                Some("提亮".into())
            } else {
                Some("原图".into())
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod font_tests {
    use ttf_parser::{Face, GlyphId};

    /// 校验随包思源黑体是合法字体且含关键中文字形（无需 GPU，可在 CI 跑）。
    #[test]
    fn bundled_cjk_font_parses_and_has_chinese() {
        let base = env!("CARGO_MANIFEST_DIR");
        let path = std::path::Path::new(base)
            .join("assets")
            .join("fonts")
            .join("NotoSansSC-VF.ttf");
        let data = std::fs::read(&path)
            .expect("思源黑体资源文件必须存在（assets/fonts/NotoSansSC-VF.ttf）");
        let face = Face::parse(&data, 0).expect("字体必须是合法 ttf/otf");
        for ch in ['中', '你', '图', '片', '修', '改', '左', '转'] {
            let gid = face.glyph_index(ch).expect("字符必须有字形索引");
            assert_ne!(gid, GlyphId(0), "字符 {ch} 不应映射到 .notdef");
        }
        assert!(face.tables().fvar.is_some(), "应为含字重轴的变量字体");
    }
}

/// 几何保护不变式（纯函数）：`new` 的所有字段生效，但几何沿用 `current` 的。
/// 抽成自由函数便于单测直接验证，且 `replace_adj_preserve_geo` 调用它。
fn preserve_geometry(current: &Adjustments, new: Adjustments) -> Adjustments {
    let mut n = new;
    n.geometry = current.geometry.clone();
    n
}

#[cfg(test)]
mod geo_preserve_tests {
    use super::preserve_geometry;
    use retouch_core::pipeline::Adjustments;

    /// 用户先旋转+裁剪+翻转，再走 `to_adjustments()` 产出的"几何清零"新参数，
    /// 几何必须被死保、颜色参数正常保留。回归：防"调别的参数旋转又回来"。
    #[test]
    fn keeps_rotation_crop_flip_through_auto_preset() {
        let mut current = Adjustments::default();
        current.geometry.quarter_turns = 1;
        current.geometry.crop = Some((0.1, 0.1, 0.9, 0.9));
        current.geometry.flip_h = true;

        let new = {
            let mut a = Adjustments::default();
            a.exposure_ev = 0.3; // 自动调了曝光，几何被清零
            a
        };
        let out = preserve_geometry(&current, new);
        assert_eq!(out.geometry.quarter_turns, 1, "旋转应被死保");
        assert_eq!(
            out.geometry.crop,
            Some((0.1, 0.1, 0.9, 0.9)),
            "裁剪应被死保"
        );
        assert!(out.geometry.flip_h, "翻转应被死保");
        assert_eq!(out.exposure_ev, 0.3, "颜色参数应保留");
    }
}
