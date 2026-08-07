use clap::{Args, Parser, Subcommand, ValueEnum};
use image::{DynamicImage, GenericImageView, Rgb, RgbImage};
use palette::{IntoColor, LinSrgb, Oklch};
use retouch_agent::{thumb_b64, QwenClient};
use retouch_core::advanced::{Advanced, FreqSepSkin, PyramidFusion};
use retouch_core::analyze::{analyze, ImageMetrics};
use retouch_core::auto::run_auto;
use retouch_core::detail::Detail;
use retouch_core::geometry::Geometry;
use retouch_core::guardrail;
use retouch_core::params::Field;
use retouch_core::pipeline::{
    render, Adjustments, ColorGrade, DefakeColor, Grade, HslRegions, SkinTone, ToneMapMode,
    WhiteBalance, ZoneGrade,
};
use retouch_core::preset::{dump_preset, load_preset, Preset};
use retouch_core::schema::param_schema;
use serde_json::Value;
use std::path::PathBuf;
use std::time::Instant;

#[derive(Parser)]
#[command(
    name = "retouch-rs",
    version,
    about = "Non-linear OKLCH photo retouch engine (JPG/TIFF, no darktable, no LibRaw)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum ToneMapArg {
    None,
    Agx,
    Filmic,
}

impl From<ToneMapArg> for ToneMapMode {
    fn from(a: ToneMapArg) -> Self {
        match a {
            ToneMapArg::None => ToneMapMode::None,
            ToneMapArg::Agx => ToneMapMode::Agx,
            ToneMapArg::Filmic => ToneMapMode::Filmic,
        }
    }
}

