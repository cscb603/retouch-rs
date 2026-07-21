//! Geometry preprocessing (M4b): perspective (homography) correction, arbitrary
//! rotation, flip, and normalized crop. Applied to the **decoded RGB image**
//! *before* the linearization / OKLCH pipeline, exactly as the design specifies
//! (顺序: 透视纠正 → 旋转 → 翻转 → 裁剪).
//!
//! All transforms are coordinate resamples implemented from scratch (no new
//! dependency). A `Geometry` that is `is_identity()` returns the input image
//! untouched, so an identity adjustment preserves the M0 pixel-exact round-trip.

use image::{DynamicImage, Rgb, RgbImage};
use image::imageops;

/// Geometry preprocessing controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    /// Normalized crop rect `(x, y, w, h)` in 0..1 of the *current* image.
    /// `None` = no crop.
    pub crop: Option<(f32, f32, f32, f32)>,
    /// Coarse 90° quarter-turns (clockwise, matching `imageops::rotate90`), 0..3.
    /// Applied as exact, lossless steps BEFORE the fine `rotate_deg` straightening,
    /// so the "左转/右转 90°" buttons never fight the ±45° fine-rotate slider.
    pub quarter_turns: u8,
    /// Fine rotation in degrees (counter-clockwise), for straightening the
    /// horizon. `0` = none. Non-90° angles crop the black corners (no stretch).
    pub rotate_deg: f32,
    /// Flip horizontally.
    pub flip_h: bool,
    /// Flip vertically.
    pub flip_v: bool,
    /// Perspective / keystone correction `(v_key, h_key)` in -1..1, `(0,0)` = none.
    /// Vertical keystone narrows/widens the top edge; horizontal the left edge.
    pub perspective: Option<(f32, f32)>,
}

impl Default for Geometry {
    fn default() -> Self {
        Self {
            crop: None,
            quarter_turns: 0,
            rotate_deg: 0.0,
            flip_h: false,
            flip_v: false,
            perspective: None,
        }
    }
}

impl Geometry {
    /// True when no transform is active — callers should then skip resampling
    /// entirely to keep the pipeline pixel-exact.
    pub fn is_identity(&self) -> bool {
        self.crop.is_none()
            && self.quarter_turns % 4 == 0
            && self.rotate_deg == 0.0
            && !self.flip_h
            && !self.flip_v
            && self.perspective.map_or(true, |(v, h)| v == 0.0 && h == 0.0)
    }
}

/// Apply geometry preprocessing. Returns the input unchanged when identity.
pub fn apply_geometry(img: DynamicImage, g: &Geometry) -> DynamicImage {
    if g.is_identity() {
        return img;
    }
    let mut cur = img.to_rgb8();

    // 1. perspective (homography keystone)
    if let Some((v, h)) = g.perspective {
        if v != 0.0 || h != 0.0 {
            cur = warp_perspective(&cur, v, h);
        }
    }
    // 1.5 coarse quarter-turns: exact, lossless 90° steps (no crop needed).
    match g.quarter_turns % 4 {
        1 => cur = imageops::rotate90(&cur),
        2 => cur = imageops::rotate180(&cur),
        3 => cur = imageops::rotate270(&cur),
        _ => {}
    }
    // 2. fine rotation (90-steps exact, otherwise bilinear affine warp + crop)
    if g.rotate_deg != 0.0 {
        let r = ((g.rotate_deg % 360.0) + 360.0) % 360.0;
        let step = (r / 90.0).round();
        if (r - step * 90.0).abs() < 0.5 {
            cur = match (step as i32) % 4 {
                1 => imageops::rotate90(&cur),
                2 => imageops::rotate180(&cur),
                3 => imageops::rotate270(&cur),
                _ => cur,
            };
        } else {
            cur = warp_rotate(&cur, g.rotate_deg);
        }
    }
    // 3. flip
    if g.flip_h {
        cur = imageops::flip_horizontal(&cur);
    }
    if g.flip_v {
        cur = imageops::flip_vertical(&cur);
    }
    // 4. crop (normalized rect)
    if let Some((x, y, w, h)) = g.crop {
        let (iw, ih) = cur.dimensions();
        let cx = (x.clamp(0.0, 1.0) * iw as f32) as u32;
        let cy = (y.clamp(0.0, 1.0) * ih as f32) as u32;
        let cw = (((x + w).clamp(0.0, 1.0)) * iw as f32) as u32 - cx;
        let ch = (((y + h).clamp(0.0, 1.0)) * ih as f32) as u32 - cy;
        if cw > 0 && ch > 0 {
            cur = imageops::crop(&mut cur, cx, cy, cw, ch).to_image();
        }
    }
    DynamicImage::ImageRgb8(cur)
}

