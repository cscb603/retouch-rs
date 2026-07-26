//! 污点修复进阶引擎（v0.6.2）：源块取纹理 + 无缝融合。
//!
//! 算法选型（见白皮书 WHITEPAPER-retouch-rs-v0.6.2-spot-heal.md）：
//! - `Telea`：传统 PDE 扩散（保留作兜底，极小污点/失败回退）。
//! - `FreqSep`：频率分离融合——取源块「高频纹理」+ 目标邻域「低频光照」，
//!   既有真实纹理、又不会因源块亮度不同而露馅。
//! - `Poisson`：梯度域无缝克隆（Pérez 2003，Mixed Gradients）——迭代求解泊松方程，
//!   边界 Dirichlet 固定为原图，内部梯度混合源/目标，达到完全无痕。纯 Rust 自写求解器。
//!
//! 设计要点：
//! - 源块搜索在污点周围环形区域做「边界匹配」，挑纹理健康、边界连续的健康补丁。
//! - 所有运算仅在污点局部 bounding box 上进行，分辨率无关、体量极小、预览实时。
//! - 任何异常都回退原图（或 Telea），绝不崩。

use crate::spot::{HealMode, SpotFix};
use image::{GenericImageView, GrayImage, Rgb, RgbImage};

/// 对整张 RGB 图施加一组污点笔画的修复（按 mode 分派）。空笔画 = 恒等。
///
/// `preview`：true = 交互预览（Poisson 迭代降到 80 次，拖动/松手时够快），
/// false = 导出定稿（Poisson 满 250 次迭代，追求完全无痕）。
/// FreqSep / Telea 无迭代成本，preview 对其无影响。
pub fn heal_image(img: &RgbImage, spot: &SpotFix, preview: bool) -> RgbImage {
    if spot.is_empty() {
        return img.clone();
    }
    match spot.mode {
        HealMode::Telea => crate::spot::inpaint_rgb_feathered(img, spot),
        HealMode::FreqSep => heal_strokes(img, spot, false, preview),
        HealMode::Poisson => heal_strokes(img, spot, true, preview),
    }
}

/// 逐笔画顺序愈合：后一笔可基于已愈合的前一笔找源，自然叠加。
fn heal_strokes(img: &RgbImage, spot: &SpotFix, poisson: bool, preview: bool) -> RgbImage {
    // 预览降迭代：80 次在下采样预览图上足够收敛且流畅；导出用满 250 次。
    let iters: u32 = if preview { 80 } else { 250 };
    let mut out = img.clone();
    for s in &spot.strokes {
        let (w, h) = out.dimensions();
        let cx = (s.cx * w as f32).round().clamp(0.0, (w - 1) as f32) as i32;
        let cy = (s.cy * h as f32).round().clamp(0.0, (h - 1) as f32) as i32;
        let r = (s.r_norm * (w.min(h)) as f32).clamp(1.0, 60.0).round() as i32;
        if r < 1 {
            continue;
        }
        // 找源块（PatchMatch 全局+边缘感知）；找不到 → 退化为 Telea 局部兜底（仅此笔画）。
        let (pm_iters, pm_step) = if preview {
            (4u32, (r / 2).max(3))
        } else {
            (10u32, (r / 3).max(2))
        };
        if let Some((sx, sy)) = patchmatch_source_center(&out, cx, cy, r, pm_iters, pm_step) {
            if poisson {
                poisson_heal(&mut out, cx, cy, r, sx, sy, iters);
            } else {
                freqsep_heal(&mut out, cx, cy, r, sx, sy);
            }
        } else {
            telea_single(&mut out, cx, cy, r);
        }
    }
    out
}