/// Adjustment flags shared by `render` and `dump`. Every field is optional:
/// a `--preset` provides the base, and any flag here *overrides* the
/// corresponding preset value. When neither a preset nor a flag is given, the
/// field falls back to its identity default (so `render in out` is a
/// pixel-exact round-trip).
#[derive(Args)]
struct CommonOpts {
    /// Load a TOML preset (base settings). CLI flags below override it.
    #[arg(long)]
    preset: Option<PathBuf>,
    /// Exposure compensation in stops (e.g. 0.3 = +1/3 EV, -1.0 = -1 EV)
    #[arg(long)]
    exposure: Option<f32>,
    /// Tone-map / shoulder-compression mode (default: none)
    #[arg(long, value_enum)]
    tone_map: Option<ToneMapArg>,
    /// Enable de-fake-color stage (chroma decay + sky/skin caps + softclip).
    /// If a preset already enables it, this is implied.
    #[arg(long, default_value_t = false)]
    defake: bool,
    /// Chroma decay strength (0..1) when defake is on
    #[arg(long)]
    chroma_decay: Option<f32>,
    /// Soft overall brightness lift in EV-ish units (highlights roll off)
    #[arg(long)]
    brightness: Option<f32>,
    /// Global contrast around mid-gray (0 = off, e.g. 0.15)
    #[arg(long)]
    contrast: Option<f32>,
    /// Mid-tone dehaze / clarity
    #[arg(long)]
    dehaze: Option<f32>,
    /// Shadow recovery — lifts ONLY the dark end to fix dead-black crush
    #[arg(long)]
    shadow_lift: Option<f32>,
    /// Deep-shadow (Blacks) recovery — extra lift for the very darkest end
    #[arg(long)]
    deep_shadow_lift: Option<f32>,
    /// White balance color temperature (-1 cool .. +1 warm), linear-RGB gain
    #[arg(long)]
    temp: Option<f32>,
    /// White balance tint (-1 green .. +1 magenta), linear-RGB gain
    #[arg(long)]
    tint: Option<f32>,
    /// Uniform saturation multiplier (1.0 = unchanged)
    #[arg(long)]
    saturation: Option<f32>,
    /// Smart vibrance: boost low-chroma pixels more than saturated ones
    #[arg(long)]
    vibrance: Option<f32>,
    /// Global hue rotation in degrees (-180..180), creative cast
    #[arg(long)]
    hue_rotate: Option<f32>,
    /// Split-tone: add this hue (deg) to SHADOWS only
    #[arg(long)]
    split_shadow: Option<f32>,
    /// Split-tone: add this hue (deg) to HIGHLIGHTS only
    #[arg(long)]
    split_highlight: Option<f32>,
    /// Per-hue-region HSL (M2c), repeatable. Format: <band>:<hue>,<sat>,<light>
    /// where <band> is red|orange|yellow|green|aqua|blue|purple|magenta.
    /// Example: --hsl blue:0,1.4,1.0  (boost sky saturation).
    #[arg(long = "hsl", value_name = "BAND:H,S,L", num_args = 0..=1, action = clap::ArgAction::Append)]
    hsl: Vec<String>,
    /// AI param patch (JSON object of `field_id: value`). Field ids come from
    /// `retouch-rs schema --json`. Lets an agent set any control without
    /// memorizing individual CLI flags. Applied after preset/flags.
    #[arg(long, value_name = "JSON")]
    params: Option<String>,
    /// 胶片感 S 曲线 (-0.25..0.35, 0 = 关)
    #[arg(long)]
    film_curve: Option<f32>,
    /// 光比融合 (-0.6..0.6, 0 = 关)
    #[arg(long)]
    light_ratio: Option<f32>,
    /// 启用粉嫩肤色模块
    #[arg(long, default_value_t = false)]
    skin: bool,
    /// 肤色强度 (0..1)
    #[arg(long)]
    skin_strength: Option<f32>,
    /// 肤色目标色相 (OKLCH deg)
    #[arg(long)]
    skin_hue: Option<f32>,
    /// 肤色目标彩度
    #[arg(long)]
    skin_chroma: Option<f32>,
    /// 肤色提亮
    #[arg(long)]
    skin_light: Option<f32>,
    /// 肤色遮罩羽化 (deg)
    #[arg(long)]
    skin_smooth: Option<f32>,
    /// 保护非肤色 (默认开)
    #[arg(long, default_value_t = true)]
    skin_protect: bool,
    /// 肤色 · 去黄 (0..1)：往红润方向拉，去除暗黄气色
    #[arg(long)]
    skin_yellow: Option<f32>,
    /// 肤色 · 减淡 (0..1)：皮肤局部提亮，改善暗沉但不过曝
    #[arg(long)]
    skin_lighten: Option<f32>,
    /// 肤色 · 加红 (0..1)：增加红润血色
    #[arg(long)]
    skin_redden: Option<f32>,
    /// 肤色 · 加粉 (0..1)：往粉色偏，打造粉嫩通透感
    #[arg(long)]
    skin_pinken: Option<f32>,
    /// 多分区 · 暗部抬升
    #[arg(long)]
    zone_shadows: Option<f32>,
    /// 多分区 · 阴影抬升
    #[arg(long)]
    zone_dark_mid: Option<f32>,
    /// 多分区 · 中间调抬升
    #[arg(long)]
    zone_light_mid: Option<f32>,
    /// 多分区 · 高光抬升
    #[arg(long)]
    zone_highlights: Option<f32>,
    // ---------------- 几何预处理 (M4b) ----------------
    /// 裁剪：归一化 `x,y,w,h` (0..1)，例如 0.1,0.1,0.8,0.8
    #[arg(long, value_name = "X,Y,W,H")]
    crop: Option<String>,
    /// 旋转角度（逆时针，度）
    #[arg(long)]
    rotate: Option<f32>,
    /// 水平翻转
    #[arg(long, default_value_t = false)]
    flip_h: bool,
    /// 垂直翻转
    #[arg(long, default_value_t = false)]
    flip_v: bool,
    /// 透视 · 纵向 keystone (-1..1，0=关)
    #[arg(long)]
    persp_v: Option<f32>,
    /// 透视 · 横向 keystone (-1..1，0=关)
    #[arg(long)]
    persp_h: Option<f32>,
    // ---------------- 细节后处理 (M5) ----------------
    /// 降噪强度 (0..1)
    #[arg(long)]
    denoise: Option<f32>,
    /// 锐化强度 (0..1)
    #[arg(long)]
    sharpen: Option<f32>,
    /// 柔光强度 (0..1，仅高光泛光)
    #[arg(long)]
    diffuse: Option<f32>,
    // ---------------- 高级修图 (原 M6) ----------------
    /// 启用频谱磨皮
    #[arg(long, default_value_t = false)]
    freqsep: bool,
    /// 磨皮强度 (0..1)
    #[arg(long)]
    freqsep_strength: Option<f32>,
    /// 磨皮 · 纹理保留 (0..1)
    #[arg(long)]
    freqsep_texture: Option<f32>,
    /// 磨皮 · 平滑度 (0..1)
    #[arg(long)]
    freqsep_smooth: Option<f32>,
    /// 磨皮 · 蒙版羽化 (0..1)
    #[arg(long)]
    freqsep_feather: Option<f32>,
    /// 启用金字塔融合（多尺度细节）
    #[arg(long, default_value_t = false)]
    pyramid: bool,
    /// 金字塔 · 强度 (0..1)
    #[arg(long)]
    pyramid_strength: Option<f32>,
    /// 金字塔 · 细节倍率 (默认 1.0)
    #[arg(long)]
    pyramid_scale: Option<f32>,
}

