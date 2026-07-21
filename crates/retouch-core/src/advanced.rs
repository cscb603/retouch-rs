//! Advanced retouch (原 M6): 频谱磨皮 (frequency-separation skin smoothing)
//! and 拉普拉斯金字塔融合 (multi-scale Laplacian detail / tone fusion).
//!
//! Both run on the sRGB 8-bit result, after the detail (M5) pass. Each effect
//! is a strict identity when disabled / strength = 0, so the M0 round-trip is
//! preserved for the rest of the pipeline.

use image::RgbImage;
use rayon::prelude::*;
use crate::spatial::{blend2, gaussian_blur, smoothstep};

/// 频谱磨皮 (frequency-separation skin smoothing). Separates the image into a
/// smoothed low-frequency layer (large-scale skin tone) and a high-frequency
/// layer (pores / texture), then rebuilds as `smoothed_low + texture_keep *
/// high`, masked to skin pixels so hair / background / eyes are untouched.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FreqSepSkin {
    /// Master switch.
    pub enabled: bool,
    /// Overall amount applied inside the skin mask, 0..1.
    pub strength: f32,
    /// How much of the high-frequency (pore) texture to keep, 0..1 (1 = full).
    pub texture_keep: f32,
    /// Low-frequency smoothing radius (scaled): larger = softer skin.
    pub smoothness: f32,
    /// Skin-mask softness (gaussian sigma on the skin cluster distance).
    pub mask_feather: f32,
}

impl Default for FreqSepSkin {
    fn default() -> Self {
        Self {
            enabled: false,
            strength: 0.5,
            texture_keep: 0.8,
            smoothness: 0.3,
            mask_feather: 0.5,
        }
    }
}

impl FreqSepSkin {
    pub fn is_identity(&self) -> bool {
        !self.enabled || self.strength <= 0.0
    }
}

/// 拉普拉斯金字塔融合 (multi-scale Laplacian detail fusion). Decomposes into
/// progressively blurrier low-frequency copies; the per-band (Laplacian)
/// differences are recombined with per-scale gains. With zero strength every
/// gain is 1 and the bands telescope back to the exact input (identity). This
/// adds cross-scale, natural-looking detail / local contrast — not a flat USM.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PyramidFusion {
    /// Master switch.
    pub enabled: bool,
    /// Overall strength, 0..1 (0 = off).
    pub strength: f32,
    /// Extra multiplier on the per-scale gains (default 1.0).
    pub detail_scale: f32,
}

impl Default for PyramidFusion {
    fn default() -> Self {
        Self { enabled: false, strength: 0.5, detail_scale: 1.0 }
    }
}

impl PyramidFusion {
    pub fn is_identity(&self) -> bool {
        !self.enabled || self.strength <= 0.0
    }
}

/// Advanced-retouch controls (both stages).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Advanced {
    pub freqsep: FreqSepSkin,
    pub pyramid: PyramidFusion,
}

impl Default for Advanced {
    fn default() -> Self {
        Self {
            freqsep: FreqSepSkin::default(),
            pyramid: PyramidFusion::default(),
        }
    }
}

impl Advanced {
    pub fn is_identity(&self) -> bool {
        self.freqsep.is_identity() && self.pyramid.is_identity()
    }
}

/// Apply both advanced effects (each short-circuits on identity).
pub fn apply_advanced(img: RgbImage, a: &Advanced) -> RgbImage {
    let mut out = img;
    if !a.freqsep.is_identity() {
        out = freq_sep_skin(&out, &a.freqsep);
    }
    if !a.pyramid.is_identity() {
        out = pyramid_fusion(&out, &a.pyramid);
    }
    out
}

/// YCbCr-based soft skin probability (0..1) in sRGB. A gaussian on the
/// (Cb, Cr) skin cluster with a lightness gate — cheap, no OKLCH needed.
#[inline]
fn skin_prob(r: f32, g: f32, b: f32, feather: f32) -> f32 {
    let y = 0.299 * r + 0.587 * g + 0.114 * b;
    let cb = 128.0 - 0.168736 * r - 0.331264 * g + 0.5 * b;
    let cr = 128.0 + 0.5 * r - 0.418688 * g - 0.081312 * b;
    let width = 14.0 + feather * 22.0; // sigma of the skin-cluster gaussian
    let dcb = (cb - 102.0) / width;
    let dcr = (cr - 153.0) / width;
    let w = (-0.5 * (dcb * dcb + dcr * dcr)).exp();
    let ly = smoothstep(25.0, 65.0, y) * (1.0 - smoothstep(205.0, 235.0, y));
    (w * ly).clamp(0.0, 1.0)
}

/// Frequency-separation skin smoothing, masked to skin pixels.
fn freq_sep_skin(img: &RgbImage, p: &FreqSepSkin) -> RgbImage {
    let (w, h) = img.dimensions();
    let rbase = (6.0 + p.smoothness * 18.0).round().clamp(2.0, 28.0) as u32;
    let base = gaussian_blur(img, rbase); // smoothed low-frequency skin
    let raw = img.as_raw().to_vec();
    let base_raw = base.into_raw();
    let mut mask = vec![0.0f32; (w * h) as usize];
    for i in 0..(w * h) as usize {
        mask[i] = skin_prob(
            raw[3 * i] as f32,
            raw[3 * i + 1] as f32,
            raw[3 * i + 2] as f32,
            p.mask_feather,
        );
    }
    let tk = p.texture_keep;
    let mut out = vec![0u8; raw.len()];
    out.par_chunks_mut(3).enumerate().for_each(|(idx, o)| {
        let m = (p.strength * mask[idx]).clamp(0.0, 1.0);
        for c in 0..3 {
            let s = raw[3 * idx + c] as f32;
            let b = base_raw[3 * idx + c] as f32;
            let tex = s - b;
            let smooth = b + tex * tk;
            o[c] = (s * (1.0 - m) + smooth * m).round().clamp(0.0, 255.0) as u8;
        }
    });
    RgbImage::from_raw(w, h, out).expect("freq_sep_skin: size matches")
}