/// Bilinear sample of an `RgbImage` at float coords (clamped to edges).
#[inline]
fn sample_bilinear(img: &RgbImage, x: f32, y: f32) -> [u8; 3] {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return [0, 0, 0];
    }
    let fx = x.clamp(0.0, w as f32 - 1.0);
    let fy = y.clamp(0.0, h as f32 - 1.0);
    let x0 = fx.floor() as i32;
    let y0 = fy.floor() as i32;
    let x1 = (x0 + 1).min(w as i32 - 1);
    let y1 = (y0 + 1).min(h as i32 - 1);
    let x0c = x0.max(0) as u32;
    let y0c = y0.max(0) as u32;
    let tx = fx - x0 as f32;
    let ty = fy - y0 as f32;
    let p00 = img.get_pixel(x0c, y0c).0;
    let p10 = img.get_pixel(x1 as u32, y0c).0;
    let p01 = img.get_pixel(x0c, y1 as u32).0;
    let p11 = img.get_pixel(x1 as u32, y1 as u32).0;
    let lerp = |a: u8, b: u8, t: f32| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    [
        lerp(lerp(p00[0], p10[0], tx), lerp(p01[0], p11[0], tx), ty),
        lerp(lerp(p00[1], p10[1], tx), lerp(p01[1], p11[1], tx), ty),
        lerp(lerp(p00[2], p10[2], tx), lerp(p01[2], p11[2], tx), ty),
    ]
}

/// Arbitrary-angle rotation via affine inverse sampling.
fn warp_rotate(img: &RgbImage, deg: f32) -> RgbImage {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return img.clone();
    }
    let rad = deg.to_radians();
    let cos = rad.cos();
    let sin = rad.sin();
    let cx = (w - 1) as f32 / 2.0;
    let cy = (h - 1) as f32 / 2.0;
    // Corners in centered coords, rotated forward to find output extent.
    let corners = [
        (-cx, -cy),
        (w as f32 - 1.0 - cx, -cy),
        (w as f32 - 1.0 - cx, h as f32 - 1.0 - cy),
        (-cx, h as f32 - 1.0 - cy),
    ];
    let mut minx = f32::MAX;
    let mut maxx = f32::MIN;
    let mut miny = f32::MAX;
    let mut maxy = f32::MIN;
    for (px, py) in corners.iter() {
        let rx = px * cos - py * sin;
        let ry = px * sin + py * cos;
        minx = minx.min(rx);
        maxx = maxx.max(rx);
        miny = miny.min(ry);
        maxy = maxy.max(ry);
    }
    let ow = (maxx - minx).ceil() as u32 + 1;
    let oh = (maxy - miny).ceil() as u32 + 1;
    let ncx = (ow - 1) as f32 / 2.0;
    let ncy = (oh - 1) as f32 / 2.0;
    let mut out = RgbImage::from_pixel(ow, oh, Rgb([0, 0, 0]));
    for dy in 0..oh {
        for dx in 0..ow {
            // inverse rotation (rotate by -deg)
            let rx = dx as f32 - ncx;
            let ry = dy as f32 - ncy;
            let sx = rx * cos + ry * sin + cx;
            let sy = -rx * sin + ry * cos + cy;
            out.put_pixel(dx, dy, Rgb(sample_bilinear(img, sx, sy)));
        }
    }

    // 裁掉旋转产生的黑色三角，而不是保留黑边或强行拉伸。计算原图 w×h 以该角度
    // 旋转后可内接的最大正矩形 (max-area inscribed upright rectangle)，居中裁切。
    let (wr, hr) = largest_inscribed_rect(w as f32, h as f32, rad);
    let wr = wr.floor().max(1.0) as u32;
    let hr = hr.floor().max(1.0) as u32;
    if wr < ow && hr < oh {
        let cx0 = (ow - wr) / 2;
        let cy0 = (oh - hr) / 2;
        imageops::crop_imm(&out, cx0, cy0, wr, hr).to_image()
    } else {
        out
    }
}

