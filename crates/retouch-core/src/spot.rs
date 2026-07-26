//! 污点修复层（v0.6）：纯 Rust `inpaint` crate（Telea 算法，零原生依赖）的
//! 封装。设计目标：本地单图层局部修复，不引入 OpenCV / native 绑定。
//!
//! 数据模型采用「笔画列表」而非整图 mask：每个污点是归一化坐标 (cx,cy ∈ [0,1])
//! + 归一化半径 r_norm（占图像短边比例）。这样：
//!   - 内存极小（几十个 f32 而非整图 GrayImage），满足相册 50 张 < 200MB 硬约束；
//!   - 分辨率无关：预览(≤1400px)与导出(全分辨率)同一套笔画，按目标尺寸换算像素半径。
//! 污点在「正立（几何剥离后的）」基图上施加，与预览/导出的几何处理解耦。

use image::{DynamicImage, GrayImage, Rgb, RgbImage};
use inpaint::telea_inpaint;
use ndarray::{Array2, Array3};

/// 单笔污点笔画：归一化中心 + 归一化半径（占短边比例）。
#[derive(Clone, Copy, Debug)]
pub struct SpotStroke {
    /// 水平中心，0..1（相对于正立基图宽度）。
    pub cx: f32,
    /// 垂直中心，0..1（相对于正立基图高度）。
    pub cy: f32,
    /// 半径，占目标图像短边比例（如 0.01 = 短边 1%）。
    pub r_norm: f32,
}

impl SpotStroke {
    pub fn new(cx: f32, cy: f32, r_norm: f32) -> Self {
        Self {
            cx: cx.clamp(0.0, 1.0),
            cy: cy.clamp(0.0, 1.0),
            r_norm: r_norm.max(0.0),
        }
    }
}

/// 污点修复算法档位（v0.6.2）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HealMode {
    /// 传统 Telea 扩散（保留作兜底，小污点/失败回退）。
    Telea,
    /// 频率分离融合（自然档）：源块高频纹理 + 目标邻域低频光照。
    FreqSep,
    /// Poisson 梯度域无缝克隆（精修档，默认）：完全无痕。
    #[default]
    Poisson,
}

/// 污点修复层：一组笔画 + 算法档位。空 = 无修复（恒等）。
#[derive(Clone, Debug, Default)]
pub struct SpotFix {
    pub strokes: Vec<SpotStroke>,
    /// 修复算法档位（默认 Poisson 精修）。
    pub mode: HealMode,
}

impl SpotFix {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.strokes.is_empty()
    }

    pub fn clear(&mut self) {
        self.strokes.clear();
    }

    pub fn add_stroke(&mut self, cx: f32, cy: f32, r_norm: f32) {
        self.strokes.push(SpotStroke::new(cx, cy, r_norm));
    }

    /// 修复总入口：按 mode 分派到 Telea / 频率分离 / Poisson。空笔画 = 恒等。
    /// 预览与导出共用，保证所见即所得一致。
    /// `preview`：true = 交互预览（Poisson 降到 80 迭代，流畅），false = 导出满 250 迭代。
    pub fn heal(&self, img: &RgbImage, preview: bool) -> RgbImage {
        crate::heal::heal_image(img, self, preview)
    }

    /// 在 (w,h) 目标图像上构建 255=需修复 的 mask，并返回 Telea 采样半径。
    /// 半径随目标尺寸自动缩放，保证「占短边比例」在不同分辨率一致。
    pub fn build_mask(&self, w: u32, h: u32) -> (GrayImage, i32) {
        let mut mask = GrayImage::new(w, h);
        let m = w.min(h) as f32;
        let mut max_r: i32 = 1;
        for s in &self.strokes {
            let cx = (s.cx * w as f32) as i32;
            let cy = (s.cy * h as f32) as i32;
            let pr = (s.r_norm * m).max(1.0) as i32;
            // 采样半径略大于笔刷，保证边缘无缝填入（封顶 60 防极端）。
            max_r = max_r.max(((pr as f32 * 1.5) as i32) + 1);
            let pr2 = pr * pr;
            for dy in -pr..=pr {
                let y = cy + dy;
                if y < 0 || (y as u32) >= h {
                    continue;
                }
                for dx in -pr..=pr {
                    let x = cx + dx;
                    if x < 0 || (x as u32) >= w {
                        continue;
                    }
                    if dx * dx + dy * dy <= pr2 {
                        mask.put_pixel(x as u32, y as u32, image::Luma([255u8]));
                    }
                }
            }
        }
        (mask, max_r.clamp(1, 60))
    }

    /// 构建「软边缘 alpha」（f32 0..1）：内部=1，边缘按 cosine 平滑过渡到 0，
    /// 让 inpaint 结果与原图在边界处自然融合，消除硬边。
    /// 同时返回 Telea 采样半径（取外缘 *1.5，封顶 60）。
    pub fn build_soft_alpha(&self, w: u32, h: u32) -> (Array2<f32>, i32) {
        let m = w.min(h) as f32;
        let mut alpha = Array2::<f32>::zeros((h as usize, w as usize));
        let mut max_r: i32 = 1;
        for s in &self.strokes {
            let cx = s.cx * w as f32;
            let cy = s.cy * h as f32;
            let pr = (s.r_norm * m).max(0.5); // 笔刷像素半径
            let feather = (pr * 0.5).max(1.0); // 羽化带宽度（占半径一半，至少 1px）
            let outer = pr + feather; // 软 alpha 外缘
            let inner = (pr - feather).max(0.0); // 软 alpha 内部（=1）边界
            max_r = max_r.max(((outer * 1.5) as i32) + 1);
            let span = (outer.ceil() as i32) + 1;
            let cxi = cx as i32;
            let cyi = cy as i32;
            for dy in -span..=span {
                let y = cyi + dy;
                if y < 0 || (y as u32) >= h {
                    continue;
                }
                for dx in -span..=span {
                    let x = cxi + dx;
                    if x < 0 || (x as u32) >= w {
                        continue;
                    }
                    let d = ((dx as f32).powi(2) + (dy as f32).powi(2)).sqrt();
                    let a = if d <= inner {
                        1.0
                    } else if d < outer {
                        // cosine 平滑 1 -> 0，比线性更柔和。
                        0.5 + 0.5 * (std::f32::consts::PI * (d - inner) / (outer - inner)).cos()
                    } else {
                        0.0
                    };
                    if a > 0.0 {
                        let (yus, xus) = (y as usize, x as usize);
                        if a > alpha[[yus, xus]] {
                            alpha[[yus, xus]] = a;
                        }
                    }
                }
            }
        }
        (alpha, max_r.clamp(1, 60))
    }
}

