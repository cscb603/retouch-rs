//! Advanced scene-adaptive sharpening.
//!
//! Replaces a naive unsharp-mask (USM) with a variance-guided, non-linear,
//! scene-aware sharpening pass. It runs on the OKLab L channel to avoid color
//! shifts, and adapts its strength to portrait / landscape / clean / general
//! content.
//!
//! Pipeline:
//!   1. Scene classification from color histograms + local flatness.
//!   2. Convert sRGB -> OKLab (decoupled lightness).
//!   3. Per-pixel local standard deviation as an edge-strength map.
//!   4. Difference-of-Gaussians (DoG) to extract high-frequency detail.
//!   5. Non-linear soft-clipping enhancement (tanh) with scene-specific gains.
//!   6. Luminance + skin protection so shadows/highlights and skin stay smooth.
//!   7. Convert OKLab -> sRGB.
//!
//! All passes are rayon-parallel. No new external dependencies are required.

use crate::spatial::gaussian_blur_f32;
use image::{DynamicImage, ImageBuffer};
use palette::{IntoColor, LinSrgb, Oklab};
use rayon::prelude::*;

/// Scene category inferred from image content.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SceneKind {
    /// People / skin dominant.
    Portrait,
    /// Sky / vegetation / texture dominant.
    Landscape,
    /// Large flat color areas / minimal graphic content.
    Clean,
    /// Mixed or unclear content.
    General,
}

#[derive(Clone, Copy, Debug)]
pub struct SceneProfile {
    pub kind: SceneKind,
    pub skin_ratio: f32,
    pub sky_ratio: f32,
    pub green_ratio: f32,
    pub flat_ratio: f32,
}

/// Per-scene sharpening recipe.
struct SharpenRecipe {
    /// Overall strength (0..1).
    base_gain: f32,
    /// Extra gain for strong edges vs flat areas.
    edge_boost: f32,
    /// Reduce sharpening near black/white (0 = no protection, 1 = full).
    luma_protect: f32,
    /// For portraits: suppress sharpening on skin pixels.
    skin_protect: f32,
    /// DoG small Gaussian radius.
    radius_small: u32,
    /// DoG large Gaussian radius.
    radius_large: u32,
}

impl SceneKind {
    fn recipe(self) -> SharpenRecipe {
        match self {
            SceneKind::Portrait => SharpenRecipe {
                base_gain: 0.50,
                edge_boost: 0.70,
                luma_protect: 0.55,
                skin_protect: 0.75,
                radius_small: 1,
                radius_large: 3,
            },
            SceneKind::Landscape => SharpenRecipe {
                base_gain: 0.75,
                edge_boost: 0.95,
                luma_protect: 0.35,
                skin_protect: 0.0,
                radius_small: 1,
                radius_large: 4,
            },
            SceneKind::Clean => SharpenRecipe {
                base_gain: 0.30,
                edge_boost: 0.45,
                luma_protect: 0.7,
                skin_protect: 0.0,
                radius_small: 1,
                radius_large: 2,
            },
            SceneKind::General => SharpenRecipe {
                base_gain: 0.65,
                edge_boost: 0.85,
                luma_protect: 0.45,
                skin_protect: 0.0,
                radius_small: 1,
                radius_large: 3,
            },
        }
    }
}

