#!/usr/bin/env python3
"""真实图泛化度量：对比原图 vs 出厂档渲染。

核心看三件事（验证负向高光压缩修复是否对真实图也成立）：
1. 高光削波：near_clip(max>=250%) / hard_clip(max>=255%) 原图→渲染 是否新增。
2. 色彩保真：显著彩度区(chroma>15)的 Lab 色相漂移 max/mean（应当 <5°）。
3. 明暗位移：luma 均值漂移（出厂档应温和，不应大幅提亮/压暗）。
"""
import sys, numpy as np
from PIL import Image

def load(path):
    im = Image.open(path).convert("RGB")
    return np.asarray(im, dtype=np.float64) / 255.0

def clip_stats(rgb):
    maxc = rgb.max(axis=2)
    near = float((maxc >= 250/255).mean() * 100)
    hard = float((maxc >= 1.0).mean() * 100)
    return near, hard

def rgb_to_lab(rgb):
    c = rgb
    lin = np.where(c <= 0.04045, c / 12.92, ((c + 0.055) / 1.055) ** 2.4)
    R, G, B = lin[..., 0], lin[..., 1], lin[..., 2]
    X = 0.4124*R + 0.3576*G + 0.1805*B
    Y = 0.2126*R + 0.7152*G + 0.0722*B
    Z = 0.0193*R + 0.1192*G + 0.9505*B
    Xn, Yn, Zn = 0.95047, 1.0, 1.08883
    x, y, z = X/Xn, Y/Yn, Z/Zn
    fx = np.where(x > 0.008856, x**(1/3), 7.787*x + 16/116)
    fy = np.where(y > 0.008856, y**(1/3), 7.787*y + 16/116)
    fz = np.where(z > 0.008856, z**(1/3), 7.787*z + 16/116)
    L = 116*fy - 16
    a = 500*(fx - fy)
    b = 200*(fy - fz)
    return L, a, b

def hue_drift(o_rgb, n_rgb):
    _, ao, bo = rgb_to_lab(o_rgb)
    _, an, bn = rgb_to_lab(n_rgb)
    co = np.hypot(ao, bo)
    cn = np.hypot(an, bn)
    # 仅统计两端都明显有彩度的像素，排除暗部噪点/近灰假象
    mask = (co > 30) & (cn > 30)
    if mask.sum() == 0:
        return 0.0, 0.0, 0.0, 0.0, 0.0, int(mask.sum())
    ho = np.arctan2(bo, ao) * 180/np.pi
    hn = np.arctan2(bn, an) * 180/np.pi
    d = np.abs(hn - ho)
    d = np.where(d > 180, 360 - d, d)
    dm = d[mask]
    p10 = float((dm > 10).mean() * 100)
    p20 = float((dm > 20).mean() * 100)
    return float(dm.max()), float(dm.mean()), float(np.median(dm)), p10, p20, int(mask.sum())

def luma(rgb):
    return 0.2126*rgb[...,0] + 0.7152*rgb[...,1] + 0.0722*rgb[...,2]

def main():
    orig_dir, new_dir = sys.argv[1], sys.argv[2]
    import os, glob
    rows = []
    for op in sorted(glob.glob(os.path.join(orig_dir, "*"))):
        name = os.path.basename(op)
        np_ = os.path.join(new_dir, name)
        if not os.path.exists(np_):
            continue
        o = load(op); n = load(np_)
        if o.shape != n.shape:
            n = np.array(Image.open(np_).convert("RGB").resize(o.shape[1::-1]), dtype=np.float64)/255.0
        on, oh = clip_stats(o); nn, nh = clip_stats(n)
        mx, mn, med, p10, p20, cnt = hue_drift(o, n)
        dl = float((luma(n) - luma(o)).mean() * 100)
        rows.append((name, on, oh, nn, nh, mx, mn, med, p10, p20, dl, cnt))
    print(f"{'name':30} {'nclipΔ':>7} {'hclipΔ':>7} {'hueMax':>7} {'hueMed':>7} {'%>10°':>6} {'%>20°':>6} {'dL%':>6} {'px':>5}")
    print("-"*89)
    bad = 0
    for r in rows:
        name, on, oh, nn, nh, mx, mn, med, p10, p20, dl, cnt = r
        flag = ""
        if nn - on > 1.0: flag += " CLIP+"; bad += 1
        if nh - oh > 0.5: flag += " HARD+"; bad += 1
        if p10 > 1.0: flag += " HUE>1%"; bad += 1
        print(f"{name[:30]:30} {nn-on:+7.3f} {nh-oh:+7.3f} {mx:7.2f} {med:7.2f} {p10:6.2f} {p20:6.2f} {dl:6.2f} {cnt:5d}{flag}")
    print("-"*89)
    if rows:
        print(f"avg near_clip Δ = {np.mean([r[3]-r[1] for r in rows]):+.3f}%   "
              f"avg hard_clip Δ = {np.mean([r[4]-r[2] for r in rows]):+.3f}%   "
              f"avg hueMed = {np.mean([r[7] for r in rows]):.2f}°   "
              f"avg %pix hue>10° = {np.mean([r[8] for r in rows]):.3f}%   "
              f"avg |dL| = {np.mean([abs(r[10]) for r in rows]):.2f}%")
    print(f"\nANOMALIES (CLIP+/HARD+/HUE>1%): {bad}")

if __name__ == "__main__":
    main()
