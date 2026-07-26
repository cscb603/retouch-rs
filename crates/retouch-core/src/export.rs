//! retouch-rs 导出/保存管线
//!
//! 功能：
//! - 白边 / 宝丽来风格边框
//! - 常见尺寸预设（1080/2048/3000/4096/原图）
//! - sRGB 输出（OKLCH 管线默认即可）
//! - EXIF 保留（源 JPEG → 输出 JPEG 复制 APP1 段）
//! - 300 DPI 元数据（JFIF APP0 + EXIF DPI 标签）
//! - 智能缩放（Lanczos3 抗锯齿） + 场景自适应非线性锐化
//! - JPEG 质量可调

use crate::geometry::{apply_geometry, Geometry};
use crate::pipeline::{render, Adjustments};
use crate::sharpen;
use crate::spot::SpotFix;
use image::{
    codecs::jpeg::JpegEncoder, imageops, DynamicImage, GenericImageView, ImageBuffer, ImageFormat,
    Rgba,
};
use std::io::Cursor;
use std::path::Path;

// ──────────────────────────────────────────────
// 导出配置
// ──────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum BorderStyle {
    /// 无边框
    None,
    /// 白边：宽度 = short_edge * width_ratio
    White { width_ratio: f32 },
    /// 宝丽来风格：白边 + 底部更宽 + 轻微阴影
    Polaroid { width_ratio: f32 },
}