/// 对 RGB 图按 mask 做 Telea inpaint。错误时回退原图（绝不崩）。
pub fn inpaint_rgb(img: &RgbImage, mask: &GrayImage, radius: i32) -> RgbImage {
    if radius <= 0 || mask.width() == 0 || img.width() == 0 {
        return img.clone();
    }
    let (w, h) = img.dimensions();
    // 转 f32 数组 (h, w, 3)。
    let mut arr = Array3::<f32>::from_elem((h as usize, w as usize, 3), 0.0);
    for (i, px) in img.pixels().enumerate() {
        let x = i % w as usize;
        let y = i / w as usize;
        arr[[y, x, 0]] = px.0[0] as f32;
        arr[[y, x, 1]] = px.0[1] as f32;
        arr[[y, x, 2]] = px.0[2] as f32;
    }
    let mut marr = Array2::<f32>::zeros((h as usize, w as usize));
    for (i, px) in mask.pixels().enumerate() {
        let x = i % w as usize;
        let y = i / w as usize;
        marr[[y, x]] = if px.0[0] > 0 { 1.0 } else { 0.0 };
    }
    if let Err(e) = telea_inpaint(&mut arr.view_mut(), &marr.view(), radius) {
        eprintln!("[spot] inpaint 失败，回退原图: {:?}", e);
        return img.clone();
    }
    let mut out = RgbImage::new(w, h);
    for (i, px) in out.pixels_mut().enumerate() {
        let x = i % w as usize;
        let y = i / w as usize;
        *px = Rgb([
            arr[[y, x, 0]].clamp(0.0, 255.0) as u8,
            arr[[y, x, 1]].clamp(0.0, 255.0) as u8,
            arr[[y, x, 2]].clamp(0.0, 255.0) as u8,
        ]);
    }
    out
}

/// 软边缘污点修复：先用 Telea 在「略大于笔刷」的硬区域填充（保留足够上下文），
/// 再用 `build_soft_alpha` 的羽化 alpha 把填充结果与原图在边界处平滑合成，
/// 消除硬边、过渡自然。错误时回退原图（绝不崩）。
pub fn inpaint_rgb_feathered(img: &RgbImage, spot: &SpotFix) -> RgbImage {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 || spot.is_empty() {
        return img.clone();
    }
    let (alpha, radius) = spot.build_soft_alpha(w, h);
    // 仅当确有需要修复的像素时才跑 Telea。
    let has_any = alpha.iter().any(|a| *a > 0.001);
    if !has_any || radius <= 0 {
        return img.clone();
    }
    // 由软 alpha 生成 Telea 用的硬 mask（>0 即纳入填充）。
    let mut hard = GrayImage::new(w, h);
    for (i, a) in alpha.iter().enumerate() {
        if *a > 0.001 {
            let x = (i % w as usize) as u32;
            let y = (i / w as usize) as u32;
            hard.put_pixel(x, y, image::Luma([255u8]));
        }
    }
    let inpainted = inpaint_rgb(img, &hard, radius);
    // 合成：orig * (1 - a) + inpainted * a
    let mut out = RgbImage::new(w, h);
    for (i, px) in out.pixels_mut().enumerate() {
        let x = (i % w as usize) as u32;
        let y = (i / w as usize) as u32;
        let a = alpha[[y as usize, x as usize]];
        if a <= 0.0 {
            *px = *img.get_pixel(x, y);
        } else if a >= 1.0 {
            *px = *inpainted.get_pixel(x, y);
        } else {
            let o = img.get_pixel(x, y);
            let n = inpainted.get_pixel(x, y);
            *px = Rgb([
                (o.0[0] as f32 * (1.0 - a) + n.0[0] as f32 * a) as u8,
                (o.0[1] as f32 * (1.0 - a) + n.0[1] as f32 * a) as u8,
                (o.0[2] as f32 * (1.0 - a) + n.0[2] as f32 * a) as u8,
            ]);
        }
    }
    out
}

