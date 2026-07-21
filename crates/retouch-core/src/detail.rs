//! Detail post-processing (M5): denoise / sharpen / diffuse glow.
//!
//! Applied to the **sRGB 8-bit result of the OKLCH pipeline** as a final,
//! perceptually-aligned finishing pass — JPG-friendly and independent of the
//! non-linear color stages. Every sub-effect short-circuits to a zero-copy
//! identity when its control is at neutral, so the M0 pixel-exact round-trip
//! is preserved.

use image::RgbImage;
use rayon::prelude::*;
use crate::sharpen;
use crate::spatial::{gaussian_blur, luma, smoothstep};

/// Detail finishing controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Detail {
    /// Luminance-domain bilateral denoise strength, 0..1 (0 = off). Reduces
    /// noise without destroying edges; safe on JPG.
    pub denoise: f32,
    /// Scene-adaptive, non-linear sharpening strength, 0..1 (0 = off).
    /// Replaces naive USM with OKLab-L, variance-guided, content-aware
    /// sharpening: portrait protects skin, landscape boosts texture, clean/flat
    /// areas stay smooth.
    pub sharpen: f32,
    /// Soft highlight-biased glow (dreamy diffuse), 0..1 (0 = off). Only the
    /// bright areas bloom — does NOT blur the whole frame.
    pub diffuse: f32,
}

impl Default for Detail {
    fn default() -> Self {
        Self { denoise: 0.0, sharpen: 0.0, diffuse: 0.0 }
    }
}

impl Detail {
    /// True when no detail effect is active — caller skips the pass entirely.
    pub fn is_identity(&self) -> bool {
        self.denoise <= 0.0 && self.sharpen <= 0.0 && self.diffuse <= 0.0
    }
}

/// Apply detail post-processing in order: denoise -> sharpen -> glow.
pub fn apply_detail(img: RgbImage, d: &Detail) -> RgbImage {
    let mut out = img;
    if d.denoise > 0.0 {
        out = bilateral_denoise(&out, d.denoise);
    }
    if d.sharpen > 0.0 {
        let tmp = sharpen::adaptive_sharpen(&image::DynamicImage::ImageRgb8(out), 1.0, d.sharpen);
        out = tmp.to_rgb8();
    }
    if d.diffuse > 0.0 {
        out = glow(&out, d.diffuse);
    }
    out
}

/// Luminance-domain joint-bilateral denoise. The range weight uses the sRGB
/// luminance distance, so color edges are protected while sensor noise (which
/// is correlated across R/G/B) is smoothed. Radius is fixed at 2 (5×5) for
/// performance; the range sigma scales with strength.
fn bilateral_denoise(img: &RgbImage, strength: f32) -> RgbImage {
    let (w, h) = img.dimensions();
    let src = img.as_raw().to_vec();
    let n = (w * h) as usize;
    // Precompute luminance for the range term.
    let mut lum = vec![0.0f32; n];
    for i in 0..n {
        lum[i] = luma([src[3 * i], src[3 * i + 1], src[3 * i + 2]]);
    }
    let radius = 2i32;
    // Spatial gaussian weights (5×5), sigma = 2.0, normalized.
    let mut sp = [[0.0f32; 5]; 5];
    let sig_s = 2.0f32;
    let mut ssum = 0.0f32;
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let v = (-0.5 * ((dx * dx + dy * dy) as f32) / (sig_s * sig_s)).exp();
            sp[(dy + radius) as usize][(dx + radius) as usize] = v;
            ssum += v;
        }
    }
    for row in sp.iter_mut() {
        for v in row.iter_mut() {
            *v /= ssum;
        }
    }
    let sigma_r = 6.0 + strength * 50.0; // sRGB luma units
    let inv = 1.0 / (2.0 * sigma_r * sigma_r);
    let mut dst = vec![0u8; src.len()];
    dst.par_chunks_mut(3).enumerate().for_each(|(idx, out)| {
        let x = idx as u32 % w;
        let y = idx as u32 / w;
        let lc = lum[idx];
        let mut acc = [0.0f32; 3];
        let mut wsum = 0.0f32;
        for dy in -radius..=radius {
            let ny = y as i32 + dy;
            if ny < 0 || ny >= h as i32 {
                continue;
            }
            for dx in -radius..=radius {
                let nx = x as i32 + dx;
                if nx < 0 || nx >= w as i32 {
                    continue;
                }
                let j = (ny as u32 * w + nx as u32) as usize;
                let dl = lum[j] - lc;
                let range = (-(dl * dl) * inv).exp();
                let wgt = sp[(dy + radius) as usize][(dx + radius) as usize] * range;
                acc[0] += src[3 * j] as f32 * wgt;
                acc[1] += src[3 * j + 1] as f32 * wgt;
                acc[2] += src[3 * j + 2] as f32 * wgt;
                wsum += wgt;
            }
        }
        if wsum > 0.0 {
            out[0] = (acc[0] / wsum).round().clamp(0.0, 255.0) as u8;
            out[1] = (acc[1] / wsum).round().clamp(0.0, 255.0) as u8;
            out[2] = (acc[2] / wsum).round().clamp(0.0, 255.0) as u8;
        } else {
            out[0] = src[3 * idx];
            out[1] = src[3 * idx + 1];
            out[2] = src[3 * idx + 2];
        }
    });
    RgbImage::from_raw(w, h, dst).expect("bilateral_denoise: size matches")
}