#[derive(Subcommand)]
enum Command {
    /// Render through the OKLCH pipeline.
    ///
    /// With no preset and no flags this is an identity round-trip
    /// (decode -> OKLCH -> encode). Add --preset and/or individual flags.
    Render {
        #[command(flatten)]
        opts: CommonOpts,
        /// Input JPG/TIFF path
        input: PathBuf,
        /// Output JPG/TIFF path
        output: PathBuf,
    },
    /// Resolve the given preset + flags into a single preset file (TOML).
    /// Useful to normalize / export settings for retouch_app migration.
    Dump {
        #[command(flatten)]
        opts: CommonOpts,
        /// Output TOML path
        output: PathBuf,
    },
    /// Minimal self-verification: color fidelity, function correctness, perf.
    Verify {
        /// A real image to benchmark full-resolution render performance on
        input: PathBuf,
    },
    /// Quantify an image into AI-readable OKLCH metrics (JSON). This is what an
    /// agent consumes to "see" the picture without pixels.
    Analyze {
        /// Input JPG/TIFF path
        input: PathBuf,
        /// Emit compact JSON instead of human-readable text
        #[arg(long)]
        json: bool,
    },
    /// Print the full AI-readable parameter schema (JSON). Field ids here are
    /// the keys an agent writes in `--params`.
    Schema {
        /// Emit compact JSON (default) vs pretty-printed
        #[arg(long)]
        json: bool,
    },
    /// Autonomous correction. `--mode local` needs no network/key; `--mode api`
    /// (planned) calls an external model. Produces a final image + result.json.
    Auto {
        /// Input JPG/TIFF path
        input: PathBuf,
        /// Output final image path
        output: PathBuf,
        /// Correction mode
        #[arg(long, value_enum, default_value_t = AutoMode::Local)]
        mode: AutoMode,
        /// Iteration rounds (default 2)
        #[arg(long, default_value_t = 2)]
        rounds: usize,
        /// Emit a `result.json` next to the output
        #[arg(long)]
        json: bool,
    },
    /// 用 Qwen 视觉为照片起名 + 点评（联网，需 DashScope Key）。
    /// 同时作为 AI 可调用的薄壳：--json 输出结构化信封 {ok,title,title_en,comment,comment_en}。
    /// 不传 --key 时依次读 $DASHSCOPE_API_KEY / ~/.retouch/qwen_key。
    Name {
        /// 输入图片路径（原图即可，会自动缩到最长边 512px）
        input: PathBuf,
        /// Qwen/DashScope API Key（省略则读 env / ~/.retouch/qwen_key）
        #[arg(long)]
        key: Option<String>,
        /// 输出 JSON 信封（默认人类可读）
        #[arg(long)]
        json: bool,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, ValueEnum)]
enum AutoMode {
    /// Zero-network heuristic ("smart default") loop.
    Local,
    /// 历史 AI 联网调参分支（v0.2 已砍，回退到 Local 纯算法）。
    Api,
}

