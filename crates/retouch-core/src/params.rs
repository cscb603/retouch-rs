//! Parameter metadata registry (M5). Drives a metadata-driven, perceptually
//! mapped GUI: each `ParamSpec` links a human label to an `Adjustments` field,
//! the perceptual `CurveKind`, and the slider's neutral center / half-range.
//!
//! The GUI renders one smart slider per spec, converting between the uniform
//! slider position (what the user drags) and the raw pipeline value (what
//! `Adjustments` / presets store) via `perceptual::{slider_to_raw,raw_to_slider}`.
//! This keeps every control non-linear & human-vision-aligned, never pure-linear.

use crate::perceptual::{raw_to_slider, slider_to_raw, CurveKind};
use crate::pipeline::*;

/// A single adjustable scalar field inside `Adjustments`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Field {
    ExposureEv,
    // 胶片感 / 光比
    FilmCurve,
    LightRatio,
    BrightnessLift,
    Contrast,
    Dehaze,
    ShadowLift,
    DeepShadowLift,
    // 白平衡
    WBTemp,
    WBTint,
    // 色彩
    Saturation,
    Vibrance,
    HueRotate,
    SplitShadow,
    SplitHighlight,
    // 粉嫩肤色
    SkinStrength,
    SkinHue,
    SkinChroma,
    SkinLight,
    SkinYellowReduce,
    SkinLighten,
    SkinRedden,
    SkinPinken,
    // HSL 8 色相分区
    HslHue(usize),
    HslSat(usize),
    HslLight(usize),
    // 多分区亮度融合 (4 区)
    Zone(usize),
    // 几何预处理 (M4b)
    GeomRotate,
    GeomPerspV,
    GeomPerspH,
    // 细节后处理 (M5)
    DetailDenoise,
    DetailSharpen,
    DetailDiffuse,
    // 高级修图 (原 M6)
    FreqSepStrength,
    FreqSepTexture,
    FreqSepSmooth,
    FreqSepFeather,
    PyramidStrength,
    PyramidScale,
}

impl Field {
    /// Read the raw pipeline value of this field.
    pub fn get(&self, a: &Adjustments) -> f32 {
        match self {
            Field::ExposureEv => a.exposure_ev,
            Field::FilmCurve => a.grade.film_curve,
            Field::LightRatio => a.grade.light_ratio,
            Field::BrightnessLift => a.grade.brightness_lift,
            Field::Contrast => a.grade.contrast,
            Field::Dehaze => a.grade.dehaze,
            Field::ShadowLift => a.grade.shadow_lift,
            Field::DeepShadowLift => a.grade.deep_shadow_lift,
            Field::WBTemp => a.white_balance.temp,
            Field::WBTint => a.white_balance.tint,
            Field::Saturation => a.color.saturation,
            Field::Vibrance => a.color.vibrance,
            Field::HueRotate => a.color.hue_rotate,
            Field::SplitShadow => a.color.split_shadow,
            Field::SplitHighlight => a.color.split_highlight,
            Field::SkinStrength => a.skin.strength,
            Field::SkinHue => a.skin.hue_target,
            Field::SkinChroma => a.skin.chroma_target,
            Field::SkinLight => a.skin.light_lift,
            Field::SkinYellowReduce => a.skin.yellow_reduce,
            Field::SkinLighten => a.skin.lighten,
            Field::SkinRedden => a.skin.redden,
            Field::SkinPinken => a.skin.pinken,
            Field::HslHue(i) => a.hsl.hue_shift[*i],
            Field::HslSat(i) => a.hsl.sat_mult[*i],
            Field::HslLight(i) => a.hsl.light_mult[*i],
            Field::Zone(i) => a.zones.lift[*i],
            Field::GeomRotate => a.geometry.rotate_deg,
            Field::GeomPerspV => a.geometry.perspective.map_or(0.0, |p| p.0),
            Field::GeomPerspH => a.geometry.perspective.map_or(0.0, |p| p.1),
            Field::DetailDenoise => a.detail.denoise,
            Field::DetailSharpen => a.detail.sharpen,
            Field::DetailDiffuse => a.detail.diffuse,
            Field::FreqSepStrength => a.advanced.freqsep.strength,
            Field::FreqSepTexture => a.advanced.freqsep.texture_keep,
            Field::FreqSepSmooth => a.advanced.freqsep.smoothness,
            Field::FreqSepFeather => a.advanced.freqsep.mask_feather,
            Field::PyramidStrength => a.advanced.pyramid.strength,
            Field::PyramidScale => a.advanced.pyramid.detail_scale,
        }
    }

