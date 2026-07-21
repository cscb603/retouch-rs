//! Shared spatial helpers used by the detail (M5) and advanced (原 M6) stages.
//!
//! Dependency-free, `rayon`-parallel, operate on sRGB 8-bit `RgbImage`. These
//! are the low-level building blocks (Gaussian blur, luminance, smoothstep,
//! per-pixel blend) so the two finishing stages don't each re-implement them.

use image::RgbImage;
use rayon::prelude::*;

/// sRGB perceptual luminance (Rec.709) of an sRGB pixel, range 0..255.
#[inline]
pub fn luma(p: [u8; 3]) -> f32 {
    0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
}

/// Hermite smoothstep in `[edge0, edge1]` -> `[0, 1]`.
#[inline]
pub fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Separable Gaussian blur with a `radius`-tap kernel (sigma = radius/2 + 0.5).
/// `radius == 0` returns a clone (identity). Horizontal then vertical passes,
/// each parallel over pixels (read-only source, distinct output rows).
pub fn gaussian_blur(img: &RgbImage, radius: u32) -> RgbImage {
    if radius == 0 {
        return img.clone();
    }
    let (w, h) = img.dimensions();
    let r = radius as i32;
    let sigma = radius as f32 / 2.0 + 0.5;
    let k: Vec<f32> = (-r..=r)
        .map(|d| (-0.5 * (d as f32 / sigma).powi(2)).exp())
        .collect();
    let ksum: f32 = k.iter().sum();
    let k: Vec<f32> = k.iter().map(|v| v / ksum).collect();

    let src = img.as_raw().to_vec();
    let mut tmp = vec![0u8; src.len()];
    // horizontal pass
    tmp.par_chunks_mut(3).enumerate().for_each(|(idx, out)| {
        let x = idx as u32 % w;
        let row = (idx as u32 / w) as usize * w as usize;
        let mut acc = [0.0f32; 3];
        for (di, &kw) in k.iter().enumerate() {
            let dx = di as i32 - r;
            let nx = (x as i32 + dx).clamp(0, w as i32 - 1) as u32;
            let j = (row + nx as usize) * 3;
            acc[0] += src[j] as f32 * kw;
            acc[1] += src[j + 1] as f32 * kw;
            acc[2] += src[j + 2] as f32 * kw;
        }
        out[0] = acc[0].round().clamp(0.0, 255.0) as u8;
        out[1] = acc[1].round().clamp(0.0, 255.0) as u8;
        out[2] = acc[2].round().clamp(0.0, 255.0) as u8;
    });
    // vertical pass
    let mut dst = vec![0u8; src.len()];
    dst.par_chunks_mut(3).enumerate().for_each(|(idx, out)| {
        let x = idx as u32 % w;
        let y = idx as u32 / w;
        let mut acc = [0.0f32; 3];
        for (di, &kw) in k.iter().enumerate() {
            let dy = di as i32 - r;
            let ny = (y as i32 + dy).clamp(0, h as i32 - 1) as u32;
            let j = ((ny as usize * w as usize) + x as usize) * 3;
            acc[0] += tmp[j] as f32 * kw;
            acc[1] += tmp[j + 1] as f32 * kw;
            acc[2] += tmp[j + 2] as f32 * kw;
        }
        out[0] = acc[0].round().clamp(0.0, 255.0) as u8;
        out[1] = acc[1].round().clamp(0.0, 255.0) as u8;
        out[2] = acc[2].round().clamp(0.0, 255.0) as u8;
    });
    RgbImage::from_raw(w, h, dst).expect("gaussian_blur: size matches")
}

/// Separable Gaussian blur for a single-channel f32 image. Returns a new
/// buffer of the same size. `radius == 0` returns a clone.
pub fn gaussian_blur_f32(buf: &[f32], w: u32, h: u32, radius: u32) -> Vec<f32> {
    if radius == 0 {
        return buf.to_vec();
    }
    let r = radius as i32;
    let sigma = radius as f32 / 2.0 + 0.5;
    let k: Vec<f32> = (-r..=r)
        .map(|d| (-0.5 * (d as f32 / sigma).powi(2)).exp())
        .collect();
    let ksum: f32 = k.iter().sum();
    let k: Vec<f32> = k.iter().map(|v| v / ksum).collect();

    let mut tmp = vec![0.0f32; buf.len()];
    tmp.par_chunks_mut(w as usize).enumerate().for_each(|(y, out_row)| {
        for x in 0..w as usize {
            let mut acc = 0.0f32;
            for (di, &kw) in k.iter().enumerate() {
                let dx = di as i32 - r;
                let nx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
                acc += buf[y * w as usize + nx] * kw;
            }
            out_row[x] = acc;
        }
    });
    let mut dst = vec![0.0f32; buf.len()];
    dst.par_chunks_mut(w as usize).enumerate().for_each(|(y, out_row)| {
        for x in 0..w as usize {
            let mut acc = 0.0f32;
            for (di, &kw) in k.iter().enumerate() {
                let dy = di as i32 - r;
                let ny = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
                acc += tmp[ny * w as usize + x] * kw;
            }
            out_row[x] = acc;
        }
    });
    dst
}
pub fn blend2(a: &RgbImage, b: &RgbImage, f: impl Fn(f32, f32) -> f32 + Sync + Send) -> RgbImage {
    let (w, h) = a.dimensions();
    assert_eq!((w, h), b.dimensions(), "blend2: size mismatch");
    let ra = a.as_raw().to_vec();
    let rb = b.as_raw().to_vec();
    let mut out = vec![0u8; ra.len()];
    out.par_chunks_mut(3).enumerate().for_each(|(idx, o)| {
        for c in 0..3 {
            let v = f(ra[3 * idx + c] as f32, rb[3 * idx + c] as f32);
            o[c] = v.clamp(0.0, 255.0).round() as u8;
        }
    });
    RgbImage::from_raw(w, h, out).expect("blend2: size matches")
}