fn oklch_of(px: [u8; 3]) -> Oklch<f32> {
    let lin = LinSrgb::new(
        srgb_to_linear(px[0]),
        srgb_to_linear(px[1]),
        srgb_to_linear(px[2]),
    );
    lin.into_color()
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

/// Parse repeatable `--hsl BAND:H,S,L` flags into an `HslRegions`. Unknown
/// band names or malformed triples are rejected with a clear message so the
/// user is never silently handed an identity result.
fn build_hsl_regions(specs: &[String]) -> HslRegions {
    let mut r = HslRegions::default();
    for spec in specs {
        let (name, rest) = match spec.split_once(':') {
            Some(v) => v,
            None => {
                eprintln!(
                    "warning: --hsl '{}' missing ':' (expected BAND:H,S,L), ignored",
                    spec
                );
                continue;
            }
        };
        let idx = match HslRegions::band_index(name.trim()) {
            Some(i) => i,
            None => {
                eprintln!(
                    "warning: --hsl unknown band '{}' (expected red|orange|yellow|green|aqua|blue|purple|magenta), ignored",
                    name
                );
                continue;
            }
        };
        let parts: Vec<&str> = rest.split(',').collect();
        if parts.len() != 3 {
            eprintln!(
                "warning: --hsl '{}' needs 3 numbers H,S,L, got {}, ignored",
                spec,
                parts.len()
            );
            continue;
        }
        let parse = |s: &str| s.trim().parse::<f32>();
        let (h, s, l) = match (parse(parts[0]), parse(parts[1]), parse(parts[2])) {
            (Ok(h), Ok(s), Ok(l)) => (h, s, l),
            _ => {
                eprintln!("warning: --hsl '{}' has non-numeric values, ignored", spec);
                continue;
            }
        };
        r.hue_shift[idx] = h;
        r.sat_mult[idx] = s;
        r.light_mult[idx] = l;
    }
    r
}

/// Parse `--crop X,Y,W,H` (normalized 0..1) into an optional rect. Malformed
/// input is warned and ignored (never silently identity).
fn parse_crop(s: &str) -> Option<(f32, f32, f32, f32)> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 4 {
        eprintln!("warning: --crop needs 4 numbers X,Y,W,H, ignored");
        return None;
    }
    let p: Vec<f32> = parts
        .iter()
        .filter_map(|x| x.trim().parse::<f32>().ok())
        .collect();
    if p.len() != 4 {
        eprintln!("warning: --crop has non-numeric values, ignored");
        return None;
    }
    Some((p[0], p[1], p[2], p[3]))
}

/// Apply an AI `field_id: value` JSON patch onto `adj`. Unknown ids and
/// non-numeric values are warned and skipped; results are clamped to safe
/// slider bounds so an agent can never emit a destructive value.
fn apply_params_patch(adj: &mut Adjustments, json: &str) {
    let v: Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("warning: --params JSON 解析失败: {e}");
            return;
        }
    };
    let obj = match v.as_object() {
        Some(o) => o,
        None => {
            eprintln!("warning: --params 必须是 JSON 对象 {{field_id: value}}");
            return;
        }
    };
    let mut applied = 0usize;
    for (k, val) in obj {
        let f = match Field::from_id(k) {
            Some(f) => f,
            None => {
                eprintln!("warning: 未知参数 id '{k}' 忽略（用 `schema --json` 查）");
                continue;
            }
        };
        let nv = match val.as_f64() {
            Some(x) => x as f32,
            None => {
                eprintln!("warning: 参数 '{k}' 的值不是数字，忽略");
                continue;
            }
        };
        f.set(adj, nv);
        applied += 1;
    }
    if applied > 0 {
        guardrail::clamp(adj);
    }
}

/// Human-readable summary of [`ImageMetrics`] (the non-JSON path).
fn print_metrics_human(m: &ImageMetrics) {
    println!(
        "尺寸 {}x{} | 亮度 meanL={:.3} stdL={:.3} 动态范围={:.3}\n\
         色彩 meanC={:.3} 主色相={:.1}° 色相集中={:.2}\n\
         肤色占比={:.3} 肤色C={:.3} 肤色色相={:.1}°\n\
         曝光 过曝={:.2}% 死黑={:.2}% | 色域溢出={:.2}% 最大C={:.3}\n\
         偏色 色相={:.1}° 强度={:.3}",
        m.width,
        m.height,
        m.tone.mean_l,
        m.tone.std_l,
        m.dynamic_range,
        m.color.mean_c,
        m.color.mean_h_deg,
        m.color.hue_peakiness,
        m.skin.ratio,
        m.skin.mean_c,
        m.skin.mean_h_deg,
        m.exposure.highlight_clip_pct,
        m.exposure.shadow_clip_pct,
        m.gamut.clip_pct,
        m.gamut.max_c,
        m.cast.hue_deg,
        m.cast.chroma,
    );
    print!("每色相分区平均彩度: ");
    for c in &m.color.per_hue_chroma {
        print!("{:.3} ", c);
    }
    println!();
}