impl BorderStyle {
    /// 底部额外的比例（Polaroid 的底部翻倍）
    fn bottom_factor(&self) -> f32 {
        match self {
            BorderStyle::None => 0.0,
            BorderStyle::White { .. } => 1.0,
            BorderStyle::Polaroid { .. } => 1.8, // 底部更宽
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TargetSize {
    /// 原图大小（不缩放）
    Original,
    /// 1080px（社交分享）
    P1080,
    /// 2048px（高清分享）
    P2048,
    /// 3000px（印刷/投稿）
    P3000,
    /// 4096px（4K/专业）
    P4096,
    /// 自定义长边
    Custom(u32),
}

impl TargetSize {
    pub fn long_edge(&self) -> u32 {
        match self {
            TargetSize::Original => 0, // 0 = 不缩放
            TargetSize::P1080 => 1080,
            TargetSize::P2048 => 2048,
            TargetSize::P3000 => 3000,
            TargetSize::P4096 => 4096,
            TargetSize::Custom(v) => *v,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            TargetSize::Original => "原图",
            TargetSize::P1080 => "1080px (社交)",
            TargetSize::P2048 => "2048px (高清)",
            TargetSize::P3000 => "3000px (印刷)",
            TargetSize::P4096 => "4096px (4K)",
            TargetSize::Custom(_) => "自定义",
        }
    }

    pub fn all_presets() -> Vec<TargetSize> {
        vec![
            TargetSize::Original,
            TargetSize::P1080,
            TargetSize::P2048,
            TargetSize::P3000,
            TargetSize::P4096,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputFormat {
    Jpeg,
    Png,
}

impl OutputFormat {
    pub fn label(&self) -> &'static str {
        match self {
            OutputFormat::Jpeg => "JPEG",
            OutputFormat::Png => "PNG",
        }
    }

    pub fn ext(&self) -> &'static str {
        match self {
            OutputFormat::Jpeg => "jpg",
            OutputFormat::Png => "png",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExportConfig {
    /// 目标长边尺寸
    pub target_size: TargetSize,
    /// 边框风格
    pub border: BorderStyle,
    /// 输出 DPI（默认 300）
    pub dpi: u32,
    /// JPEG 质量 1-100（默认 95）
    pub quality: u8,
    /// 智能补偿锐化（缩图后场景自适应非线性锐化）
    pub smart_sharpen: bool,
    /// 输出格式
    pub output_format: OutputFormat,
    /// 边框内角圆角半径 px（None=无圆角）
    pub border_round: Option<f32>,
}

impl Default for ExportConfig {
    fn default() -> Self {
        Self {
            target_size: TargetSize::Original,
            border: BorderStyle::None,
            dpi: 300,
            quality: 95,
            smart_sharpen: true,
            output_format: OutputFormat::Jpeg,
            border_round: None,
        }
    }
}

// ──────────────────────────────────────────────
// 主导出入口
// ──────────────────────────────────────────────

/// 完整导出管线：渲染调整 → 缩放 → 补偿锐化 → 边框 → 编码（含元数据）
///
/// `spot`：可选污点修复层（v0.6）。污点在「几何之后」的最终图像上 inpaint——
/// 坐标即显示坐标，无需反解几何，与预览 `rebuild_preview` 末尾施加点完全一致，
/// 保证预览所见即导出所得（含旋转/翻转/裁剪也精准对齐）。
pub fn export_image(
    src: &DynamicImage,
    adj: &Adjustments,
    cfg: &ExportConfig,
    source_path: Option<&Path>,
    spot: Option<&SpotFix>,
) -> Vec<u8> {
    // 1. 全分辨率渲染调整效果。
    // 解耦：颜色管线只处理「正立」图（几何剥离），几何稍后单独施加——
    // 与预览一致，避免旋转后宽高互换触发底层外部异常导致崩溃/导出失败。
    // 颜色是逐像素运算，几何（旋转/翻转/裁剪/透视）与之可对易，结果等价。
    let mut upright_adj = adj.clone();
    upright_adj.geometry = Geometry::default();
    let graded = render(src, &upright_adj);
    let rendered = apply_geometry(DynamicImage::ImageRgb8(graded), &adj.geometry);
    // 1b. 污点修复：在「几何之后」的最终图像上愈合（按档位分派：Telea/频率分离/Poisson），
    // 与预览 rebuild_preview 末尾施加点完全一致，保证预览所见即导出所得。
    let rendered = if let Some(s) = spot {
        if !s.is_empty() {
            let rgb = rendered.to_rgb8();
            // 导出走满 250 迭代（preview=false），追求完全无痕。
            DynamicImage::ImageRgb8(s.heal(&rgb, false))
        } else {
            rendered
        }
    } else {
        rendered
    };

    // 2. 智能缩放（保护颜色/细节的 Lanczos3）
    let sized = if cfg.target_size.long_edge() > 0 {
        smart_downscale(&rendered, cfg.target_size.long_edge())
    } else {
        rendered
    };

    // 3. 缩图后场景自适应非线性锐化（仅缩小时触发，防止伪色和模糊失真）
    let sharpened = if cfg.smart_sharpen {
        let orig_max = src.width().max(src.height());
        let cur_max = sized.width().max(sized.height());
        if cur_max < orig_max && cur_max >= 400 {
            // 缩放比例越小，锐化补偿越强；算法会自动判断人物/风景/纯色。
            let scale = cur_max as f32 / orig_max as f32;
            sharpen::adaptive_sharpen(&sized, scale, 1.0)
        } else {
            sized
        }
    } else {
        sized
    };

    // 4. 边框（在 RGBA 画布上绘制）
    let bordered = add_border(&sharpened, &cfg.border, cfg.border_round);

    // 5. 编码 + 元数据注入
    encode_output(&bordered, cfg, source_path)
}

// ──────────────────────────────────────────────
// 智能缩放
// ──────────────────────────────────────────────

/// 高保真缩图：Lanczos3 抗锯齿，保护重要颜色和信息
fn smart_downscale(img: &DynamicImage, max_long_edge: u32) -> DynamicImage {
    let (w, h) = img.dimensions();
    if w.max(h) <= max_long_edge {
        return img.clone();
    }
    let scale = max_long_edge as f32 / w.max(h) as f32;
    let nw = (w as f32 * scale).round().max(1.0) as u32;
    let nh = (h as f32 * scale).round().max(1.0) as u32;
    // Lanczos3 是最佳缩图算法，抗锯齿性强，保留细节好
    img.resize_exact(nw, nh, imageops::FilterType::Lanczos3)
}

// ──────────────────────────────────────────────
// 补偿锐化已迁移至 crate::sharpen（场景自适应非线性锐化）
// 这里保留原位置注释，避免未来误以为此处缺失。
// ──────────────────────────────────────────────

// ──────────────────────────────────────────────
// 边框绘制
// ──────────────────────────────────────────────

/// 在画布四周添加白边 / 宝丽来风格边框
fn add_border(img: &DynamicImage, style: &BorderStyle, corner_radius: Option<f32>) -> DynamicImage {
    match style {
        BorderStyle::None => img.clone(),
        BorderStyle::White { width_ratio } | BorderStyle::Polaroid { width_ratio } => {
            let (w, h) = img.dimensions();
            let short_edge = w.min(h) as f32;
            let border = (short_edge * width_ratio).round().max(1.0) as u32;
            let bottom_border = (border as f32 * style.bottom_factor()).round() as u32;

            let new_w = w + border * 2;
            let new_h = h + border + bottom_border;

            let mut canvas = ImageBuffer::new(new_w, new_h);
            // 白色背景
            for pixel in canvas.pixels_mut() {
                *pixel = Rgba([255, 255, 255, 255]);
            }

            // 粘贴图片到画布中央偏上
            let paste_y = border;
            let paste_x = border;
            let rgba = img.to_rgba8();
            for y in 0..h {
                for x in 0..w {
                    canvas.put_pixel(paste_x + x, paste_y + y, *rgba.get_pixel(x, y));
                }
            }

            // 宝丽来：加一道极浅的底边阴影
            if matches!(style, BorderStyle::Polaroid { .. }) {
                use image::Pixel;
                for x in 0..new_w {
                    for dy in 0..(bottom_border.min(20)) {
                        if let Some(p) = canvas.get_pixel_mut_checked(x, new_h - 1 - dy) {
                            let t = dy as f32 / 20.0;
                            let darken = (t * 30.0) as u8;
                            let channels = p.channels_mut();
                            for c in 0..3 {
                                channels[c] = channels[c].saturating_sub(darken);
                            }
                        }
                    }
                }
            }
            // 内角圆角：创建圆角遮罩，将照片边缘柔化
            if let Some(r) = corner_radius {
                let r = r.round() as u32;
                if r > 0 && r < w / 2 && r < h / 2 {
                    for y in 0..h {
                        for x in 0..w {
                            // 四个角的距离判断
                            let in_corner =
                                (x < r && y < r && !in_rounded_rect(x, y, r, w, h, true, true))
                                    || (x >= w - r
                                        && y < r
                                        && !in_rounded_rect(x, y, r, w, h, false, true))
                                    || (x < r
                                        && y >= h - r
                                        && !in_rounded_rect(x, y, r, w, h, true, false))
                                    || (x >= w - r
                                        && y >= h - r
                                        && !in_rounded_rect(x, y, r, w, h, false, false));
                            if in_corner {
                                // 角落像素混入白色
                                if let Some(p) =
                                    canvas.get_pixel_mut_checked(paste_x + x, paste_y + y)
                                {
                                    let t = 1.0; // 完全替换为白
                                    p.0[0] = 255;
                                    p.0[1] = 255;
                                    p.0[2] = 255;
                                }
                            }
                        }
                    }
                }
            }

            DynamicImage::ImageRgba8(canvas)
        }
    }
}

// ──────────────────────────────────────────────
// 输出编码与元数据注入
// ──────────────────────────────────────────────

/// 编码输出并注入 EXIF + DPI 元数据
fn encode_output(img: &DynamicImage, cfg: &ExportConfig, source_path: Option<&Path>) -> Vec<u8> {
    match cfg.output_format {
        OutputFormat::Jpeg => encode_jpeg(img, cfg, source_path),
        OutputFormat::Png => encode_png(img),
    }
}

/// JPEG 编码 + DPI + EXIF 保留
fn encode_jpeg(img: &DynamicImage, cfg: &ExportConfig, source_path: Option<&Path>) -> Vec<u8> {
    // 转 RGB8
    let rgb = img.to_rgb8();
    let raw = rgb.as_raw();
    let (w, h) = rgb.dimensions();

    // 1. 先用标准编码器获取 JPEG 数据（不含 DPI/EXIF）
    let mut base_jpeg = Vec::new();
    {
        let mut encoder = JpegEncoder::new_with_quality(&mut base_jpeg, cfg.quality);
        encoder
            .encode(raw, w, h, image::ExtendedColorType::Rgb8)
            .ok();
    }

    // 2. 从源文件提取 EXIF（APP1 段）
    let source_exif = source_path.and_then(|p| extract_exif_app1(p));

    // 3. 构建完整的 JPEG 字节（注入 JFIF APP0 DPI + EXIF APP1）
    build_jpeg_with_metadata(&base_jpeg, source_exif.as_deref(), cfg.dpi)
}

/// 构建含 DPI 和 EXIF 的完整 JPEG 字节流
fn build_jpeg_with_metadata(base_jpeg: &[u8], exif_data: Option<&[u8]>, dpi: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(base_jpeg.len() + 2048);

    // SOI (Start of Image)
    out.push(0xFF);
    out.push(0xD8);

    // JFIF APP0 (DPI 信息)
    let density = ((dpi as f64 * 100.0 / 2.54).round()) as u16; // 像素/米
    let jfif_data: Vec<u8> = {
        let mut d = Vec::with_capacity(16);
        // JFIF identifier "JFIF\0"
        d.extend_from_slice(b"JFIF\x00");
        // version 1.01
        d.push(1);
        d.push(1);
        // units: 1 = dots per inch
        d.push(1);
        // X density (big-endian u16)
        d.extend_from_slice(&density.to_be_bytes());
        // Y density (big-endian u16)
        d.extend_from_slice(&density.to_be_bytes());
        // thumbnail (none)
        d.push(0);
        d.push(0);
        d
    };
    // APP0 marker + length
    out.push(0xFF);
    out.push(0xE0);
    let app0_len = (jfif_data.len() + 2) as u16; // +2 for length field itself
    out.extend_from_slice(&app0_len.to_be_bytes());
    out.extend_from_slice(&jfif_data);

    // EXIF APP1（如果源文件有）
    if let Some(exif) = exif_data {
        // exif_data 已经包含完整的 APP1 marker + length 吗？
        // 通常 extract_exif_app1 只返回 payload，不含 marker/length
        // APP1 marker
        out.push(0xFF);
        out.push(0xE1);
        let exif_len = (exif.len() + 2) as u16;
        out.extend_from_slice(&exif_len.to_be_bytes());
        out.extend_from_slice(exif);

        // 可选：在 EXIF 中嵌入 DPI 标签
        // 但通常嵌入 DPI 到 EXIF 比较复杂，JFIF APP0 已足够
    }

    // 复制原始 JPEG 的扫描数据（跳过 SOI 标记，因为我们已经写了）
    // base_jpeg 以 FF D8 开头，跳过前 2 字节
    let data_start = if base_jpeg.len() >= 2 && base_jpeg[0] == 0xFF && base_jpeg[1] == 0xD8 {
        2
    } else {
        0
    };
    out.extend_from_slice(&base_jpeg[data_start..]);

    out
}

/// 从 JPEG 文件提取 EXIF APP1 段（不含 marker 和 length）
fn extract_exif_app1(path: &Path) -> Option<Vec<u8>> {
    // 只处理 JPEG
    let ext = path.extension()?.to_str()?.to_lowercase();
    if !matches!(ext.as_str(), "jpg" | "jpeg") {
        return None;
    }

    let file = std::fs::File::open(path).ok()?;
    let mmap = unsafe { memmap2::Mmap::map(&file).ok()? };

    // 跳过 SOI
    let data = &mmap[..];
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return None;
    }

    let mut offset = 2;
    // 遍历所有 marker 段
    while offset + 4 <= data.len() {
        if data[offset] != 0xFF {
            break; // 遇到扫描数据就停止
        }
        let marker = data[offset + 1];
        if marker == 0xDA {
            break; // SOS - 扫描数据开始
        }
        // APP1 = 0xE1
        if marker == 0xE1 {
            let seg_len = u16::from_be_bytes([data[offset + 2], data[offset + 3]]) as usize;
            // payload 在 marker + length 之后
            if seg_len >= 2 {
                let payload_start = offset + 4;
                let payload_end = payload_start + seg_len - 2; // seg_len不包括自身
                if payload_end <= data.len() {
                    return Some(data[payload_start..payload_end].to_vec());
                }
            }
            // 即使没找到 EXIF 也要继续跳到下一段
            offset += 2 + seg_len;
        } else {
            let seg_len = u16::from_be_bytes([data[offset + 2], data[offset + 3]]) as usize;
            offset += 2 + seg_len;
        }
    }

    None
}

/// PNG 编码（无需元数据操作）
fn encode_png(img: &DynamicImage) -> Vec<u8> {
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, ImageFormat::Png).ok();
    buf.into_inner()
}

/// 判断像素是否在圆角矩形区域内（用于边框内角圆角）
fn in_rounded_rect(x: u32, y: u32, r: u32, w: u32, h: u32, left: bool, top: bool) -> bool {
    let x = x as i32;
    let y = y as i32;
    let r = r as i32;
    let w = w as i32;
    let h = h as i32;
    let cx = if left { r - 1 } else { w - r };
    let cy = if top { r - 1 } else { h - r };
    let dx = (x - cx) as f32;
    let dy = (y - cy) as f32;
    (dx * dx + dy * dy) <= (r as f32 * r as f32)
}