    /// Stable string id used as the JSON key for AI-facing param patches and
    /// the `schema` command. Indexed variants encode their index (e.g.
    /// `hsl_hue_0`, `zone_2`) so an agent can address every control uniquely.
    pub fn id(&self) -> String {
        match self {
            Field::ExposureEv => "exposure_ev".into(),
            Field::FilmCurve => "film_curve".into(),
            Field::LightRatio => "light_ratio".into(),
            Field::BrightnessLift => "brightness_lift".into(),
            Field::Contrast => "contrast".into(),
            Field::Dehaze => "dehaze".into(),
            Field::ShadowLift => "shadow_lift".into(),
            Field::DeepShadowLift => "deep_shadow_lift".into(),
            Field::WBTemp => "wb_temp".into(),
            Field::WBTint => "wb_tint".into(),
            Field::Saturation => "saturation".into(),
            Field::Vibrance => "vibrance".into(),
            Field::HueRotate => "hue_rotate".into(),
            Field::SplitShadow => "split_shadow".into(),
            Field::SplitHighlight => "split_highlight".into(),
            Field::SkinStrength => "skin_strength".into(),
            Field::SkinHue => "skin_hue".into(),
            Field::SkinChroma => "skin_chroma".into(),
            Field::SkinLight => "skin_light".into(),
            Field::SkinYellowReduce => "skin_yellow_reduce".into(),
            Field::SkinLighten => "skin_lighten".into(),
            Field::SkinRedden => "skin_redden".into(),
            Field::SkinPinken => "skin_pinken".into(),
            Field::HslHue(i) => format!("hsl_hue_{}", i),
            Field::HslSat(i) => format!("hsl_sat_{}", i),
            Field::HslLight(i) => format!("hsl_light_{}", i),
            Field::Zone(i) => format!("zone_{}", i),
            Field::GeomRotate => "geom_rotate".into(),
            Field::GeomPerspV => "geom_persp_v".into(),
            Field::GeomPerspH => "geom_persp_h".into(),
            Field::DetailDenoise => "detail_denoise".into(),
            Field::DetailSharpen => "detail_sharpen".into(),
            Field::DetailDiffuse => "detail_diffuse".into(),
            Field::FreqSepStrength => "freqsep_strength".into(),
            Field::FreqSepTexture => "freqsep_texture".into(),
            Field::FreqSepSmooth => "freqsep_smooth".into(),
            Field::FreqSepFeather => "freqsep_feather".into(),
            Field::PyramidStrength => "pyramid_strength".into(),
            Field::PyramidScale => "pyramid_scale".into(),
        }
    }

    /// Parse a stable id (see [`Field::id`]) back into a `Field`. Used by the
    /// CLI/AI `--params` patch path. Returns `None` for unknown ids.
    pub fn from_id(s: &str) -> Option<Field> {
        use Field::*;
        match s {
            "exposure_ev" => Some(ExposureEv),
            "film_curve" => Some(FilmCurve),
            "light_ratio" => Some(LightRatio),
            "brightness_lift" => Some(BrightnessLift),
            "contrast" => Some(Contrast),
            "dehaze" => Some(Dehaze),
            "shadow_lift" => Some(ShadowLift),
            "deep_shadow_lift" => Some(DeepShadowLift),
            "wb_temp" => Some(WBTemp),
            "wb_tint" => Some(WBTint),
            "saturation" => Some(Saturation),
            "vibrance" => Some(Vibrance),
            "hue_rotate" => Some(HueRotate),
            "split_shadow" => Some(SplitShadow),
            "split_highlight" => Some(SplitHighlight),
            "skin_strength" => Some(SkinStrength),
            "skin_hue" => Some(SkinHue),
            "skin_chroma" => Some(SkinChroma),
            "skin_light" => Some(SkinLight),
            "skin_yellow_reduce" => Some(SkinYellowReduce),
            "skin_lighten" => Some(SkinLighten),
            "skin_redden" => Some(SkinRedden),
            "skin_pinken" => Some(SkinPinken),
            "geom_rotate" => Some(GeomRotate),
            "geom_persp_v" => Some(GeomPerspV),
            "geom_persp_h" => Some(GeomPerspH),
            "detail_denoise" => Some(DetailDenoise),
            "detail_sharpen" => Some(DetailSharpen),
            "detail_diffuse" => Some(DetailDiffuse),
            "freqsep_strength" => Some(FreqSepStrength),
            "freqsep_texture" => Some(FreqSepTexture),
            "freqsep_smooth" => Some(FreqSepSmooth),
            "freqsep_feather" => Some(FreqSepFeather),
            "pyramid_strength" => Some(PyramidStrength),
            "pyramid_scale" => Some(PyramidScale),
            _ => {
                // indexed variants
                if let Some(rest) = s.strip_prefix("hsl_hue_") {
                    rest.parse::<usize>().ok().map(HslHue)
                } else if let Some(rest) = s.strip_prefix("hsl_sat_") {
                    rest.parse::<usize>().ok().map(HslSat)
                } else if let Some(rest) = s.strip_prefix("hsl_light_") {
                    rest.parse::<usize>().ok().map(HslLight)
                } else if let Some(rest) = s.strip_prefix("zone_") {
                    rest.parse::<usize>().ok().map(Zone)
                } else {
                    None
                }
            }
        }
    }

