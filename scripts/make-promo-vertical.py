# -*- coding: utf-8 -*-
"""ClipHist 竖版宣传片（1080×1920 / 30fps / 30s）—— 同一套「复写纸档案」语言。

竖版不是横版裁一刀：前 3 秒必须先给动作（弹窗已经在滑入 + Alt+V + 一句话），
卡片改为纵向堆叠，左侧文字轴改为顶部文字块，安全区避开平台 UI。
"""
import os
import subprocess

import numpy as np
from PIL import Image, ImageDraw, ImageFilter, ImageFont

W, H, FPS, DUR = 1080, 1920, 30, 30.0
NF = int(FPS * DUR)
_here = os.path.dirname(os.path.abspath(__file__))
# 脚本既可能在仓库 scripts/ 下，也可能在 target/promo/ 下直接跑
ROOT = _here if os.path.exists(os.path.join(_here, "popup@2x.png")) \
    else os.path.join(_here, "..", "target", "promo")
OUT = os.path.join(ROOT, "ClipHist-promo-vertical.mp4")

INK = (14, 15, 16)
PAPER = (245, 244, 240)
SILVER = (168, 173, 181)
GRAPHITE = (58, 61, 66)
STAMP = (196, 46, 38)

F_CJK = r"C:\Windows\Fonts\msyh.ttc"
F_CJK_BD = r"C:\Windows\Fonts\msyhbd.ttc"
F_MONO = r"C:\Windows\Fonts\consola.ttf"
F_MONO_BD = r"C:\Windows\Fonts\consolab.ttf"
F_LATIN = r"C:\Windows\Fonts\bahnschrift.ttf"

X0 = 96            # 左边距（全片统一，保持"左对齐"的identity）
TOP = 250          # 文字块顶部（避开平台顶部 UI）
_fc = {}


def _load(p, size, variation=None):
    f = ImageFont.truetype(p, size)
    if variation:
        try:
            f.set_variation_by_name(variation)
        except Exception:
            pass
    return f


def cjk(size, bold=True):
    k = ("c", size, bold)
    if k not in _fc:
        _fc[k] = _load(F_CJK_BD if bold else F_CJK, size)
    return _fc[k]


def mono(size, bold=False):
    k = ("m", size, bold)
    if k not in _fc:
        _fc[k] = _load(F_MONO_BD if bold else F_MONO, size)
    return _fc[k]


def latin(size):
    k = ("l", size)
    if k not in _fc:
        try:
            _fc[k] = _load(F_LATIN, size, "SemiBold Condensed")
        except Exception:
            _fc[k] = _load(F_MONO_BD, size)
    return _fc[k]


def ease(p):
    p = max(0.0, min(1.0, p))
    return p * p * (3 - 2 * p)


def ease_out(p):
    p = max(0.0, min(1.0, p))
    return 1 - (1 - p) ** 3


def _is_cjk(ch):
    return ord(ch) > 0x2E80


def mixed(d, xy, s, sz_cjk, sz_mono, color=PAPER, gap=0):
    x, y = xy
    runs = []
    for ch in s:
        k = "cjk" if _is_cjk(ch) else "mono"
        if runs and runs[-1][0] == k:
            runs[-1][1] += ch
        else:
            runs.append([k, ch])
    for kind, text in runs:
        f = cjk(sz_cjk, True) if kind == "cjk" else mono(sz_mono, True)
        d.text((x, y), text, font=f, fill=color, anchor="ls")
        x += d.textlength(text, font=f) + gap