/// 边缘感知描述子：亮度 + x/y 梯度（中心差分）。
/// 比旧 `lum`（纯亮度）多两维梯度，使源块在「纹理/边缘」上也对齐，
/// 杜绝「一样亮但纹理/光照不匹配」的搬移（PS 修复画笔同款思路）。
#[inline]
fn edge_feat(img: &RgbImage, x: i32, y: i32) -> [f32; 3] {
    let (w, h) = (img.width() as i32, img.height() as i32);
    let lum = |xx: i32, yy: i32| -> f32 {
        let xx = xx.clamp(0, w - 1);
        let yy = yy.clamp(0, h - 1);
        let p = img.get_pixel(xx as u32, yy as u32);
        0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32
    };
    let x = x.clamp(0, w - 1);
    let y = y.clamp(0, h - 1);
    let l_c = lum(x, y);
    let gx = lum(x + 1, y) - lum(x - 1, y);
    let gy = lum(x, y + 1) - lum(x, y - 1);
    [l_c, gx, gy]
}

/// 洞上下文环 vs 候选源对应点的加权 SSD。
/// 边缘样本（|Gx|+|Gy| 大）权重高 → 强边必须对齐，弱区可宽松。
fn context_ssd(
    img: &RgbImage,
    cx: i32,
    cy: i32,
    sx: i32,
    sy: i32,
    hole: &[(i32, i32, [f32; 3])],
    edge_w: f32,
) -> f32 {
    let mut sum = 0.0;
    for &(hx, hy, hf) in hole.iter() {
        let px = hx + (sx - cx);
        let py = hy + (sy - cy);
        let sf = edge_feat(img, px, py);
        let w = 1.0 + edge_w * (hf[1].abs() + hf[2].abs());
        for k in 0..3 {
            let d = hf[k] - sf[k];
            sum += w * d * d;
        }
    }
    sum
}

/// 环形局域源搜索：在洞周围环形区域找匹配度最高的纹理补丁。
///
/// 策略（分两阶段）：
///   Phase 1 — 环形局域（主路径）：在洞外 1.5R–5R 的环形带内穷举候选，
///     用 `context_ssd`（边缘感知加权）打分。源块保证与洞同区域 → 纹理连续自然，
///     避免全局搜到远区纹理拼接时梯度搅糊。
///   Phase 2 — 全局兜底（仅 Phase 1 无候选时触发）：扩大搜索至全图，
///     但 `context_ssd` 打分加入**空间距离惩罚**，强约束远区源远离最优。
///
/// 找不到任何合法源 → None（调用方退 `telea_single`）。
fn patchmatch_source_center(
    img: &RgbImage,
    cx: i32,
    cy: i32,
    r: i32,
    iters: u32,
    step: i32,
) -> Option<(i32, i32)> {
    let (w, h) = img.dimensions();
    let (w, h) = (w as i32, h as i32);
    // 洞 bbox 越界即无合法源。
    if cx - r < 0 || cy - r < 0 || cx + r >= w || cy + r >= h {
        return None;
    }

    // 预计算洞上下文环特征（只算一次，两边通用）。
    let ctx = (r / 2).max(2);
    let stride = (r / 4).max(1);
    let r0 = r as f32;
    let r1 = (r + ctx) as f32;
    let mut hole: Vec<(i32, i32, [f32; 3])> = Vec::new();
    for y in (cy - (r + ctx)..=cy + (r + ctx)).step_by(stride as usize) {
        for x in (cx - (r + ctx)..=cx + (r + ctx)).step_by(stride as usize) {
            let dx = x - cx;
            let dy = y - cy;
            let dist = ((dx * dx + dy * dy) as f32).sqrt();
            if dist >= r0 && dist <= r1 {
                hole.push((x, y, edge_feat(img, x, y)));
            }
        }
    }
    if hole.is_empty() {
        return None;
    }

    let edge_w: f32 = 0.1; // 边缘权重（略高于默认，强边对齐更有保障）。
    let no_overlap = |sx: i32, sy: i32| -> bool {
        let dx = sx - cx;
        let dy = sy - cy;
        (dx * dx + dy * dy) as f32 > (2.0 * r as f32).powi(2)
    };
    let short_dim = w.min(h);

    // ── Phase 1：环形局域搜索（近源优先，~90% 场景走此路）──
    let r_inner = r + (r / 2).max(2);
    let r_outer = ((r as f32 * 5.0) as i32)
        .min(short_dim / 2 - r)
        .max(r_inner + 1);
    let ring_step = step.max(2);
    {
        let mut best: Option<(i32, i32, f32)> = None;
        let mut rad = r_inner;
        while rad <= r_outer {
            let circ = 2.0 * std::f32::consts::PI * rad as f32;
            let n = ((circ / ring_step as f32).max(6.0)) as i32;
            for k in 0..n {
                let a = 2.0 * std::f32::consts::PI * k as f32 / n as f32;
                let sx = cx + (rad as f32 * a.cos()).round() as i32;
                let sy = cy + (rad as f32 * a.sin()).round() as i32;
                if sx < r || sy < r || sx + r >= w || sy + r >= h {
                    continue;
                }
                if !no_overlap(sx, sy) {
                    continue;
                }
                let s = context_ssd(img, cx, cy, sx, sy, &hole, edge_w);
                if best.is_none() || s < best.unwrap().2 {
                    best = Some((sx, sy, s));
                }
            }
            rad += ring_step;
        }
        if let Some((sx, sy, _)) = best {
            return Some((sx, sy));
        }
    }

    // ── Phase 2：全局兜底（环形无候选，罕见边缘/小图场景）──
    //    加入空间距离惩罚：context_ssd + 距离权重 × (dist - 2.5R)²
    //    使得全局搜也倾向近源，避免远区纹理搅糊。
    let spatial_w = 0.01; // 空间惩罚权重（小幅，仅干扰等分时打破平局）。
    let best_dist = 2.5 * r as f32;
    {
        let mut best: Option<(i32, i32, f32)> = None;
        {
            let mut sx = r;
            while sx + r < w {
                let mut sy = r;
                while sy + r < h {
                    if no_overlap(sx, sy) {
                        let s = context_ssd(img, cx, cy, sx, sy, &hole, edge_w);
                        let dx = (sx - cx) as f32;
                        let dy = (sy - cy) as f32;
                        let dist = (dx * dx + dy * dy).sqrt();
                        let sp = spatial_w * (dist - best_dist).powi(2);
                        let score = s + sp;
                        if best.is_none() || score < best.unwrap().2 {
                            best = Some((sx, sy, score));
                        }
                    }
                    sy += step;
                }
                sx += step;
            }
        }
        if let Some((sx, sy, _)) = best {
            return Some((sx, sy));
        }
    }

    None // 全图无合法源 → Telea 兜底
}