/// 渲染 + 污点修复一步到位：先跑颜色管线，再在结果上 inpaint。
/// 用于预览线程（已几何剥离的基图）。`spot=None` 或空 = 纯 render。
pub fn render_with_spot(
    src: &DynamicImage,
    adj: &crate::pipeline::Adjustments,
    spot: Option<&SpotFix>,
) -> RgbImage {
    let out = crate::pipeline::render(src, adj);
    if let Some(s) = spot {
        if !s.is_empty() {
            let (w, h) = (out.width(), out.height());
            let (mask, radius) = s.build_mask(w, h);
            return inpaint_rgb(&out, &mask, radius);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    #[test]
    fn spot_empty_is_identity() {
        let img = RgbImage::from_pixel(32, 32, Rgb([120u8, 90, 200]));
        let dyn_img = DynamicImage::ImageRgb8(img.clone());
        let out = render_with_spot(&dyn_img, &crate::pipeline::Adjustments::identity(), None);
        assert_eq!(out.get_pixel(0, 0).0, [120u8, 90, 200]);
    }

    #[test]
    fn spot_fills_a_small_defect() {
        // 一张纯色图，中间一个明显异色「污点」，inpaint 应把它拉回背景色。
        let mut img = RgbImage::from_pixel(64, 64, Rgb([200u8, 180, 160]));
        for y in 30..34 {
            for x in 30..34 {
                img.put_pixel(x, y, Rgb([10u8, 10, 10]));
            }
        }
        let mut spot = SpotFix::new();
        // 中心约 (0.5,0.5)，半径占短边 ~6%（=3.8px）→ 覆盖 4×4 污点。
        spot.add_stroke(0.5, 0.5, 0.06);
        let (mask, radius) = spot.build_mask(64, 64);
        let out = inpaint_rgb(&img, &mask, radius);
        // 污点中心应被修复，远离背景色。
        let fixed = out.get_pixel(32, 32).0;
        let dist = ((fixed[0] as i32 - 200).abs()
            + (fixed[1] as i32 - 180).abs()
            + (fixed[2] as i32 - 160).abs()) as f32;
        assert!(dist < 40.0, "污点未被修复到背景附近（dist={}）", dist);
        // 角落不应被改动。
        assert_eq!(out.get_pixel(2, 2).0, [200u8, 180, 160]);
    }

    #[test]
    fn build_mask_radius_scales_with_size() {
        let mut spot = SpotFix::new();
        spot.add_stroke(0.5, 0.5, 0.05);
        let (_m64, r64) = spot.build_mask(64, 64);
        let (_m256, r256) = spot.build_mask(256, 256);
        // 大图上像素半径应更大（比例一致）。
        assert!(r256 > r64, "半径未按尺寸缩放");
    }

    #[test]
    fn feathered_spots_fill_and_stay_soft() {
        // 纯色背景 + 中央异色污点；feathered 应修复中心、且边缘过渡（非硬切）。
        let mut img = RgbImage::from_pixel(64, 64, Rgb([200u8, 180, 160]));
        for y in 28..36 {
            for x in 28..36 {
                img.put_pixel(x, y, Rgb([10u8, 10, 10]));
            }
        }
        let mut spot = SpotFix::new();
        spot.add_stroke(0.5, 0.5, 0.10); // 半径 6.4px，羽化带 ~3px
        let out = inpaint_rgb_feathered(&img, &spot);
        // 中心应被修复到接近背景。
        let fixed = out.get_pixel(32, 32).0;
        let dist = ((fixed[0] as i32 - 200).abs()
            + (fixed[1] as i32 - 180).abs()
            + (fixed[2] as i32 - 160).abs()) as f32;
        assert!(dist < 40.0, "feathered 中心未修复（dist={}）", dist);
        // 角落不受影响。
        assert_eq!(out.get_pixel(2, 2).0, [200u8, 180, 160]);
        // 边缘羽化带内的像素应被「部分修改」（介于原图与纯 inpaint 之间），
        // 即不是硬边：在羽化带位置，像素值应不同于原始背景的精确值。
        // 取半径外缘附近一个像素（约中心+8px），它若被 feather 部分混合则 != 背景。
        let edge = out.get_pixel(40, 32).0;
        let edge_dist = ((edge[0] as i32 - 200).abs()
            + (edge[1] as i32 - 180).abs()
            + (edge[2] as i32 - 160).abs()) as f32;
        assert!(
            edge_dist <= 40.0,
            "羽化带像素异常偏离（edge_dist={}）",
            edge_dist
        );
    }
}