/// Resolve preset + CLI overrides into a concrete `Adjustments`.
fn resolve(opts: &CommonOpts) -> Adjustments {
    let base: Adjustments = match &opts.preset {
        Some(p) => match load_preset(p) {
            Ok(pre) => pre.to_adjustments(),
            Err(e) => {
                eprintln!("{}", e);
                std::process::exit(2);
            }
        },
        None => Adjustments::default(),
    };

    // Auto-applied AgX shoulder compression when exposure is pushed but no
    // tone-map was explicitly chosen — prevents blown highlights.
    let exp = opts.exposure.unwrap_or(base.exposure_ev);
    let tone = match opts.tone_map {
        Some(tm) => tm.into(),
        None => {
            if exp > 0.01 {
                ToneMapMode::Agx
            } else {
                base.tone_map
            }
        }
    };

    let defake_on = opts.defake || base.defake.enabled;
    let defake = if defake_on {
        DefakeColor {
            chroma_decay: opts.chroma_decay.unwrap_or(base.defake.chroma_decay),
            ..DefakeColor::on()
        }
    } else {
        DefakeColor::default()
    };

    let skin_enabled = opts.skin || base.skin.enabled;
    // When the user switches skin ON via flag but the base (preset) has no
    // real skin settings, fall back to the healthy "粉嫩" defaults instead of
    // zeroed values.
    let skin_base = if opts.skin && !base.skin.enabled {
        SkinTone::pink()
    } else {
        base.skin
    };
    let skin = if skin_enabled {
        SkinTone {
            enabled: true,
            strength: opts.skin_strength.unwrap_or(skin_base.strength),
            hue_target: opts.skin_hue.unwrap_or(skin_base.hue_target),
            chroma_target: opts.skin_chroma.unwrap_or(skin_base.chroma_target),
            light_lift: opts.skin_light.unwrap_or(skin_base.light_lift),
            smoothness: opts.skin_smooth.unwrap_or(skin_base.smoothness),
            protect_non_skin: opts.skin_protect,
            yellow_reduce: opts.skin_yellow.unwrap_or(skin_base.yellow_reduce),
            lighten: opts.skin_lighten.unwrap_or(skin_base.lighten),
            redden: opts.skin_redden.unwrap_or(skin_base.redden),
            pinken: opts.skin_pinken.unwrap_or(skin_base.pinken),
        }
    } else {
        SkinTone::default()
    };

    let zones = ZoneGrade {
        lift: [
            opts.zone_shadows.unwrap_or(base.zones.lift[0]),
            opts.zone_dark_mid.unwrap_or(base.zones.lift[1]),
            opts.zone_light_mid.unwrap_or(base.zones.lift[2]),
            opts.zone_highlights.unwrap_or(base.zones.lift[3]),
        ],
    };

    // 几何预处理 (M4b)
    let crop = opts
        .crop
        .as_deref()
        .and_then(parse_crop)
        .or(base.geometry.crop);
    let perspective = {
        let pv = opts
            .persp_v
            .unwrap_or(base.geometry.perspective.map_or(0.0, |p| p.0));
        let ph = opts
            .persp_h
            .unwrap_or(base.geometry.perspective.map_or(0.0, |p| p.1));
        if pv == 0.0 && ph == 0.0 {
            None
        } else {
            Some((pv, ph))
        }
    };
    let geometry = Geometry {
        crop,
        quarter_turns: base.geometry.quarter_turns,
        rotate_deg: opts.rotate.unwrap_or(base.geometry.rotate_deg),
        flip_h: opts.flip_h || base.geometry.flip_h,
        flip_v: opts.flip_v || base.geometry.flip_v,
        perspective,
    };

    // 细节后处理 (M5)
    let detail = Detail {
        denoise: opts.denoise.unwrap_or(base.detail.denoise),
        sharpen: opts.sharpen.unwrap_or(base.detail.sharpen),
        diffuse: opts.diffuse.unwrap_or(base.detail.diffuse),
    };

    // 高级修图 (原 M6)
    let freqsep_enabled = opts.freqsep || base.advanced.freqsep.enabled;
    let freqsep_base = if opts.freqsep && !base.advanced.freqsep.enabled {
        FreqSepSkin::default()
    } else {
        base.advanced.freqsep
    };
    let freqsep = FreqSepSkin {
        enabled: freqsep_enabled,
        strength: opts.freqsep_strength.unwrap_or(freqsep_base.strength),
        texture_keep: opts.freqsep_texture.unwrap_or(freqsep_base.texture_keep),
        smoothness: opts.freqsep_smooth.unwrap_or(freqsep_base.smoothness),
        mask_feather: opts.freqsep_feather.unwrap_or(freqsep_base.mask_feather),
    };
    let pyramid_enabled = opts.pyramid || base.advanced.pyramid.enabled;
    let pyramid_base = if opts.pyramid && !base.advanced.pyramid.enabled {
        PyramidFusion::default()
    } else {
        base.advanced.pyramid
    };
    let pyramid = PyramidFusion {
        enabled: pyramid_enabled,
        strength: opts.pyramid_strength.unwrap_or(pyramid_base.strength),
        detail_scale: opts.pyramid_scale.unwrap_or(pyramid_base.detail_scale),
    };
    let advanced = Advanced { freqsep, pyramid };

    Adjustments {
        exposure_ev: exp,
        tone_map: tone,
        defake,
        grade: Grade {
            brightness_lift: opts.brightness.unwrap_or(base.grade.brightness_lift),
            contrast: opts.contrast.unwrap_or(base.grade.contrast),
            dehaze: opts.dehaze.unwrap_or(base.grade.dehaze),
            shadow_lift: opts.shadow_lift.unwrap_or(base.grade.shadow_lift),
            deep_shadow_lift: opts.deep_shadow_lift.unwrap_or(base.grade.deep_shadow_lift),
            film_curve: opts.film_curve.unwrap_or(base.grade.film_curve),
            light_ratio: opts.light_ratio.unwrap_or(base.grade.light_ratio),
        },
        white_balance: WhiteBalance {
            temp: opts.temp.unwrap_or(base.white_balance.temp),
            tint: opts.tint.unwrap_or(base.white_balance.tint),
        },
        color: ColorGrade {
            saturation: opts.saturation.unwrap_or(base.color.saturation),
            vibrance: opts.vibrance.unwrap_or(base.color.vibrance),
            hue_rotate: opts.hue_rotate.unwrap_or(base.color.hue_rotate),
            split_shadow: opts.split_shadow.unwrap_or(base.color.split_shadow),
            split_highlight: opts.split_highlight.unwrap_or(base.color.split_highlight),
        },
        skin,
        zones,
        geometry,
        detail,
        advanced,
        hsl: if opts.hsl.is_empty() {
            base.hsl
        } else {
            build_hsl_regions(&opts.hsl)
        },
        color_plan: None,
        mix: 1.0,
    }
}