/// 频率分离融合：源块高频纹理 + 目标邻域低频光照，disk 内填充、外缘 cosine 羽化。
fn freqsep_heal(img: &mut RgbImage, cx: i32, cy: i32, r: i32, sx: i32, sy: i32) {
    let (w, h) = img.dimensions();
    let rr = r as usize;
    let d = 2 * rr + 1;
    let bx = cx - r;
    let by = cy - r;
    if bx < 0 || by < 0 || (bx as u32 + d as u32) > w || (by as u32 + d as u32) > h {
        return;
    }
    // 目标/源 bounding box（3 通道 f32）。
    let mut tgt = vec![0.0f32; d * d * 3];
    let mut src = vec![0.0f32; d * d * 3];
    for y in 0..d {
        for x in 0..d {
            let ox = (bx + x as i32) as u32;
            let oy = (by + y as i32) as u32;
            let tp = img.get_pixel(ox, oy);
            let sxx = (bx + x as i32 + (sx - cx)).clamp(0, w as i32 - 1) as u32;
            let syy = (by + y as i32 + (sy - cy)).clamp(0, h as i32 - 1) as u32;
            let sp = img.get_pixel(sxx, syy);
            let idx = (y * d + x) * 3;
            for c in 0..3 {
                tgt[idx + c] = tp[c] as f32;
                src[idx + c] = sp[c] as f32;
            }
        }
    }
    // 关键：目标低频在洞内会被缺陷本身污染（模糊核把瑕疵平均进去），
    // 导致愈合中心偏暗。先用品洞边界环均值填平洞内，再模糊 → 干净的低频。
    let mut bavg = [0.0f32; 3];
    let mut bcount = 0u32;
    for y in 0..d {
        for x in 0..d {
            let dx = x as i32 - rr as i32;
            let dy = y as i32 - rr as i32;
            let dist = ((dx * dx + dy * dy) as f32).sqrt();
            if dist >= (r as f32 - 1.0) && dist <= (r as f32 + 1.0) {
                let idx = (y * d + x) * 3;
                for c in 0..3 {
                    bavg[c] += tgt[idx + c];
                }
                bcount += 1;
            }
        }
    }
    if bcount > 0 {
        for c in 0..3 {
            bavg[c] /= bcount as f32;
        }
        for y in 0..d {
            for x in 0..d {
                let dx = x as i32 - rr as i32;
                let dy = y as i32 - rr as i32;
                let dist = ((dx * dx + dy * dy) as f32).sqrt();
                if dist <= r as f32 {
                    let idx = (y * d + x) * 3;
                    for c in 0..3 {
                        tgt[idx + c] = bavg[c];
                    }
                }
            }
        }
    }
    // 各自高斯模糊得低频；源高频 = 源 − 源低频。
    let sigma = (rr as f32 * 0.4).max(1.0);
    let mut tgt_low = vec![0.0f32; d * d * 3];
    let mut src_low = vec![0.0f32; d * d * 3];
    for c in 0..3 {
        let tslice: Vec<f32> = tgt.chunks(3).map(|p| p[c]).collect();
        let sslice: Vec<f32> = src.chunks(3).map(|p| p[c]).collect();
        let tb = gaussian_blur(&tslice, d, d, sigma);
        let sb = gaussian_blur(&sslice, d, d, sigma);
        for i in 0..d * d {
            tgt_low[i * 3 + c] = tb[i];
            src_low[i * 3 + c] = sb[i];
        }
    }
    // 写回：愈合值 = 目标低频 + 源高频；disk 内 alpha=1，外 15% cosine 羽化。
    for y in 0..d {
        for x in 0..d {
            let dx = x as i32 - rr as i32;
            let dy = y as i32 - rr as i32;
            let dist = ((dx * dx + dy * dy) as f32).sqrt();
            if dist > r as f32 {
                continue;
            }
            let t = dist / r as f32;
            let a = if t < 0.85 {
                1.0
            } else {
                0.5 * (1.0 + (std::f32::consts::PI * (t - 0.85) / 0.15).cos())
            };
            let idx = (y * d + x) * 3;
            let ox = (bx + x as i32) as u32;
            let oy = (by + y as i32) as u32;
            let mut px = *img.get_pixel(ox, oy);
            for c in 0..3 {
                let high = src[idx + c] - src_low[idx + c];
                let val = tgt_low[idx + c] + high;
                let v = tgt[idx + c] * (1.0 - a) + val.clamp(0.0, 255.0) * a;
                px[c] = v as u8;
            }
            img.put_pixel(ox, oy, px);
        }
    }
}