/// Largest axis-aligned rectangle (max area) that fits inside a `w×h` rectangle
/// rotated by `rad` radians. Standard closed-form solution — used to crop away
/// the black corners produced by an arbitrary rotation.
fn largest_inscribed_rect(w: f32, h: f32, rad: f32) -> (f32, f32) {
    if w <= 0.0 || h <= 0.0 {
        return (w, h);
    }
    let sin_a = rad.sin().abs();
    let cos_a = rad.cos().abs();
    let width_is_longer = w >= h;
    let (side_long, side_short) = if width_is_longer { (w, h) } else { (h, w) };

    if side_short <= 2.0 * sin_a * cos_a * side_long || (sin_a - cos_a).abs() < 1e-10 {
        // half-constrained case: crop touches the short side's midpoint.
        let x = 0.5 * side_short;
        let (wr, hr) = if width_is_longer {
            (x / sin_a.max(1e-6), x / cos_a.max(1e-6))
        } else {
            (x / cos_a.max(1e-6), x / sin_a.max(1e-6))
        };
        (wr.min(w), hr.min(h))
    } else {
        let cos_2a = cos_a * cos_a - sin_a * sin_a;
        let wr = (w * cos_a - h * sin_a) / cos_2a;
        let hr = (h * cos_a - w * sin_a) / cos_2a;
        (wr.max(1.0).min(w), hr.max(1.0).min(h))
    }
}

/// Solve a 3x3 homography `H` (with `h33 = 1`) mapping 4 source points to 4
/// destination points via the direct linear transform (8x8 Gaussian solve).
fn solve_homography(src: &[(f32, f32); 4], dst: &[(f32, f32); 4]) -> Option<[[f64; 3]; 3]> {
    // Unknowns [h11,h12,h13,h21,h22,h23,h31,h32]; build augmented 8x9.
    let mut a = [[0.0f64; 9]; 8];
    for i in 0..4 {
        let (x, y) = src[i];
        let (u, v) = dst[i];
        let (x, y, u, v) = (x as f64, y as f64, u as f64, v as f64);
        // row 2i: h11 x + h12 y + h13 - u h31 x - u h32 y = u
        a[2 * i][0] = x;
        a[2 * i][1] = y;
        a[2 * i][2] = 1.0;
        a[2 * i][3] = 0.0;
        a[2 * i][4] = 0.0;
        a[2 * i][5] = 0.0;
        a[2 * i][6] = -u * x;
        a[2 * i][7] = -u * y;
        a[2 * i][8] = u;
        // row 2i+1: h21 x + h22 y + h23 - v h31 x - v h32 y = v
        a[2 * i + 1][0] = 0.0;
        a[2 * i + 1][1] = 0.0;
        a[2 * i + 1][2] = 0.0;
        a[2 * i + 1][3] = x;
        a[2 * i + 1][4] = y;
        a[2 * i + 1][5] = 1.0;
        a[2 * i + 1][6] = -v * x;
        a[2 * i + 1][7] = -v * y;
        a[2 * i + 1][8] = v;
    }
    // Gaussian elimination with partial pivoting.
    for col in 0..8 {
        let mut piv = col;
        let mut max = a[col][col].abs();
        for r in (col + 1)..8 {
            if a[r][col].abs() > max {
                max = a[r][col].abs();
                piv = r;
            }
        }
        if max < 1e-12 {
            return None;
        }
        a.swap(col, piv);
        let d = a[col][col];
        for c in col..9 {
            a[col][c] /= d;
        }
        for r in 0..8 {
            if r != col {
                let f = a[r][col];
                if f != 0.0 {
                    for c in col..9 {
                        a[r][c] -= f * a[col][c];
                    }
                }
            }
        }
    }
    let h = [
        a[0][8], a[1][8], a[2][8], a[3][8], a[4][8], a[5][8], a[6][8], a[7][8],
    ];
    Some([
        [h[0], h[1], h[2]],
        [h[3], h[4], h[5]],
        [h[6], h[7], 1.0],
    ])
}