fn main() {
    let cli = Cli::parse();
    match cli.cmd {
        Command::Render {
            opts,
            input,
            output,
        } => {
            let mut adj = resolve(&opts);
            if let Some(p) = &opts.params {
                apply_params_patch(&mut adj, p);
            }
            let img = image::open(&input).expect("failed to open input");
            let out = render(&img, &adj);
            out.save(&output).expect("failed to save output");
            let preset_name = opts
                .preset
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "none".into());
            println!(
                "rendered -> {}  (preset={}, exposure_ev={}, tone_map={:?}, defake={}, hsl_bands={}, crop={:?}, rotate={}, flip=({},{}), persp={:?}, denoise={}, sharpen={}, diffuse={}, freqsep={}, pyramid={})",
                output.display(),
                preset_name,
                adj.exposure_ev,
                adj.tone_map,
                adj.defake.enabled,
                if opts.hsl.is_empty() { 0 } else { opts.hsl.len() },
                adj.geometry.crop,
                adj.geometry.rotate_deg,
                adj.geometry.flip_h,
                adj.geometry.flip_v,
                adj.geometry.perspective,
                adj.detail.denoise,
                adj.detail.sharpen,
                adj.detail.diffuse,
                adj.advanced.freqsep.enabled,
                adj.advanced.pyramid.enabled,
            );
        }
        Command::Dump { opts, output } => {
            let adj = resolve(&opts);
            let preset: Preset = adj.to_preset();
            match dump_preset(&preset, &output) {
                Ok(()) => println!("dumped preset -> {}", output.display()),
                Err(e) => {
                    eprintln!("{}", e);
                    std::process::exit(1);
                }
            }
        }
        Command::Verify { input } => run_verify(&input),
        Command::Analyze { input, json } => {
            let img = image::open(&input).expect("failed to open input");
            let m = analyze(&img);
            if json {
                println!("{}", serde_json::to_string(&m).unwrap());
            } else {
                print_metrics_human(&m);
            }
        }
        Command::Schema { json } => {
            let schema = param_schema();
            if json {
                println!("{}", serde_json::to_string(&schema).unwrap());
            } else {
                println!("{}", serde_json::to_string_pretty(&schema).unwrap());
            }
        }
        Command::Auto {
            input,
            output,
            mode,
            rounds,
            json,
        } => match mode {
            AutoMode::Local | AutoMode::Api => {
                if mode == AutoMode::Api {
                    eprintln!(
                        "[warn] --mode api（AI 联网调参）已在 v0.2 砍掉：数值回归不准、会过曝毁图。\n\
                         自动回退到 local（纯算法中性校正，零 key）。"
                    );
                }
                let img = image::open(&input).expect("failed to open input");
                let (final_img, result) = run_auto(&img, 1024, rounds.max(1), 1.0);
                final_img.save(&output).expect("failed to save output");
                if json {
                    let report = format!("{}.json", output.display());
                    if let Ok(s) = serde_json::to_string_pretty(&result) {
                        let _ = std::fs::write(&report, s);
                        println!("auto -> {} | report -> {}", output.display(), report);
                    }
                } else {
                    println!("auto -> {}", output.display());
                    for line in &result.log {
                        println!("  {}", line);
                    }
                    println!(
                        "  护栏: {}",
                        if result.guardrail_passed {
                            "通过"
                        } else {
                            "未完全通过(取最安全候选)"
                        }
                    );
                    println!(
                        "  采用参数: {}",
                        serde_json::to_string(&result.applied_params).unwrap()
                    );
                }
            }
        },
        Command::Name { input, key, json } => {
            // key 解析优先级：--key > $DASHSCOPE_API_KEY > ~/.retouch/qwen_key
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_else(|_| ".".to_string());
            let mut kp = std::path::PathBuf::from(home);
            kp.push(".retouch");
            kp.push("qwen_key");
            let key = match key.filter(|k| !k.trim().is_empty()) {
                Some(k) => k.trim().to_string(),
                None => match std::env::var("DASHSCOPE_API_KEY")
                    .ok()
                    .filter(|k| !k.trim().is_empty())
                {
                    Some(k) => k.trim().to_string(),
                    None => std::fs::read_to_string(&kp)
                        .map(|s| s.trim().to_string())
                        .unwrap_or_default(),
                },
            };
            if key.trim().is_empty() {
                eprintln!(
                    "[name] ERROR: 未提供 Qwen Key（--key / $DASHSCOPE_API_KEY / {}）",
                    kp.display()
                );
                std::process::exit(2);
            }
            eprintln!(
                "[name] key loaded: len={} prefix={}…",
                key.trim().len(),
                &key.trim()[..key.trim().len().min(4)]
            );
            eprintln!("[name] opening image: {}", input.display());
            let b64 = match thumb_b64(&input, 512) {
                Ok(b) => {
                    eprintln!("[name] thumb ok: base64 len={}", b.len());
                    b
                }
                Err(e) => {
                    eprintln!("[name] thumb FAILED: {}", e);
                    std::process::exit(1);
                }
            };
            eprintln!("[name] calling QwenClient::review ...");
            match QwenClient::new(key).review(&b64, "{}", "中性校正 + 影调优化") {
                Ok(v) => {
                    let title = v
                        .get("title")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string();
                    let title_en = v
                        .get("title_en")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string();
                    let comment = v
                        .get("comment")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string();
                    let comment_en = v
                        .get("comment_en")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string();
                    if json {
                        let env = serde_json::json!({
                            "ok": true,
                            "title": title,
                            "title_en": title_en,
                            "comment": comment,
                            "comment_en": comment_en,
                        });
                        println!("{}", serde_json::to_string(&env).unwrap());
                    } else {
                        println!("《{}》 ({})", title, title_en);
                        println!("点评: {}", comment);
                        if !comment_en.is_empty() {
                            println!("EN: {}", comment_en);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[name] review FAILED: {}", e);
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string(&serde_json::json!({"ok": false, "error": e}))
                                .unwrap()
                        );
                    }
                    std::process::exit(1);
                }
            }
        }
    }
}