/// Poisson 梯度域无缝克隆（Mixed Gradients）：自写 Gauss-Seidel 迭代求解器。
/// 边界（box 内非洞像素、及越界邻居）固定为原图值（Dirichlet）。
fn poisson_heal(img: &mut RgbImage, cx: i32, cy: i32, r: i32, sx: i32, sy: i32, iters: u32) {
    let (w, h) = img.dimensions();
    let rr = r as usize;
    let d = 2 * rr + 1;
    let bx = cx - r;
    let by = cy - r;
    if bx < 0 || by < 0 || (bx as u32 + d as u32) > w || (by as u32 + d as u32) > h {
        return;
    }
    // 洞 mask（disk 内部）。
    let mut in_mask = vec![false; d * d];
    for y in 0..d {
        for x in 0..d {
            let dx = x as i32 - rr as i32;
            let dy = y as i32 - rr as i32;
            if dx * dx + dy * dy <= rr as i32 * rr as i32 {
                in_mask[y * d + x] = true;
            }
        }
    }
    // 目标/源 bounding box（3 通道 f32）。
    let mut tgt = vec![0.0f32; d * d * 3];
    let mut src = vec![0.0f32; d * d * 3];
    for y in 0..d {
        for x in 0..d {
            let ox = (bx + x as i32) as u32;
            let oy = (by + y as i32) as u32;
            let tp = img.get_pixel(ox, oy);
            let sxx = (bx + x as i32 + (sx - cx)).clamp(0, w as i32 - 1) as u32;
            let syy = (by + y as i32 + (sy - cy)).clamp(0, h as i32 - 1) as u32;
            let sp = img.get_pixel(sxx, syy);
            let idx = (y * d + x) * 3;
            for c in 0..3 {
                tgt[idx + c] = tp[c] as f32;
                src[idx + c] = sp[c] as f32;
            }
        }
    }
    // 解向量 f：初值取目标（边界即固定为目标）。原地 Gauss-Seidel 更新。
    // iters 由调用方给：预览 80、导出 250。
    let mut f = tgt.clone();
    for _ in 0..iters {
        for y in 0..d {
            for x in 0..d {
                if !in_mask[y * d + x] {
                    continue;
                }
                let idx = (y * d + x) * 3;
                for c in 0..3 {
                    let mut sumf = 0.0f32;
                    let mut div = 0.0f32;
                    // 4-邻域：取 Mixed Gradient（源/目标梯度之大者），邻居值边界用目标。
                    for (ddx, ddy) in [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)] {
                        let nx = x as i32 + ddx;
                        let ny = y as i32 + ddy;
                        let (nb_val, gs, gt) =
                            if nx >= 0 && ny >= 0 && (nx as usize) < d && (ny as usize) < d {
                                let nidx = (ny as usize * d + nx as usize) * 3;
                                let nb = if in_mask[ny as usize * d + nx as usize] {
                                    f[nidx + c]
                                } else {
                                    tgt[nidx + c]
                                };
                                let gs = src[nidx + c] - src[idx + c];
                                let gt = tgt[nidx + c] - tgt[idx + c];
                                (nb, gs, gt)
                            } else {
                                // 越界：用 box 内最近的边界目标值。
                                let cxp = x.clamp(0, d - 1);
                                let cyp = y.clamp(0, d - 1);
                                let nidx = (cyp * d + cxp) * 3;
                                (
                                    tgt[nidx + c],
                                    src[nidx + c] - src[idx + c],
                                    tgt[nidx + c] - tgt[idx + c],
                                )
                            };
                        sumf += nb_val;
                        // 纯源梯度克隆（Pérez seamless cloning）：洞内梯度场取源块，
                        // 边界 Dirichlet 固定为目标。这样缺陷的「假强边」不会污染求解，
                        // 洞被源块纹理无痕填满，同时与目标边界光照连续。
                        let g = gs;
                        div += g;
                    }
                    f[idx + c] = (sumf - div) * 0.25;
                }
            }
        }
    }
    // 写回洞内像素。
    for y in 0..d {
        for x in 0..d {
            if !in_mask[y * d + x] {
                continue;
            }
            let idx = (y * d + x) * 3;
            let ox = (bx + x as i32) as u32;
            let oy = (by + y as i32) as u32;
            let mut px = *img.get_pixel(ox, oy);
            for c in 0..3 {
                px[c] = f[idx + c].clamp(0.0, 255.0) as u8;
            }
            img.put_pixel(ox, oy, px);
        }
    }
}