    /// Write the raw pipeline value of this field.
    pub fn set(&self, a: &mut Adjustments, v: f32) {
        match self {
            Field::ExposureEv => a.exposure_ev = v,
            Field::FilmCurve => a.grade.film_curve = v,
            Field::LightRatio => a.grade.light_ratio = v,
            Field::BrightnessLift => a.grade.brightness_lift = v,
            Field::Contrast => a.grade.contrast = v,
            Field::Dehaze => a.grade.dehaze = v,
            Field::ShadowLift => a.grade.shadow_lift = v,
            Field::DeepShadowLift => a.grade.deep_shadow_lift = v,
            Field::WBTemp => a.white_balance.temp = v,
            Field::WBTint => a.white_balance.tint = v,
            Field::Saturation => a.color.saturation = v,
            Field::Vibrance => a.color.vibrance = v,
            Field::HueRotate => a.color.hue_rotate = v,
            Field::SplitShadow => a.color.split_shadow = v,
            Field::SplitHighlight => a.color.split_highlight = v,
            Field::SkinStrength => a.skin.strength = v,
            Field::SkinHue => a.skin.hue_target = v,
            Field::SkinChroma => a.skin.chroma_target = v,
            Field::SkinLight => a.skin.light_lift = v,
            Field::SkinYellowReduce => a.skin.yellow_reduce = v,
            Field::SkinLighten => a.skin.lighten = v,
            Field::SkinRedden => a.skin.redden = v,
            Field::SkinPinken => a.skin.pinken = v,
            Field::HslHue(i) => a.hsl.hue_shift[*i] = v,
            Field::HslSat(i) => a.hsl.sat_mult[*i] = v,
            Field::HslLight(i) => a.hsl.light_mult[*i] = v,
            Field::Zone(i) => a.zones.lift[*i] = v,
            Field::GeomRotate => a.geometry.rotate_deg = v,
            Field::GeomPerspV => {
                let h = a.geometry.perspective.map_or(0.0, |p| p.1);
                a.geometry.perspective = Some((v, h));
            }
            Field::GeomPerspH => {
                let vv = a.geometry.perspective.map_or(0.0, |p| p.0);
                a.geometry.perspective = Some((vv, v));
            }
            Field::DetailDenoise => a.detail.denoise = v,
            Field::DetailSharpen => a.detail.sharpen = v,
            Field::DetailDiffuse => a.detail.diffuse = v,
            // 注意：set 时**不**联动 enabled —— enabled 由调用方显式控制
            // （UI 拖滑块时自己设 enabled，见 main.rs；手动/预设也显式设）。
            // 否则 guardrail::clamp 遍历 registry 时会因默认 strength=0.5>0 把
            // 已关闭的磨皮/融合误开，导致自动路径输出柔光模糊图（2026-07-19 修复）。
            Field::FreqSepStrength => {
                a.advanced.freqsep.strength = v;
            }
            Field::FreqSepTexture => a.advanced.freqsep.texture_keep = v,
            Field::FreqSepSmooth => a.advanced.freqsep.smoothness = v,
            Field::FreqSepFeather => a.advanced.freqsep.mask_feather = v,
            Field::PyramidStrength => {
                a.advanced.pyramid.strength = v;
            }
            Field::PyramidScale => a.advanced.pyramid.detail_scale = v,
        }
    }
}