/// Analyze the image and classify it into one of the scene kinds.
pub fn classify_scene(img: &DynamicImage) -> SceneProfile {
    let rgb = img.to_rgb8();
    let (w, h) = rgb.dimensions();

    // Sample on a grid for speed; 512px long-edge equivalent is plenty.
    let step = ((w.max(h) as f32 / 512.0).max(1.0).ceil()) as u32;
    let tw = (w + step - 1) / step;

    let mut total = 0usize;
    let mut skin = 0usize;
    let mut sky = 0usize;
    let mut green = 0usize;

    let mut luma = Vec::with_capacity((w * h) as usize / (step * step) as usize + 1);

    for y in (0..h).step_by(step as usize) {
        for x in (0..w).step_by(step as usize) {
            let px = rgb.get_pixel(x, y).0;
            let lin = LinSrgb::new(
                srgb_to_linear(px[0]),
                srgb_to_linear(px[1]),
                srgb_to_linear(px[2]),
            );
            let ok: Oklab<f32> = lin.into_color();
            let l = ok.l;
            let c = ok.a.hypot(ok.b);
            let hdeg = ok.a.atan2(ok.b).to_degrees().rem_euclid(360.0);

            luma.push(l);
            total += 1;

            // Skin tone: warm, low-mid chroma, mid lightness.
            if (10.0..75.0).contains(&hdeg) && c > 0.03 && c < 0.25 && (0.35..0.85).contains(&l) {
                skin += 1;
            }
            // Sky: blue-cyan, reasonable luminance.
            if (170.0..260.0).contains(&hdeg) && c > 0.05 && l > 0.15 {
                sky += 1;
            }
            // Green / vegetation.
            if (90.0..160.0).contains(&hdeg) && c > 0.05 && l > 0.1 {
                green += 1;
            }
        }
    }

    let th = (luma.len() as u32).max(1) / tw.max(1);
    let std_buf = local_std_dev(&luma, tw.max(1), th.max(1), 1);
    let flat = std_buf.iter().filter(|&&v| v < 0.02).count();

    let total_f = total as f32;
    let skin_ratio = skin as f32 / total_f;
    let sky_ratio = sky as f32 / total_f;
    let green_ratio = green as f32 / total_f;
    let flat_ratio = flat as f32 / std_buf.len().max(1) as f32;

    let kind = if skin_ratio > 0.12 {
        SceneKind::Portrait
    } else if sky_ratio + green_ratio > 0.25 {
        SceneKind::Landscape
    } else if flat_ratio > 0.45 {
        SceneKind::Clean
    } else {
        SceneKind::General
    };

    SceneProfile {
        kind,
        skin_ratio,
        sky_ratio,
        green_ratio,
        flat_ratio,
    }
}

/// Compute per-pixel local standard deviation in a grayscale f32 buffer.
fn local_std_dev(buf: &[f32], w: u32, h: u32, radius: u32) -> Vec<f32> {
    let n = (w * h) as usize;
    let r = radius as i32;
    let mut out = vec![0.0f32; n];

    out.par_chunks_mut(w as usize).enumerate().for_each(|(y, row)| {
        for x in 0..w as usize {
            let mut sum = 0.0f32;
            let mut sum2 = 0.0f32;
            let mut count = 0u32;
            for dy in -r..=r {
                let ny = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
                for dx in -r..=r {
                    let nx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
                    let v = buf[ny * w as usize + nx];
                    sum += v;
                    sum2 += v * v;
                    count += 1;
                }
            }
            let mean = sum / count as f32;
            let var = (sum2 / count as f32) - mean * mean;
            row[x] = var.max(0.0).sqrt();
        }
    });

    out
}