/// Telea 局部兜底：找不到好源块时，在污点局部 ROI 上跑 Telea（避免整图 mask）。
fn telea_single(img: &mut RgbImage, cx: i32, cy: i32, r: i32) {
    let (w, h) = img.dimensions();
    let rr = r as usize;
    let d = 2 * rr + 3;
    let bx = (cx - rr as i32 - 1).clamp(0, (w as i32) - d as i32);
    let by = (cy - rr as i32 - 1).clamp(0, (h as i32) - d as i32);
    let sub = img
        .view(bx as u32, by as u32, d as u32, d as u32)
        .to_image();
    // 在 ROI 内构造 disk mask（中心 = rr+1）。
    let mut mask = GrayImage::new(d as u32, d as u32);
    let cc = rr as i32 + 1;
    for y in 0..d as i32 {
        for x in 0..d as i32 {
            let dx = x - cc;
            let dy = y - cc;
            if dx * dx + dy * dy <= rr as i32 * rr as i32 {
                mask.put_pixel(x as u32, y as u32, image::Luma([255u8]));
            }
        }
    }
    let inp = crate::spot::inpaint_rgb(&sub, &mask, (r + 1).clamp(1, 60));
    for y in 0..d as u32 {
        for x in 0..d as u32 {
            img.put_pixel(
                (bx + x as i32) as u32,
                (by + y as i32) as u32,
                *inp.get_pixel(x, y),
            );
        }
    }
}