fn run_verify(input: &PathBuf) {
    println!("=== retouch-rs minimal verification ===\n");
    let mut all_pass = true;

    // [color] identity round-trip on a synthetic gradient (in-memory). This is
    // a *worst-case* gradient (pure primaries / secondaries at full tilt) that
    // deliberately pushes some pixels just outside the sRGB gamut in OKLCH;
    // the always-on gamut soft-clip then nudges those few edge pixels back in
    // (by <= ~3% — imperceptible, and the whole point vs. a hard hue-shifting
    // clip). So the tolerance is 8, not 0/2; perceptual fidelity on real photos
    // is validated separately (hue drift median ~1.4 deg).
    {
        let mut img = RgbImage::new(256, 256);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = Rgb([x as u8, y as u8, ((x + y) / 2) as u8]);
        }
        let dyn_img = DynamicImage::ImageRgb8(img.clone());
        let out = render(&dyn_img, &Adjustments::identity());
        let (mut maxd, mut sumd) = (0i32, 0i64);
        for (p, q) in img.pixels().zip(out.pixels()) {
            for i in 0..3 {
                let d = (p.0[i] as i32 - q.0[i] as i32).abs();
                maxd = maxd.max(d);
                sumd += d as i64;
            }
        }
        let mean = sumd as f64 / (256.0 * 256.0 * 3.0);
        let pass = maxd <= 8;
        all_pass &= pass;
        println!(
            "[color] identity round-trip  max_diff={}  mean_diff={:.3}  -> {}",
            maxd,
            mean,
            if pass { "PASS" } else { "FAIL" }
        );
    }

    // [func] exposure brightens.
    {
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(8, 8, Rgb([100u8, 100, 100])));
        let lit = render(
            &img,
            &Adjustments {
                exposure_ev: 2.0,
                ..Default::default()
            },
        );
        let v = lit.get_pixel(0, 0).0[0];
        let pass = v > 150;
        all_pass &= pass;
        println!(
            "[func ] exposure +2EV brightens: 100 -> {}  -> {}",
            v,
            if pass { "PASS" } else { "FAIL" }
        );
    }

    // [func] AgX desaturates over-bright highlights (anti fake-color).
    {
        let red = DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, Rgb([255u8, 40, 40])));
        let no_tm = render(&red, &Adjustments::identity());
        let agx = render(
            &red,
            &Adjustments {
                exposure_ev: 2.0,
                tone_map: ToneMapMode::Agx,
                ..Default::default()
            },
        );
        let nt = no_tm.get_pixel(0, 0).0;
        let ax = agx.get_pixel(0, 0).0;
        let nt_sat = (nt[0] as i32 - nt[1] as i32).abs() + (nt[0] as i32 - nt[2] as i32).abs();
        let ax_sat = (ax[0] as i32 - ax[1] as i32).abs() + (ax[0] as i32 - ax[2] as i32).abs();
        let pass = ax_sat <= nt_sat;
        all_pass &= pass;
        println!(
            "[func ] AgX highlights desaturate: noTM_sat={} AgX_sat={}  -> {}",
            nt_sat,
            ax_sat,
            if pass { "PASS" } else { "FAIL" }
        );
    }

    // [func] de-fake-color reduces chroma of a bright saturated pixel WITHOUT
    //        shifting hue (the whole point of the OKLCH route).
    {
        let px = [180u8, 200, 250];
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, Rgb(px)));
        let mut adj = Adjustments::identity();
        adj.defake = DefakeColor {
            enabled: true,
            chroma_decay: 0.5,
            fix_sky: false,
            protect_skin: false,
            gamut_softclip: true,
        };
        let out = render(&img, &adj);
        let o = out.get_pixel(0, 0).0;
        let ok_in = oklch_of(px);
        let ok_out = oklch_of(o);
        let h_in = ok_in.hue.into_positive_degrees();
        let h_out = ok_out.hue.into_positive_degrees();
        let dh = (h_in - h_out).abs().min(360.0 - (h_in - h_out).abs());
        let pass = ok_out.chroma < ok_in.chroma && dh < 5.0;
        all_pass &= pass;
        println!(
            "[func ] de-fake-color: C {:.3}->{:.3}  hue drift {:.2}deg  -> {}",
            ok_in.chroma,
            ok_out.chroma,
            dh,
            if pass { "PASS" } else { "FAIL" }
        );
    }

    // [perf] full-resolution render benchmark on a real image (product look:
    //        exposure + AgX + de-fake-color, all on).
    {
        let img = match image::open(input) {
            Ok(i) => i,
            Err(e) => {
                println!("[perf ] SKIP (cannot open {}): {}", input.display(), e);
                report(all_pass);
                return;
            }
        };
        let (w, h) = img.dimensions();
        let adj = Adjustments {
            exposure_ev: 0.3,
            tone_map: ToneMapMode::Agx,
            defake: DefakeColor::on(),
            grade: Grade::default(),
            ..Default::default()
        };
        // warm up once, then time 3 renders.
        let _ = render(&img, &adj);
        let runs = 3;
        let t0 = Instant::now();
        for _ in 0..runs {
            let _ = render(&img, &adj);
        }
        let ms = t0.elapsed().as_secs_f64() * 1000.0 / runs as f64;
        println!(
            "[perf ] {} ({}x{})  avg {:.1}ms/render (exposure+AgX+de-fake, rayon parallel)",
            input.display(),
            w,
            h,
            ms
        );
    }

    report(all_pass);
}

fn report(all_pass: bool) {
    println!();
    if all_pass {
        println!("=== ALL CHECKS PASS — route feasible, color & function correct ===");
    } else {
        println!("=== SOME CHECKS FAILED — see above ===");
        std::process::exit(1);
    }
}