/// Soft highlight-biased glow (dreamy diffuse). A wide Gaussian is blended back
/// only where luminance is high (`smoothstep(0.45, 0.95)`), so the look blooms
/// highlights without muddying the whole frame — the opposite of a full-image
/// blur, and exactly the darktable "dreamy" guardrail.
fn glow(img: &RgbImage, amount: f32) -> RgbImage {
    let blur = gaussian_blur(img, 6);
    let ra = img.as_raw().to_vec();
    let rb = blur.into_raw();
    let (w, h) = img.dimensions();
    let mut dst = vec![0u8; ra.len()];
    dst.par_chunks_mut(3).enumerate().for_each(|(idx, out)| {
        let l = luma([ra[3 * idx], ra[3 * idx + 1], ra[3 * idx + 2]]) / 255.0;
        let wb = smoothstep(0.45, 0.95, l); // highlight bias
        let g = amount * wb;
        for c in 0..3 {
            let s = ra[3 * idx + c] as f32;
            let b = rb[3 * idx + c] as f32;
            let v = s + g * (b - s);
            out[c] = v.clamp(0.0, 255.0).round() as u8;
        }
    });
    RgbImage::from_raw(w, h, dst).expect("glow: size matches")
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, Rgb, RgbImage};
    use std::cmp::max;

    fn noise_img(w: u32, h: u32, seed: u32) -> RgbImage {
        let mut img = RgbImage::new(w, h);
        for (x, y, px) in img.enumerate_pixels_mut() {
            let i = (y * w + x) as u32;
            let v = ((i.wrapping_mul(2654435761) ^ seed) % 256) as u8;
            // mid-gray base + noise so denoise has something to clean
            let base = 120u8;
            let n = if v > 200 { 30 } else { 0 };
            *px = Rgb([base.saturating_add(n), base.saturating_add(n), base.saturating_add(n)]);
        }
        img
    }

    #[test]
    fn identity_is_noop() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(8, 8, Rgb([120u8, 90, 200])));
        let d = Detail::default();
        assert!(d.is_identity());
        let out = apply_detail(img.to_rgb8(), &d);
        assert_eq!(out.get_pixel(0, 0).0, [120u8, 90, 200]);
    }

    #[test]
    fn denoise_reduces_noise_energy() {
        let img = noise_img(40, 40, 7);
        let mut d = Detail::default();
        d.denoise = 0.6;
        let out = apply_detail(img.clone(), &d);
        // Compare local variance of a flat region before/after.
        let var = |im: &RgbImage| {
            let mut s = 0.0f64;
            let mut s2 = 0.0f64;
            let n = 20u32;
            for y in 0..n {
                for x in 0..n {
                    let p = im.get_pixel(x, y).0;
                    let v = (p[0] as f64 + p[1] as f64 + p[2] as f64) / 3.0;
                    s += v;
                    s2 += v * v;
                }
            }
            let m = s / (n * n) as f64;
            s2 / (n * n) as f64 - m * m
        };
        let v_in = var(&img);
        let v_out = var(&out);
        assert!(
            v_out < v_in,
            "denoise should reduce local variance ({} -> {})",
            v_in,
            v_out
        );
    }

    #[test]
    fn sharpen_increases_edge_contrast() {
        let mut img = RgbImage::from_pixel(40, 40, Rgb([60u8, 60, 60]));
        for (x, _y, px) in img.enumerate_pixels_mut() {
            if x < 20 {
                *px = Rgb([180u8, 180, 180]); // hard edge at x=20
            }
        }
        let mut d = Detail::default();
        d.sharpen = 0.8;
        let out = apply_detail(img.clone(), &d);
        // gradient across the edge should be steeper (overshoot) after sharpen
        let a = out.get_pixel(19, 20).0[0] as i32;
        let b = out.get_pixel(20, 20).0[0] as i32;
        let c = out.get_pixel(21, 20).0[0] as i32;
        let grad_out = (b - a).abs() + (c - b).abs();
        let a0 = img.get_pixel(19, 20).0[0] as i32;
        let b0 = img.get_pixel(20, 20).0[0] as i32;
        let c0 = img.get_pixel(21, 20).0[0] as i32;
        let grad_in = (b0 - a0).abs() + (c0 - b0).abs();
        assert!(
            grad_out > grad_in,
            "sharpen should steepen the edge gradient ({} -> {})",
            grad_in,
            grad_out
        );
    }

    #[test]
    fn glow_only_softens_highlights_keeps_darks() {
        // Dark half (flat, low luminance) + bright half with a ramp (so there is
        // high-frequency content for the highlight-biased glow to act on).
        let mut img = RgbImage::new(32, 32);
        for (x, _y, px) in img.enumerate_pixels_mut() {
            let v = if x < 16 {
                20u8
            } else {
                (200u8).saturating_add(((x - 16) * 3) as u8) // 200..248 ramp
            };
            *px = Rgb([v, v, v]);
        }
        let mut d = Detail::default();
        d.diffuse = 0.8;
        let out = apply_detail(img.clone(), &d);
        // Dark (low-luminance) region: glow weight ~0, must stay put.
        let dark_in = img.get_pixel(4, 4).0[0] as i32;
        let dark_out = out.get_pixel(4, 4).0[0] as i32;
        assert!(
            (dark_out - dark_in).abs() <= 2,
            "glow must not soften darks ({} -> {})",
            dark_in,
            dark_out
        );
        // Brightest pixel (top of the ramp) gets pulled toward its blurred
        // (lower) neighborhood — the dreamy highlight bloom.
        let hi_in = img.get_pixel(31, 4).0[0] as i32;
        let hi_out = out.get_pixel(31, 4).0[0] as i32;
        assert!(
            hi_out < hi_in,
            "glow should pull the brightest pixel toward its blur ({} -> {})",
            hi_in,
            hi_out
        );
    }
}