/// 可分离高斯模糊（f32 单通道）。半径 = ceil(3*sigma)，边界 clamp。
fn gaussian_blur(buf: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    if sigma <= 0.0 {
        return buf.to_vec();
    }
    let radius = (3.0 * sigma).ceil().max(1.0) as usize;
    let mut k = vec![0.0f32; 2 * radius + 1];
    let mut sum = 0.0;
    for i in 0..=2 * radius {
        let x = i as f32 - radius as f32;
        let v = (-0.5 * (x / sigma).powi(2)).exp();
        k[i] = v;
        sum += v;
    }
    for v in &mut k {
        *v /= sum;
    }
    let mut tmp = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for i in 0..=2 * radius {
                let kx = (x as i32 + i as i32 - radius as i32).clamp(0, w as i32 - 1) as usize;
                acc += buf[y * w + kx] * k[i];
            }
            tmp[y * w + x] = acc;
        }
    }
    let mut out = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for i in 0..=2 * radius {
                let ky = (y as i32 + i as i32 - radius as i32).clamp(0, h as i32 - 1) as usize;
                acc += tmp[ky * w + x] * k[i];
            }
            out[y * w + x] = acc;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spot::SpotFix;
    use image::{DynamicImage, Rgb};

    /// 构造一张带中央异色缺陷的图，用指定模式愈合，返回中心像素是否被修复到背景附近。
    fn heal_center_distance(mode: HealMode) -> f32 {
        let mut img = RgbImage::from_pixel(96, 96, Rgb([200u8, 180, 160]));
        // 皮肤纹理：加一点随机颗粒，让源块有真实纹理可取。
        for y in 0..96 {
            for x in 0..96 {
                let n = ((x * 13 + y * 7) % 17) as i32;
                let v = img.get_pixel(x, y);
                img.put_pixel(
                    x,
                    y,
                    Rgb([
                        (v[0] as i32 + n - 8).clamp(0, 255) as u8,
                        (v[1] as i32 + n - 8).clamp(0, 255) as u8,
                        (v[2] as i32 + n - 8).clamp(0, 255) as u8,
                    ]),
                );
            }
        }
        // 中央明显异色瑕疵（比背景亮很多，模拟斑点）。
        for y in 44..52 {
            for x in 44..52 {
                img.put_pixel(x, y, Rgb([20u8, 20, 20]));
            }
        }
        let mut spot = SpotFix::new();
        spot.mode = mode;
        spot.add_stroke(0.5, 0.5, 0.07); // 半径约 6.7px，覆盖 8×8 瑕疵
        let out = heal_image(&img, &spot, false);
        let fixed = out.get_pixel(48, 48).0;
        ((fixed[0] as i32 - 200).abs()
            + (fixed[1] as i32 - 180).abs()
            + (fixed[2] as i32 - 160).abs()) as f32
    }

    #[test]
    fn freqsep_heals_defect_natural() {
        let dist = heal_center_distance(HealMode::FreqSep);
        assert!(dist < 60.0, "频率分离档未修复到背景附近（dist={}）", dist);
        // 角落不受影响。
        let mut img = RgbImage::from_pixel(96, 96, Rgb([200u8, 180, 160]));
        for y in 44..52 {
            for x in 44..52 {
                img.put_pixel(x, y, Rgb([20u8, 20, 20]));
            }
        }
        let mut spot = SpotFix::new();
        spot.mode = HealMode::FreqSep;
        spot.add_stroke(0.5, 0.5, 0.07);
        let out = heal_image(&img, &spot, false);
        assert_eq!(out.get_pixel(2, 2).0, [200u8, 180, 160], "角落被误改");
        let _ = DynamicImage::ImageRgb8(out);
    }

    #[test]
    fn poisson_heals_defect_seamless() {
        let dist = heal_center_distance(HealMode::Poisson);
        assert!(dist < 60.0, "Poisson 档未修复到背景附近（dist={}）", dist);
    }

    #[test]
    fn poisson_no_panic_on_small_image() {
        // 极小的图也应安全返回（候选区可能为空 → Telea 兜底）。
        let img = RgbImage::from_pixel(16, 16, Rgb([120u8, 90, 200]));
        let mut spot = SpotFix::new();
        spot.mode = HealMode::Poisson;
        spot.add_stroke(0.5, 0.5, 0.2);
        let out = heal_image(&img, &spot, false);
        assert_eq!(out.dimensions(), (16, 16));
    }

    #[test]
    fn patchmatch_selects_nearby_source_not_remote() {
        // 左半竖向细条纹(0≤x<80)、右半横向粗条纹(80≤x<160)，两区平均亮度相同。
        // 污点落在左半(30,48)。环形搜索半径 1.5R–5R = 15–50 → 所有候选 x≤80。
        // 断言选中源 sx < cx+5R，证明环形搜索的「空间局域性」生效。
        let w = 160i32;
        let h = 96i32;
        let mut img = RgbImage::new(w as u32, h as u32);
        for y in 0..h as u32 {
            for x in 0..w as u32 {
                let (r, g, b) = if (x as i32) < w / 2 {
                    let v = if x % 4 < 2 { 230u8 } else { 50u8 }; // 竖向细条纹
                    (v, v, v)
                } else {
                    let v = if y % 16 < 8 { 230u8 } else { 50u8 }; // 横向粗条纹（同亮度）
                    (v, v, v)
                };
                img.put_pixel(x, y, Rgb([r, g, b]));
            }
        }
        let cx = 30i32;
        let cy = h / 2;
        let r = 10i32;
        let chosen =
            patchmatch_source_center(&img, cx, cy, r, 10, (r / 3).max(2)).expect("应找到源");
        // 环形搜索最大半径为 5R=50，cx+50=80 → 候选 x 不超出 80。
        assert!(
            chosen.0 < cx + 5 * r,
            "环形搜索应在局域内（sx < {}），实际 sx={}",
            cx + 5 * r,
            chosen.0
        );
    }

    #[test]
    fn patchmatch_edge_aware_matches_texture_energy() {
        // 全局竖向条纹（同纹理处处一致）。断言选中源的纹理能量（梯度幅值均值）与洞周接近，
        // 证明边缘感知描述子生效（而非仅亮度匹配），选出的源确实纹理连贯。
        let w = 160i32;
        let h = 96i32;
        let mut img = RgbImage::new(w as u32, h as u32);
        for y in 0..h as u32 {
            for x in 0..w as u32 {
                let v = if x % 4 < 2 { 220u8 } else { 40u8 }; // 均匀竖向条纹
                img.put_pixel(x, y, Rgb([v, v, v]));
            }
        }
        let cx = w / 2;
        let cy = h / 2;
        let r = 10i32;
        let chosen =
            patchmatch_source_center(&img, cx, cy, r, 10, (r / 3).max(2)).expect("应找到源");
        let energy = |ccx: i32, ccy: i32| -> f32 {
            let c = (r / 2).max(2);
            let mut s = 0.0;
            let mut n = 0;
            for yy in (ccy - (r + c)..=ccy + (r + c)).step_by(2) {
                for xx in (ccx - (r + c)..=ccx + (r + c)).step_by(2) {
                    let dx = xx - ccx;
                    let dy = yy - ccy;
                    let d = ((dx * dx + dy * dy) as f32).sqrt();
                    if d >= r as f32 && d <= (r + c) as f32 {
                        let f = edge_feat(&img, xx, yy);
                        s += (f[1].abs() + f[2].abs());
                        n += 1;
                    }
                }
            }
            if n > 0 {
                s / n as f32
            } else {
                0.0
            }
        };
        let e_hole = energy(cx, cy);
        let e_src = energy(chosen.0, chosen.1);
        let ratio = if e_hole > 1e-3 { e_src / e_hole } else { 1.0 };
        assert!(
            ratio > 0.5 && ratio < 2.0,
            "选中源纹理能量应与洞周接近（ratio={}）",
            ratio
        );
    }
}