/// Laplacian-pyramid multi-scale detail fusion.
fn pyramid_fusion(img: &RgbImage, p: &PyramidFusion) -> RgbImage {
    let levels: usize = 4;
    let radii = [2u32, 4, 8, 16];
    let mut lows: Vec<RgbImage> = Vec::with_capacity(levels + 1);
    lows.push(img.clone());
    for &r in radii.iter() {
        lows.push(gaussian_blur(lows.last().unwrap(), r));
    }
    // Per-scale gain profile: emphasize mid frequencies, protect the finest
    // (noise) and the coarsest (tone) bands.
    let profile = [0.5f32, 1.0, 0.8, 0.3];
    let gain: Vec<f32> = profile
        .iter()
        .map(|&pr| 1.0 + p.strength * p.detail_scale * pr)
        .collect();
    // out = lows[L] + Σ gain_k * (lows[k] - lows[k+1])  (telescopes to src @ gain 1)
    let mut out = lows[levels].clone();
    for k in 0..levels {
        let g = gain[k];
        let sub = blend2(&lows[k], &lows[k + 1], |a, b| g * (a - b));
        out = blend2(&out, &sub, |x, s| x + s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, Rgb, RgbImage};

    #[test]
    fn advanced_identity_is_noop() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(8, 8, Rgb([120u8, 90, 200])));
        let a = Advanced::default();
        assert!(a.is_identity());
        let out = apply_advanced(img.to_rgb8(), &a);
        assert_eq!(out.get_pixel(0, 0).0, [120u8, 90, 200]);
    }

    #[test]
    fn freqsep_smooths_skin_pixels() {
        // A flat skin-tone field with a single bright blemish pixel.
        let mut img = RgbImage::from_pixel(40, 40, Rgb([200u8, 150, 120]));
        // A brighter-but-still-skin-luminance blemish (the freq-sep skin mask
        // excludes pixels above ~y=235, so it must stay in the skin range).
        img.put_pixel(20, 20, Rgb([235u8, 180, 145]));
        let mut fs = FreqSepSkin::default();
        fs.enabled = true;
        fs.strength = 0.8;
        fs.texture_keep = 0.5;
        let a = Advanced { freqsep: fs, pyramid: PyramidFusion::default() };
        let out = apply_advanced(img.clone(), &a);
        // The blemish should be pulled toward the surrounding skin tone.
        let b_in = img.get_pixel(20, 20).0;
        let b_out = out.get_pixel(20, 20).0;
        // out should be closer to the skin base than the blemish was
        let dist = |p: [u8; 3], q: [u8; 3]| {
            ((p[0] as i32 - q[0] as i32).pow(2)
                + (p[1] as i32 - q[1] as i32).pow(2)
                + (p[2] as i32 - q[2] as i32).pow(2)) as f32
        };
        let d_in = dist(b_in, [200, 150, 120]);
        let d_out = dist(b_out, [200, 150, 120]);
        assert!(
            d_out < d_in,
            "blemish should move toward skin base (d {} -> {})",
            d_in,
            d_out
        );
    }

    #[test]
    fn pyramid_zero_is_identity() {
        // With strength 0, every gain is 1, so the bands must telescope to src.
        let mut img = RgbImage::new(32, 32);
        for (x, y, px) in img.enumerate_pixels_mut() {
            let v = ((x * 7 + y * 13) % 256) as u8;
            *px = Rgb([v, v.wrapping_mul(3), v.wrapping_mul(5)]);
        }
        let mut pf = PyramidFusion::default();
        pf.enabled = true;
        pf.strength = 0.0;
        let a = Advanced { freqsep: FreqSepSkin::default(), pyramid: pf };
        let out = apply_advanced(img.clone(), &a);
        for (p, q) in img.pixels().zip(out.pixels()) {
            assert_eq!(p.0, q.0, "pyramid strength=0 must be identity");
        }
    }

    #[test]
    fn pyramid_changes_image_when_strength_nonzero() {
        let mut img = RgbImage::new(48, 48);
        for (x, y, px) in img.enumerate_pixels_mut() {
            let v = ((x * 5 + y * 9) % 256) as u8;
            *px = Rgb([v, 255 - v, v / 2]);
        }
        let mut pf = PyramidFusion::default();
        pf.enabled = true;
        pf.strength = 0.5;
        let a = Advanced { freqsep: FreqSepSkin::default(), pyramid: pf };
        let out = apply_advanced(img.clone(), &a);
        let mut changed = false;
        for (p, q) in img.pixels().zip(out.pixels()) {
            if p.0 != q.0 {
                changed = true;
                break;
            }
        }
        assert!(changed, "pyramid with strength should modify the image");
    }
}