/// Static description of one slider control.
#[derive(Clone)]
pub struct ParamSpec {
    /// Human label (Chinese), shown next to the slider.
    pub label: String,
    /// One-sentence, friend-style explanation shown on hover.
    pub tooltip: &'static str,
    /// Which field it drives.
    pub field: Field,
    /// Perceptual curve kind.
    pub curve: CurveKind,
    /// True: slider space is `[-1, 1]` (bipolar, neutral at 0). False: `[0, 1]`
    /// (unipolar, neutral at 0).
    pub bipolar: bool,
    /// Raw value at the slider's neutral point.
    pub center: f32,
    /// Raw half-range (bipolar) or full range from center (unipolar).
    pub half: f32,
    /// Unit suffix for the live readout (e.g. "EV", "×", "°").
    pub unit: &'static str,
    /// Decimal places for the live readout.
    pub dec: usize,
}

impl ParamSpec {
    /// Convert a uniform slider position (already in `[-1, 1]` for both bipolar
    /// and unipolar callers) to the raw pipeline value.
    pub fn to_raw(&self, pos: f32) -> f32 {
        slider_to_raw(self.curve, self.center, self.half, pos)
    }

    /// Convert a raw pipeline value back to a uniform slider position `[-1, 1]`.
    pub fn to_pos(&self, raw: f32) -> f32 {
        raw_to_slider(self.curve, self.center, self.half, raw)
    }

    /// Format the live effective-value readout for a raw value in plain language.
    pub fn fmt(&self, raw: f32) -> String {
        format!("效果值 {:.*}{}", self.dec, raw, self.unit)
    }
}