def build_bg():
    img = Image.new("RGB", (W, H), INK)
    d = ImageDraw.Draw(img)
    for x in range(0, W, 160):
        d.line([(x, 0), (x, H)], fill=(25, 26, 28))
    for y in range(0, H, 160):
        d.line([(0, y), (W, y)], fill=(23, 24, 26))
    vig = Image.new("L", (W, H), 0)
    ImageDraw.Draw(vig).ellipse([-W // 3, -H // 4, W + W // 3, H + H // 4], fill=44)
    vig = vig.filter(ImageFilter.GaussianBlur(200))
    dark = Image.new("RGB", (W, H), (0, 0, 0))
    img = Image.composite(img, dark, vig.point(lambda v: 255 - v // 3))
    return img.convert("RGBA")


BG = build_bg()
_rng = np.random.default_rng(11)
GRAIN = Image.fromarray(
    np.clip(128 + _rng.normal(0, 7, (H, W, 1)).repeat(3, axis=2), 0, 255).astype("uint8"), "RGB")

_cards = {}


def card(name, scale, radius=20):
    key = (name, scale, radius)
    if key in _cards:
        return _cards[key]
    src = Image.open(os.path.join(ROOT, name)).convert("RGBA")
    w, h = int(src.width * scale), int(src.height * scale)
    body = src.resize((w, h), Image.LANCZOS)
    mask = Image.new("L", (w, h), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, w - 1, h - 1], radius=radius, fill=255)
    body.putalpha(mask)
    pad = 60
    out = Image.new("RGBA", (w + pad * 2, h + pad * 2), (0, 0, 0, 0))
    sh = Image.new("L", out.size, 0)
    ImageDraw.Draw(sh).rounded_rectangle(
        [pad - 4, pad + 12, pad + w + 4, pad + h + 20], radius=radius, fill=145)
    out.paste(Image.new("RGBA", out.size, (0, 0, 0, 255)), (0, 0),
              sh.filter(ImageFilter.GaussianBlur(24)))
    g = body.copy()
    g.putalpha(g.getchannel("A").point(lambda v: v * 28 // 255))
    out.alpha_composite(g, (pad + 10, pad + 12))
    out.alpha_composite(body, (pad, pad))
    _cards[key] = out
    return out


def put_card(img, c, cx, cy, alpha=255, reveal=1.0):
    if alpha <= 0 or reveal <= 0:
        return
    c = c.copy()
    if reveal < 1.0:
        c = c.crop((0, 0, c.width, int(60 + (c.height - 60) * reveal)))
    if alpha < 255:
        c.putalpha(c.getchannel("A").point(lambda v: v * alpha // 255))
    x = int(cx - c.width / 2)
    y = int(cy - c.height / 2)
    img.alpha_composite(c, (x, y))
    if reveal < 1.0:
        hh = 40
        band = Image.new("RGBA", (c.width, hh), (0, 0, 0, 0))
        bd = ImageDraw.Draw(band)
        for i in range(hh):
            t = i / (hh - 1)
            bd.line([(0, i), (c.width, i)], fill=(255, 255, 255, int(120 * (1 - t) ** 2)))
        bd.line([(0, hh - 1), (c.width, hh - 1)], fill=(255, 255, 255, 210), width=2)
        img.alpha_composite(band, (x, y + c.height - hh + 2))


def eyebrow(d, y, s, color=SILVER, size=22):
    mixed(d, (X0, y), s, size + 2, size, color, gap=6)


def head(d, y, s, size=88, color=PAPER):
    d.text((X0, y), s, font=cjk(size, True), fill=color, anchor="ls")


def sub(d, y, s, color=SILVER, size=30):
    mixed(d, (X0, y), s, size - 1, size, color)


def chrome(img, idx, total, t):
    d = ImageDraw.Draw(img)
    # 顶部细进度（竖版没有"左侧轴"，把编号放到右上角，进度用顶线）
    d.line([(X0, 150), (W - X0, 150)], fill=(44, 46, 50), width=2)
    d.line([(X0, 150), (X0 + int((W - X0 * 2) * (t / DUR)), 150)], fill=PAPER, width=2)
    d.text((W - X0, 118), f"{idx:02d}/{total:02d}", font=mono(22, True), fill=GRAPHITE, anchor="rs")



TOTAL = 8


def s_hook(t, dur):
    """前 3 秒：动作先发生 —— 卡片滑入，标题压上去。"""
    d = ImageDraw.Draw(layer)
    slide = ease_out(t / 0.7)
    d.text((X0, 470), "剪贴板", font=cjk(112), fill=PAPER, anchor="ls")
    d.text((X0, 590), "历史", font=cjk(112), fill=PAPER, anchor="ls")
    if t > 0.25:
        q = ease((t - 0.25) / 0.6)
        d.text((X0 + 300, 586), "CLIPHIST", font=latin(64),
               fill=tuple(int(SILVER[i] * q) for i in range(3)), anchor="ls")
    if t > 0.55:
        q = ease((t - 0.55) / 0.6)
        mixed(d, (X0, 700), "复制的东西，不该丢。", 40, 40,
              tuple(int(PAPER[i] * q) for i in range(3)))
    pr = min(1.0, ease_out((t - 0.1) / 0.8) + 0.03)
    put_card(img, card("popup@2x.png", 0.80), W // 2, 1330 + int((1 - slide) * 60),
             255, pr)


def s_call(t, dur):
    d = ImageDraw.Draw(layer)
    eyebrow(d, TOP, "01 / 呼出")
    head(d, TOP + 96, "Alt+V", 104)
    sub(d, TOP + 176, "光标处弹出，不打断打字")
    if t > 0.5:
        q = ease((t - 0.5) / 0.6)
        mixed(d, (X0, TOP + 250), "enter 粘贴   esc 关闭", 24, 26,
              tuple(int(GRAPHITE[i] + 60 * q) for i in range(3)))
    pr = min(1.0, ease_out((t - 0.12) / 0.85) + 0.03)
    put_card(img, card("popup@2x.png", 0.80), W // 2, 1320, 255, pr)


def s_search(t, dur):
    d = ImageDraw.Draw(layer)
    eyebrow(d, TOP, "02 / 搜索")
    head(d, TOP + 96, "空格分词", 92)
    sub(d, TOP + 176, "多个词同时命中")
    chips = ["vs", "code", "配置"]
    for i, s in enumerate(chips):
        q = ease((t - 0.45 - i * 0.2) / 0.45)
        if q <= 0:
            continue
        w, x = 150, X0 + i * 168
        y = TOP + 236
        box = Image.new("RGBA", (w, 58), (0, 0, 0, 0))
        bd = ImageDraw.Draw(box)
        bd.rounded_rectangle([0, 0, w - 1, 57], radius=6, outline=(255, 255, 255, int(95 * q)), width=2)
        if any(ord(ch) > 0x2E80 for ch in s):
            bd.text((w // 2, 34), s, font=cjk(28), fill=(255, 255, 255, int(255 * q)), anchor="ms")
        else:
            bd.text((w // 2, 36), s, font=mono(30), fill=(255, 255, 255, int(255 * q)), anchor="ms")
        layer.alpha_composite(box, (x, y - 36))
    if t > 1.2:
        q = ease((t - 1.2) / 0.6)
        mixed(d, (X0, TOP + 350), "相关度排序 · 命中处下划线", 24, 25,
              tuple(int(SILVER[i] * q) for i in range(3)))
    pr = min(1.0, ease_out((t - 0.15) / 0.85) + 0.03)
    put_card(img, card("popup@2x.png", 0.80), W // 2, 1330, 255, pr)


def s_main(t, dur):
    d = ImageDraw.Draw(layer)
    eyebrow(d, TOP, "03 / 挑选")
    head(d, TOP + 96, "托盘单击", 92)
    sub(d, TOP + 176, "主窗口在屏幕正中")
    if t > 0.7:
        q = ease((t - 0.7) / 0.6)
        mixed(d, (X0, TOP + 246), "单击选中 · 双击粘贴", 24, 25,
              tuple(int(SILVER[i] * q) for i in range(3)))
    pr = min(1.0, ease_out((t - 0.12) / 0.85) + 0.03)
    put_card(img, card("main@2x.png", 0.56), W // 2, 1350, 255, pr)


def s_queue(t, dur):
    d = ImageDraw.Draw(layer)
    eyebrow(d, TOP, "04 / 排队")
    head(d, TOP + 96, "粘贴队列", 92)
    sub(d, TOP + 176, "空格入队，Ctrl+Alt+V 逐条粘")
    if t > 1.5:
        q = ease((t - 1.5) / 0.6)
        mixed(d, (X0, TOP + 246), "Ctrl+Shift+Enter 合并成一段", 24, 25,
              tuple(int(SILVER[i] * q) for i in range(3)))
    slips = ["张三", "138xxxx0000", "浙江省杭州市……"]
    for i in range(3):
        q = ease_out((t - 0.3 - i * 0.45) / 0.45)
        if q <= 0:
            continue
        bw, bh = 700, 112
        x = int(X0 + (1 - q) * 70)
        y = int(TOP + 330 + i * 140 - (1 - q) * 24)
        a = int(255 * q)
        slip = Image.new("RGBA", (bw + 40, bh + 40), (0, 0, 0, 0))
        sd = ImageDraw.Draw(slip)
        sd.rounded_rectangle([24, 30, 24 + bw, 30 + bh], radius=8, fill=(0, 0, 0, 100))
        sd.rounded_rectangle([20, 20, 20 + bw, 20 + bh], radius=8, fill=(PAPER[0], PAPER[1], PAPER[2], 250))
        sd.rectangle([20, 20, 104, 20 + bh], fill=INK)
        sd.rectangle([88, 20, 104, 20 + bh], fill=INK)
        sd.text((62, 20 + bh // 2), ["①", "②", "③"][i], font=cjk(42), fill=PAPER, anchor="mm")
        sd.text((130, 20 + bh // 2), slips[i], font=cjk(38, False), fill=INK, anchor="lm")
        if a < 255:
            slip.putalpha(slip.getchannel("A").point(lambda v: v * a // 255))
        layer.alpha_composite(slip, (x - 20, y - 20))


def s_types(t, dur):
    d = ImageDraw.Draw(layer)
    eyebrow(d, TOP, "05 / 设置")
    head(d, TOP + 96, "文本·文件·图片", 76)
    sub(d, TOP + 176, "全部可搜可粘")
    lines = ["热键与启动动画可改", "保留策略：条数 / 天数 / 图片总量", "数字调节：点左边减、点右边加"]
    for i, s in enumerate(lines):
        q = ease((t - 0.5 - i * 0.3) / 0.55)
        if q <= 0:
            continue
        col = tuple(int(PAPER[c] * q) for c in range(3))
        y = TOP + 250 + i * 64
        d.line([(X0, y), (X0 + 24, y)], fill=col, width=2)
        d.text((X0 + 42, y), s, font=cjk(32, False), fill=col, anchor="lm")
    pr = min(1.0, ease_out((t - 0.15) / 0.85) + 0.03)
    put_card(img, card("settings@2x.png", 0.74), W // 2, 1390, 255, pr)


def s_data(t, dur):
    d = ImageDraw.Draw(layer)
    eyebrow(d, TOP, "06 / 实测")
    rows = [("常驻私有内存", "4.5 MB"), ("第三方运行库", "0"), ("单元测试", "135 通过")]
    for i, (label, val) in enumerate(rows):
        q = ease((t - 0.2 - i * 0.35) / 0.55)
        if q <= 0:
            continue
        y = TOP + 120 + i * 150
        mixed(d, (X0, y), label, 34, 34, tuple(int(SILVER[c] * q) for c in range(3)))
        vw = sum(d.textlength(t, font=(cjk(44) if any(ord(ch) > 0x2E80 for ch in t) else mono(48, True)))
                 for t in (val,))
        mixed(d, (W - X0 - vw, y), val, 44, 48, tuple(int(PAPER[c] * q) for c in range(3)))
        # 右对齐：把数值画成右对齐需要先量宽
        d.line([(X0, y + 24), (W - X0, y + 24)], fill=(255, 255, 255, 0), width=1)
    if t > 1.4:
        q = ease((t - 1.4) / 0.6)
        mixed(d, (X0, TOP + 660), "一个 exe，一个 data.db，零安装。", 34, 34,
              tuple(int(PAPER[c] * q) for c in range(3)))


def s_end(t, dur):
    d = ImageDraw.Draw(layer)
    d.text((X0, 760), "ClipHist", font=latin(128), fill=PAPER, anchor="ls")
    d.text((X0 + 6, 836), "剪贴板历史", font=cjk(40, False), fill=SILVER, anchor="ls")
    if t > 0.45:
        q = ease((t - 0.45) / 0.6)
        d.text((X0, 960), "github.com/hunterlihao/cliphist", font=mono(34),
               fill=tuple(int(PAPER[c] * q) for c in range(3)), anchor="ls")
        mixed(d, (X0, 1024), "MIT License   ·   便携   ·   无需安装", 24, 26,
              tuple(int(SILVER[c] * q) for c in range(3)))
    if t > 0.8:
        q = ease_out((t - 0.8) / 0.45)
        ss = 1.0 + 0.14 * (1 - q)
        box = Image.new("RGBA", (250, 250), (0, 0, 0, 0))
        bd = ImageDraw.Draw(box)
        bd.rectangle([8, 8, 241, 241], outline=(STAMP[0], STAMP[1], STAMP[2], int(235 * q)), width=7)
        bd.text((125, 100), "MIT", font=latin(78), fill=(STAMP[0], STAMP[1], STAMP[2], int(235 * q)), anchor="mm")
        bd.text((125, 180), "2026", font=mono(36, True), fill=(STAMP[0], STAMP[1], STAMP[2], int(205 * q)), anchor="mm")
        box = box.rotate(-7, expand=True, resample=Image.BICUBIC)
        box = box.resize((int(box.width * ss), int(box.height * ss)), Image.LANCZOS)
        layer.alpha_composite(box, (X0 + 40, 1150))


SCENES = [
    (0.0, 3.2, s_hook),
    (3.2, 8.0, s_call),
    (8.0, 12.4, s_search),
    (12.4, 16.8, s_main),
    (16.8, 21.4, s_queue),
    (21.4, 25.4, s_types),
    (25.4, 27.6, s_data),
    (27.6, 30.0, s_end),
]
FADE = 0.38
layer = None
img = None


def render():
    global layer, img
    cmd = ["ffmpeg", "-y", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{W}x{H}",
           "-r", str(FPS), "-i", "-", "-an", "-c:v", "libx264", "-preset", "slow",
           "-crf", "19", "-pix_fmt", "yuv420p", "-movflags", "+faststart", OUT]
    proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL)
    for fi in range(NF):
        t = fi / FPS
        img = BG.copy()
        for idx, (s0, s1, fn) in enumerate(SCENES):
            if not (s0 <= t < s1):
                continue
            layer = Image.new("RGBA", (W, H), (0, 0, 0, 0))
            fn(t - s0, s1 - s0)
            a = 255
            lt = t - s0
            if lt < FADE:
                a = int(255 * (lt / FADE))
            if s1 - t < FADE:
                a = min(a, int(255 * ((s1 - t) / FADE)))
            if a < 255:
                layer.putalpha(layer.getchannel("A").point(lambda v: v * a // 255))
            img.alpha_composite(layer)
            chrome(img, idx + 1, TOTAL, t)
        img = Image.blend(img.convert("RGB"), GRAIN, 0.09)
        proc.stdin.write(img.tobytes())
        if fi % 90 == 0:
            print(f"  frame {fi}/{NF}", flush=True)
    proc.stdin.close()
    proc.wait()
    print("完成:", OUT, os.path.getsize(OUT) // 1024, "KB")


if __name__ == "__main__":
    render()