/// Public entry: apply scene-adaptive, non-linear sharpening.
///
/// `scale` is the current image size / original image size (<= 1.0). The more
/// the image was downscaled, the more compensation is applied. `strength` in
/// 0..1 blends between the original (0) and the full sharpened result (1).
pub fn adaptive_sharpen(img: &DynamicImage, scale: f32, strength: f32) -> DynamicImage {
    let rgb = img.to_rgb8();
    let (w, h) = rgb.dimensions();
    let n = (w * h) as usize;

    let profile = classify_scene(img);
    let recipe = profile.kind.recipe();

    // Convert to OKLab, keeping L/A/B separate for fast per-channel work.
    let mut l = vec![0.0f32; n];
    let mut a = vec![0.0f32; n];
    let mut b = vec![0.0f32; n];

    l.par_iter_mut()
        .zip(a.par_iter_mut())
        .zip(b.par_iter_mut())
        .enumerate()
        .for_each(|(i, ((lo, ao), bo))| {
            let px = rgb.get_pixel(i as u32 % w, i as u32 / w).0;
            let lin = LinSrgb::new(
                srgb_to_linear(px[0]),
                srgb_to_linear(px[1]),
                srgb_to_linear(px[2]),
            );
            let ok: Oklab<f32> = lin.into_color();
            *lo = ok.l;
            *ao = ok.a;
            *bo = ok.b;
        });

    // Local edge strength.
    let local_std = local_std_dev(&l, w, h, 1);

    // Difference-of-Gaussians for high-frequency detail.
    let blur_s = gaussian_blur_f32(&l, w, h, recipe.radius_small);
    let blur_l = gaussian_blur_f32(&l, w, h, recipe.radius_large);
    let mut dog = vec![0.0f32; n];
    for i in 0..n {
        dog[i] = blur_s[i] - blur_l[i];
    }

    // Scale compensation: the more we shrunk, the more we need to recover.
    let scale_factor = (1.0 - scale).clamp(0.0, 1.0) * 1.0;
    let gain = (recipe.base_gain + scale_factor).min(1.25);

    // Apply non-linear, scene-aware enhancement to L only.
    let mut l_out = l.clone();
    let strength = strength.clamp(0.0, 1.0);
    l_out
        .par_iter_mut()
        .enumerate()
        .for_each(|(i, lo)| {
            let edge = (local_std[i] * 6.0).clamp(0.0, 1.0);
            // Edges get more boost; flat areas get very little (noise guard).
            let boost = edge * recipe.edge_boost + (1.0 - edge) * (gain * 0.20);

            // Non-linear soft-clipping: tanh prevents overshoot / ringing.
            let h = dog[i];
            let enhanced = h * boost;
            let soft = (h * boost * 2.5).tanh() * 0.4;
            let mixed = enhanced * 0.55 + soft * 0.45;

            // Luminance protection: less sharpening near blacks and whites.
            let luma_w = 1.0 - (l[i] - 0.5).abs() * 2.0 * recipe.luma_protect;
            let luma_w = luma_w.clamp(0.15, 1.0);

            // Skin protection (only when classified as portrait and pixel looks skin-like).
            let skin_w = if recipe.skin_protect > 0.0 {
                let c = (a[i] * a[i] + b[i] * b[i]).sqrt();
                let hdeg = a[i].atan2(b[i]).to_degrees().rem_euclid(360.0);
                let is_skin = (10.0..75.0).contains(&hdeg)
                    && c > 0.03
                    && c < 0.25
                    && (0.35..0.85).contains(&l[i]);
                if is_skin {
                    1.0 - recipe.skin_protect
                } else {
                    1.0
                }
            } else {
                1.0
            };

            *lo = (l[i] + mixed * luma_w * skin_w * strength).clamp(0.0, 1.0);
        });

    // Reconstruct RGB from OKLab.
    let mut out_rgb = ImageBuffer::new(w, h);
    out_rgb
        .par_chunks_mut(3)
        .enumerate()
        .for_each(|(i, px)| {
            let ok = Oklab::new(l_out[i], a[i], b[i]);
            let lin: LinSrgb<f32> = ok.into_color();
            let (r, g, b) = lin.into_components();
            px[0] = linear_to_srgb(r);
            px[1] = linear_to_srgb(g);
            px[2] = linear_to_srgb(b);
        });

    DynamicImage::ImageRgb8(out_rgb)
}

/// sRGB encoded 8-bit -> linear-light f32 (exact sRGB transfer function).
#[inline]
fn srgb_to_linear(u: u8) -> f32 {
    let c = u as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// linear-light f32 -> sRGB encoded 8-bit.
#[inline]
fn linear_to_srgb(l: f32) -> u8 {
    let c = if l <= 0.0031308 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    (c.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GenericImageView, Rgb, RgbImage};

    #[test]
    fn identity_for_no_downscale() {
        let img = RgbImage::from_pixel(400, 40, Rgb([120u8, 120, 120]));
        let out = adaptive_sharpen(&DynamicImage::ImageRgb8(img.clone()), 1.0, 1.0);
        let diff = out.get_pixel(0, 0).0[0] as i32 - img.get_pixel(0, 0).0[0] as i32;
        assert!(
            diff.abs() <= 2,
            "flat image should stay flat, diff={}",
            diff
        );
    }

    #[test]
    fn edge_gets_sharper() {
        let mut img = RgbImage::new(400, 200);
        for (x, y, px) in img.enumerate_pixels_mut() {
            let base = if x < 200 { 80u8 } else { 200u8 };
            // Add texture far from the central edge so the image is not classified
            // as "Clean", while keeping the edge region itself clean for measurement.
            let noise = if x < 170 || x > 230 {
                ((x * 7 + y * 13) % 25) as u8
            } else {
                0
            };
            let v = base.saturating_add(noise).min(230);
            *px = Rgb([v, v, v]);
        }
        let out = adaptive_sharpen(&DynamicImage::ImageRgb8(img.clone()), 0.5, 1.0);
        let a_in = img.get_pixel(199, 100).0[0] as i32;
        let b_in = img.get_pixel(200, 100).0[0] as i32;
        let c_in = img.get_pixel(201, 100).0[0] as i32;
        let grad_in = (b_in - a_in).abs() + (c_in - b_in).abs();

        let a = out.get_pixel(199, 100).0[0] as i32;
        let b = out.get_pixel(200, 100).0[0] as i32;
        let c = out.get_pixel(201, 100).0[0] as i32;
        let grad = (b - a).abs() + (c - b).abs();
        assert!(
            grad > grad_in * 115 / 100,
            "edge should be steeper after sharpening ({} -> {})",
            grad_in,
            grad
        );
    }
}