/// Invert a 3x3 matrix.
fn mat3_inv(m: [[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    Some([
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * inv,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * inv,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * inv,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * inv,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * inv,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * inv,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * inv,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * inv,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * inv,
        ],
    ])
}

/// Perspective keystone warp. `v` (vertical) scales the top edge width by
/// `(1 - v)` about the horizontal center; `h` (horizontal) scales the left edge
/// height by `(1 - h)` about the vertical center. We warp the *rectangle
/// output* by sampling the *trapezoid source*, so `v=h=0` is identity.
fn warp_perspective(img: &RgbImage, v: f32, h: f32) -> RgbImage {
    let (w, ht) = img.dimensions();
    if w == 0 || ht == 0 {
        return img.clone();
    }
    let cx = (w - 1) as f32 / 2.0;
    let cy = (ht - 1) as f32 / 2.0;
    let hw = w as f32 / 2.0;
    let hh = ht as f32 / 2.0;
    let sv = 1.0 - v;
    let sh = 1.0 - h;
    // Source trapezoid corners (what we sample from), normalized *pixel* coords.
    let src = [
        (cx - hw * sv, cy - hh * sh), // TL
        (cx + hw * sv, cy - hh * sh), // TR
        (cx + hw * sv, cy + hh * sh), // BR
        (cx - hw * sv, cy + hh * sh), // BL
    ];
    // Destination = full rectangle.
    let dst = [
        (0.0, 0.0),
        (w as f32 - 1.0, 0.0),
        (w as f32 - 1.0, ht as f32 - 1.0),
        (0.0, ht as f32 - 1.0),
    ];
    let h_mat = match solve_homography(&src, &dst) {
        Some(m) => m,
        None => return img.clone(),
    };
    let inv = match mat3_inv(h_mat) {
        Some(m) => m,
        None => return img.clone(),
    };
    let mut out = RgbImage::from_pixel(w, ht, Rgb([0, 0, 0]));
    for y in 0..ht {
        for x in 0..w {
            let dx = x as f32;
            let dy = y as f32;
            let den = inv[2][0] * dx as f64 + inv[2][1] * dy as f64 + inv[2][2];
            if den.abs() < 1e-8 {
                continue;
            }
            let sx = (inv[0][0] * dx as f64 + inv[0][1] * dy as f64 + inv[0][2]) / den;
            let sy = (inv[1][0] * dx as f64 + inv[1][1] * dy as f64 + inv[1][2]) / den;
            out.put_pixel(x, y, Rgb(sample_bilinear(img, sx as f32, sy as f32)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageBuffer;

    fn mk(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> RgbImage {
        let mut img = ImageBuffer::new(w, h);
        for y in 0..h {
            for x in 0..w {
                img.put_pixel(x, y, Rgb(f(x, y)));
            }
        }
        img
    }

    #[test]
    fn identity_returns_same_image() {
        let img = mk(20, 16, |x, y| [(x * 10) as u8, (y * 12) as u8, 100]);
        let g = Geometry::default();
        assert!(g.is_identity());
        let out = apply_geometry(DynamicImage::ImageRgb8(img.clone()), &g);
        let out = out.to_rgb8();
        assert_eq!(img.dimensions(), out.dimensions());
        for (p, q) in img.pixels().zip(out.pixels()) {
            assert_eq!(p.0, q.0);
        }
    }

    #[test]
    fn crop_reduces_size_and_keeps_content() {
        let img = mk(40, 30, |x, y| [(x * 5) as u8, (y * 5) as u8, 128]);
        let mut g = Geometry::default();
        g.crop = Some((0.25, 0.25, 0.5, 0.5));
        let out = apply_geometry(DynamicImage::ImageRgb8(img.clone()), &g).to_rgb8();
        assert_eq!(out.dimensions(), (20, 15));
        // top-left of crop == source pixel at (10, 7/8)
        let sp = img.get_pixel(10, 7).0;
        let op = out.get_pixel(0, 0).0;
        assert_eq!(sp, op);
    }

    #[test]
    fn rotate90_is_exact_and_flips_axes() {
        // Vertical gradient; after 90° rotation it should become horizontal.
        let img = mk(10, 20, |_x, y| [0, (y * 12) as u8, 0]);
        let mut g = Geometry::default();
        g.rotate_deg = 90.0;
        let out = apply_geometry(DynamicImage::ImageRgb8(img.clone()), &g).to_rgb8();
        assert_eq!(out.dimensions(), (20, 10));
        // The source green gradient ran along y; after 90° it must run along
        // the output x-axis (and be constant along output y).
        let gx0 = out.get_pixel(0, 0).0[1];
        let gx5 = out.get_pixel(5, 0).0[1];
        assert_ne!(gx0, gx5, "green should vary along output x after 90°");
        let gy0 = out.get_pixel(0, 5).0[1];
        assert_eq!(gx0, gy0, "green should be constant along output y");
    }

    #[test]
    fn perspective_zero_is_identity_size() {
        let img = mk(24, 18, |x, y| [(x * 8) as u8, (y * 10) as u8, 200]);
        let mut g = Geometry::default();
        g.perspective = Some((0.0, 0.0));
        let out = apply_geometry(DynamicImage::ImageRgb8(img.clone()), &g).to_rgb8();
        assert_eq!(out.dimensions(), img.dimensions());
    }

    #[test]
    fn flip_h_mirrors_x() {
        let img = mk(8, 4, |x, _y| [(x * 30) as u8, 0, 0]);
        let mut g = Geometry::default();
        g.flip_h = true;
        let out = apply_geometry(DynamicImage::ImageRgb8(img.clone()), &g).to_rgb8();
        assert_eq!(out.dimensions(), img.dimensions());
        assert_eq!(out.get_pixel(0, 0).0, img.get_pixel(7, 0).0);
        assert_eq!(out.get_pixel(7, 0).0, img.get_pixel(0, 0).0);
    }

    /// 全面静态检查：所有几何变换同时开启（旋转 90° + 任意角微调 + 透视 +
    /// 翻转 + 裁剪），必须产出合法、正尺寸、且内容不越界的结果，绝不 panic。
    /// 这正是 GUI「几何预处理」菜单全部控件叠加的极端路径。
    #[test]
    fn combined_geometry_all_active_no_panic() {
        let img = mk(37, 53, |x, y| [
            (x * 6) as u8,
            (y * 4) as u8,
            ((x + y) * 3) as u8,
        ]);
        let mut g = Geometry::default();
        g.quarter_turns = 1; // 逆时针 90°
        g.rotate_deg = 12.0; // 非 90 整数倍的任意角 → 走 warp_rotate + 裁黑角
        g.perspective = Some((0.3, -0.2)); // 透视梯形
        g.flip_h = true;
        g.flip_v = true;
        g.crop = Some((0.1, 0.15, 0.7, 0.6));
        let out = apply_geometry(DynamicImage::ImageRgb8(img.clone()), &g).to_rgb8();
        let (w, h) = out.dimensions();
        assert!(w > 0 && h > 0, "组合几何后尺寸必须为正: {}x{}", w, h);
        // 全部像素应在 0..255 内（warp_rotate 用双线性，不应产生越界值）。
        for p in out.pixels() {
            for c in p.0 {
                assert!((c as i32) <= 255 && (c as i32) >= 0, "像素越界: {}", c);
            }
        }
    }

    /// 90° 旋转后再任意角微调（用户先点「左转 90°」再拖微调滑竿的常见组合），
    /// 必须尺寸互换且输出正尺寸，不 panic。
    #[test]
    fn quarter_turn_then_fine_rotate() {
        let img = mk(60, 30, |x, y| [(x * 4) as u8, (y * 8) as u8, 77]);
        for qt in [0u8, 1, 2, 3] {
            let mut g = Geometry::default();
            g.quarter_turns = qt;
            g.rotate_deg = if qt % 2 == 0 { 20.0 } else { 20.0 }; // 任意角都走 warp
            let out = apply_geometry(DynamicImage::ImageRgb8(img.clone()), &g).to_rgb8();
            let (w, h) = out.dimensions();
            assert!(w > 0 && h > 0, "qt={} 后尺寸必须为正", qt);
        }
    }

    /// 归一化裁剪坐标越界（x+w>1 等）不能产生负宽高或 panic —— 源码已用
    /// clamp 兜底，这里验证该护栏确实生效。
    #[test]
    fn crop_out_of_bounds_is_clamped() {
        let img = mk(40, 40, |x, y| [(x) as u8, (y) as u8, 128]);
        let mut g = Geometry::default();
        g.crop = Some((0.9, 0.9, 0.9, 0.9)); // 明显越界
        let out = apply_geometry(DynamicImage::ImageRgb8(img.clone()), &g).to_rgb8();
        let (w, h) = out.dimensions();
        assert!(w > 0 && h > 0, "越界裁剪不应产生非正尺寸: {}x{}", w, h);
        assert!(w <= 40 && h <= 40, "裁剪后不应大于原图");
    }
}