/// Build the full control registry (used by the GUI). Grouped logically; the
/// GUI renders each spec as one smart, perceptually-mapped slider with a live
/// effective-value readout.
pub fn registry() -> Vec<ParamSpec> {
    let mut v: Vec<ParamSpec> = Vec::new();

    // 曝光
    v.push(ParamSpec {
        label: "曝光".into(),
        tooltip: "像相机曝光补偿：整体调亮或压暗。EV 单位本身按人眼对数感知设计，每档亮度变化量相同；推到 + 时建议配合 AgX/Filmic 保护高光",
        field: Field::ExposureEv,
        curve: CurveKind::Linear, // EV 本身已符合人眼（每档=同等感知亮度差）
        bipolar: true,
        center: 0.0,
        half: 3.0,
        unit: " EV",
        dec: 2,
    });

    // 胶片感 / 光比（核心智能控制）
    v.push(ParamSpec {
        label: "胶片曲线".into(),
        tooltip: "胶片感的 S 曲线：暗部不黑死、高光不刺眼，过渡柔和。滑块采用 SoftKnee 感知映射，越往两端效果越衰减，避免一下调过头",
        field: Field::FilmCurve,
        curve: CurveKind::SoftKnee, // 中心中和 + 两端加权衰减
        bipolar: true,
        center: 0.0,
        half: 0.3,
        unit: "",
        dec: 2,
    });
    v.push(ParamSpec {
        label: "光比融合".into(),
        tooltip: "柔化亮部与暗部的反差，让照片光影过渡更自然。感知映射：小幅拖动即可看到明显变化， extreme 位置自动放缓",
        field: Field::LightRatio,
        curve: CurveKind::SoftKnee,
        bipolar: true,
        center: 0.0,
        half: 0.6,
        unit: "",
        dec: 2,
    });
    v.push(ParamSpec {
        label: "提亮".into(),
        tooltip: "整体画面往上提亮，高光处会自动保护不过曝。越往右提亮的增量按人眼习惯衰减，不易过曝",
        field: Field::BrightnessLift,
        curve: CurveKind::SoftKnee,
        bipolar: false,
        center: 0.0,
        half: 1.0,
        unit: "",
        dec: 2,
    });
    v.push(ParamSpec {
        label: "对比".into(),
        tooltip: "加大或减小明暗反差，中间调不会生硬断裂。SoftKnee 映射让极端位置的影响逐渐减弱",
        field: Field::Contrast,
        curve: CurveKind::SoftKnee,
        bipolar: true,
        center: 0.0,
        half: 1.0,
        unit: "",
        dec: 2,
    });
    v.push(ParamSpec {
        label: "去雾 / 通透".into(),
        tooltip: "去掉灰雾感，让照片更通透、颜色更清爽。感知映射，推到底也不会把画面拉花",
        field: Field::Dehaze,
        curve: CurveKind::SoftKnee,
        bipolar: false,
        center: 0.0,
        half: 1.0,
        unit: "",
        dec: 2,
    });
    v.push(ParamSpec {
        label: "提亮暗部".into(),
        tooltip: "把暗部往上拉，显出细节，同时避免整片灰掉。感知映射：接近上限时提升量逐渐衰减，保持自然",
        field: Field::ShadowLift,
        curve: CurveKind::SoftKnee,
        bipolar: false,
        center: 0.0,
        half: 1.0,
        unit: "",
        dec: 2,
    });
    v.push(ParamSpec {
        label: "提亮黑位".into(),
        tooltip: "把最黑的地方提起来，拯救死黑但保留层次。感知映射防止黑位被提成灰片",
        field: Field::DeepShadowLift,
        curve: CurveKind::SoftKnee,
        bipolar: false,
        center: 0.0,
        half: 1.0,
        unit: "",
        dec: 2,
    });

    // 白平衡
    v.push(ParamSpec {
        label: "色温 (暖+)".into(),
        tooltip: "往右变暖偏黄，往左变冷偏蓝，修正环境光色温",
        field: Field::WBTemp,
        curve: CurveKind::SoftKnee,
        bipolar: true,
        center: 0.0,
        half: 1.0,
        unit: "",
        dec: 2,
    });
    v.push(ParamSpec {
        label: "色调 (品红+)".into(),
        tooltip: "往右偏品红，往左偏绿，常用来修肤色偏绿",
        field: Field::WBTint,
        curve: CurveKind::SoftKnee,
        bipolar: true,
        center: 0.0,
        half: 1.0,
        unit: "",
        dec: 2,
    });

    // 色彩风格
    v.push(ParamSpec {
        label: "饱和度".into(),
        tooltip: "所有颜色一起变艳或变淡，1.0 为原图。采用分区感知：中间调响应最强，暗部/高光自然衰减，避免数码生硬感",
        field: Field::Saturation,
        curve: CurveKind::LogSat, // 对数：等比感知，中心=1.0 不变
        bipolar: true,
        center: 1.0,
        half: 3.0,
        unit: "×",
        dec: 2,
    });
    v.push(ParamSpec {
        label: "鲜艳度".into(),
        tooltip: "智能加饱和：优先让低彩度区域变鲜活，同时避开肤色并在暗部/高光自然衰减",
        field: Field::Vibrance,
        curve: CurveKind::SoftKnee,
        bipolar: true,
        center: 0.0,
        half: 1.0,
        unit: "",
        dec: 2,
    });
    v.push(ParamSpec {
        label: "色相旋转".into(),
        tooltip: "整张照片在色相环上旋转，创意调色用",
        field: Field::HueRotate,
        curve: CurveKind::Linear, // 角度本身线性即可
        bipolar: true,
        center: 0.0,
        half: 180.0,
        unit: "°",
        dec: 0,
    });
    v.push(ParamSpec {
        label: "暗部染色".into(),
        tooltip: "只给阴影区域加一层颜色，不影响亮部",
        field: Field::SplitShadow,
        curve: CurveKind::Linear,
        bipolar: true,
        center: 0.0,
        half: 180.0,
        unit: "°",
        dec: 0,
    });
    v.push(ParamSpec {
        label: "高光染色".into(),
        tooltip: "只给高光区域加一层颜色，如夕阳金、冷高光",
        field: Field::SplitHighlight,
        curve: CurveKind::Linear,
        bipolar: true,
        center: 0.0,
        half: 180.0,
        unit: "°",
        dec: 0,
    });

    // 粉嫩肤色
    v.push(ParamSpec {
        label: "肤色强度".into(),
        tooltip: "肤色模块整体作用大小，0 为不处理",
        field: Field::SkinStrength,
        curve: CurveKind::SoftKnee,
        bipolar: false,
        center: 0.0,
        half: 1.0,
        unit: "",
        dec: 2,
    });
    v.push(ParamSpec {
        label: "肤色色相 (粉嫩)".into(),
        tooltip: "肤色往粉或红偏，打造健康气色",
        field: Field::SkinHue,
        curve: CurveKind::Linear,
        bipolar: true,
        center: 35.0,
        half: 30.0,
        unit: "°",
        dec: 0,
    });
    v.push(ParamSpec {
        label: "肤色彩度".into(),
        tooltip: "肤色红润程度，宁小勿大，避免塑料感",
        field: Field::SkinChroma,
        curve: CurveKind::SoftKnee,
        bipolar: false,
        center: 0.0,
        half: 0.2,
        unit: "",
        dec: 3,
    });
    v.push(ParamSpec {
        label: "肤色提亮".into(),
        tooltip: "局部提亮肤色，改善暗沉但保持五官立体",
        field: Field::SkinLight,
        curve: CurveKind::SoftKnee,
        bipolar: false,
        center: 0.0,
        half: 0.1,
        unit: "",
        dec: 3,
    });
    v.push(ParamSpec {
        label: "去黄".into(),
        tooltip: "肤色偏黄时往红润方向拉，只影响皮肤区域",
        field: Field::SkinYellowReduce,
        curve: CurveKind::SoftKnee,
        bipolar: false,
        center: 0.0,
        half: 1.0,
        unit: "",
        dec: 2,
    });
    v.push(ParamSpec {
        label: "减淡".into(),
        tooltip: "皮肤局部提亮，改善暗沉但不过曝",
        field: Field::SkinLighten,
        curve: CurveKind::SoftKnee,
        bipolar: false,
        center: 0.0,
        half: 1.0,
        unit: "",
        dec: 2,
    });
    v.push(ParamSpec {
        label: "加红".into(),
        tooltip: "增加肤色红润感，自然血色",
        field: Field::SkinRedden,
        curve: CurveKind::SoftKnee,
        bipolar: false,
        center: 0.0,
        half: 1.0,
        unit: "",
        dec: 2,
    });
    v.push(ParamSpec {
        label: "加粉".into(),
        tooltip: "肤色往粉嫩方向偏，适合女生/健康气色",
        field: Field::SkinPinken,
        curve: CurveKind::SoftKnee,
        bipolar: false,
        center: 0.0,
        half: 1.0,
        unit: "",
        dec: 2,
    });

    // HSL 8 色相分区
    let bands = ["红", "橙", "黄", "绿", "青", "蓝", "紫", "品红"];
    for (i, name) in bands.iter().enumerate() {
        v.push(ParamSpec {
            label: format!("{} · 色相", name),
            tooltip: "单独调整该颜色区域的色相，不影响其他颜色",
            field: Field::HslHue(i),
            curve: CurveKind::Linear,
            bipolar: true,
            center: 0.0,
            half: 90.0,
            unit: "°",
            dec: 0,
        });
        v.push(ParamSpec {
            label: format!("{} · 饱和", name),
            tooltip: "单独调整该颜色区域的鲜艳度，1.0 为原图",
            field: Field::HslSat(i),
            curve: CurveKind::LogSat,
            bipolar: true,
            center: 1.0,
            half: 3.0,
            unit: "×",
            dec: 2,
        });
        v.push(ParamSpec {
            label: format!("{} · 明度", name),
            tooltip: "单独提亮或压暗该颜色区域，1.0 为原图",
            field: Field::HslLight(i),
            curve: CurveKind::SoftKnee, // 中心=1.0 不变，两端缓动
            bipolar: true,
            center: 1.0,
            half: 0.5,
            unit: "×",
            dec: 2,
        });
    }

    // 多分区亮度融合 (4 区)
    let zones = ["暗部", "阴影", "中间调", "高光"];
    for (i, name) in zones.iter().enumerate() {
        v.push(ParamSpec {
            label: format!("分区 · {}", name),
            tooltip: "单独提亮或压暗该亮度区域，多版本融合无硬边",
            field: Field::Zone(i),
            curve: CurveKind::SoftKnee,
            bipolar: true,
            center: 0.0,
            half: 0.4,
            unit: "",
            dec: 2,
        });
    }

    // 几何预处理 (M4b)
    v.push(ParamSpec {
        label: "旋转".into(),
        tooltip: "任意角度旋转画面，90° 步进会自动对齐像素",
        field: Field::GeomRotate,
        curve: CurveKind::Linear,
        bipolar: true,
        center: 0.0,
        half: 45.0,
        unit: "°",
        dec: 0,
    });
    v.push(ParamSpec {
        label: "透视 · 纵".into(),
        tooltip: "修正上下方向的透视歪斜，如建筑前倾后仰",
        field: Field::GeomPerspV,
        curve: CurveKind::SoftKnee,
        bipolar: true,
        center: 0.0,
        half: 1.0,
        unit: "",
        dec: 2,
    });
    v.push(ParamSpec {
        label: "透视 · 横".into(),
        tooltip: "修正左右方向的透视歪斜，如建筑左右倾斜",
        field: Field::GeomPerspH,
        curve: CurveKind::SoftKnee,
        bipolar: true,
        center: 0.0,
        half: 1.0,
        unit: "",
        dec: 2,
    });

    // 细节后处理 (M5)
    v.push(ParamSpec {
        label: "降噪".into(),
        tooltip: "减少颗粒噪点，边缘会被保护，不会变糊",
        field: Field::DetailDenoise,
        curve: CurveKind::SoftKnee,
        bipolar: false,
        center: 0.0,
        half: 1.0,
        unit: "",
        dec: 2,
    });
    v.push(ParamSpec {
        label: "锐化".into(),
        tooltip: "增强边缘清晰度，自动避开平坦区域避免锐化噪点",
        field: Field::DetailSharpen,
        curve: CurveKind::SoftKnee,
        bipolar: false,
        center: 0.0,
        half: 1.0,
        unit: "",
        dec: 2,
    });
    v.push(ParamSpec {
        label: "柔光".into(),
        tooltip: "只让高光区域微微泛光，产生柔焦梦幻感",
        field: Field::DetailDiffuse,
        curve: CurveKind::SoftKnee,
        bipolar: false,
        center: 0.0,
        half: 1.0,
        unit: "",
        dec: 2,
    });

    // 高级修图 (原 M6)
    v.push(ParamSpec {
        label: "磨皮强度".into(),
        tooltip: "只影响皮肤区域，发丝、眼睛、背景完全不动",
        field: Field::FreqSepStrength,
        curve: CurveKind::SoftKnee,
        bipolar: false,
        center: 0.0,
        half: 1.0,
        unit: "",
        dec: 2,
    });
    v.push(ParamSpec {
        label: "磨皮 · 纹理保留".into(),
        tooltip: "保留毛孔纹理，越高越自然，越低越平滑",
        field: Field::FreqSepTexture,
        curve: CurveKind::SoftKnee,
        bipolar: false,
        center: 0.0,
        half: 1.0,
        unit: "",
        dec: 2,
    });
    v.push(ParamSpec {
        label: "磨皮 · 平滑度".into(),
        tooltip: "控制磨皮模糊范围，适度即可避免塑料感",
        field: Field::FreqSepSmooth,
        curve: CurveKind::SoftKnee,
        bipolar: false,
        center: 0.0,
        half: 1.0,
        unit: "",
        dec: 2,
    });
    v.push(ParamSpec {
        label: "磨皮 · 蒙版羽化".into(),
        tooltip: "皮肤与非皮肤过渡的柔和度",
        field: Field::FreqSepFeather,
        curve: CurveKind::SoftKnee,
        bipolar: false,
        center: 0.0,
        half: 1.0,
        unit: "",
        dec: 2,
    });
    v.push(ParamSpec {
        label: "金字塔融合".into(),
        tooltip: "多层细节自然融合，增加立体感而不生硬",
        field: Field::PyramidStrength,
        curve: CurveKind::SoftKnee,
        bipolar: false,
        center: 0.0,
        half: 1.0,
        unit: "",
        dec: 2,
    });
    v.push(ParamSpec {
        label: "金字塔 · 细节倍率".into(),
        tooltip: "细节增强强度，1.0 接近自然，适度更立体",
        field: Field::PyramidScale,
        curve: CurveKind::SoftKnee,
        bipolar: false,
        center: 1.0,
        half: 2.0,
        unit: "×",
        dec: 2,
    });

    v
}
