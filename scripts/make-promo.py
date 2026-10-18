# -*- coding: utf-8 -*-
"""ClipHist 宣传片 v2 —— 「复写纸档案」方向。

设计决定（不是默认套路）：
- 全片**单色**：石墨黑底 + 纸白卡片 + 银灰次级字。唯一的彩色是片尾一枚朱红印章，
  整片只出现一次。产品本身就是"纸与墨"，彩色会把它讲成另一个产品。
- 字体分工来自产品本身：中文用微软雅黑（界面就是它），数字与命令用等宽 Consolas
  （界面右侧的时间列就是等宽），拉丁标题用 Bahnschrift（机器铭牌感）。
  区域标题一律左对齐在固定基线，不做居中 —— 居中是最省事的做法，也最没有个性。
- 签名元素：**扫描条**。每张界面卡片都由一条发光的细横条"从上往下印"出来，
  对应产品的核心动作（复制 = 复写一份）。卡片还带一层偏移的碳复写残影。
- 左侧窄轴是"档案编号"，只标真实的推进顺序（呼出→搜索→挑选→排队→设置→数据），
  底部 2px 进度条承担时间轴。
"""
import math
import os
import subprocess

import numpy as np
from PIL import Image, ImageDraw, ImageFilter, ImageFont

W, H, FPS, DUR = 1920, 1080, 30, 30.0
NF = int(FPS * DUR)
ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "target", "promo")
OUT = os.path.join(ROOT, "ClipHist-promo2.mp4")

INK = (14, 15, 16)
INK2 = (24, 25, 27)
PAPER = (245, 244, 240)
SILVER = (168, 173, 181)
GRAPHITE = (58, 61, 66)
STAMP = (196, 46, 38)

F_CJK = r"C:\Windows\Fonts\msyh.ttc"
F_CJK_BD = r"C:\Windows\Fonts\msyhbd.ttc"
F_MONO = r"C:\Windows\Fonts\consola.ttf"
F_MONO_BD = r"C:\Windows\Fonts\consolab.ttf"
F_LATIN = r"C:\Windows\Fonts\bahnschrift.ttf"

_fc = {}


def _load(path, size, variation=None):
    f = ImageFont.truetype(path, size)
    if variation:
        try:
            f.set_variation_by_name(variation)
        except Exception:
            pass
    return f


def cjk(size, bold=True):
    k = ("cjk", size, bold)
    if k not in _fc:
        _fc[k] = _load(F_CJK_BD if bold else F_CJK, size)
    return _fc[k]


def mono(size, bold=False):
    k = ("mono", size, bold)
    if k not in _fc:
        _fc[k] = _load(F_MONO_BD if bold else F_MONO, size)
    return _fc[k]


def latin(size, variation="SemiBold Condensed"):
    k = ("lat", size, variation)
    if k not in _fc:
        try:
            _fc[k] = _load(F_LATIN, size, variation)
        except Exception:
            _fc[k] = _load(F_MONO_BD, size)
    return _fc[k]


def ease(p):
    p = max(0.0, min(1.0, p))
    return p * p * (3 - 2 * p)


def ease_out(p):
    p = max(0.0, min(1.0, p))
    return 1 - (1 - p) ** 3


# ---------------------------------------------------------------- 背景与纹理
def build_bg():
    img = Image.new("RGB", (W, H), INK)
    d = ImageDraw.Draw(img)
    # 极细网格：呼应界面分隔线；密度刻意做得像坐标纸
    for x in range(0, W, 160):
        d.line([(x, 0), (x, H)], fill=(25, 26, 28))
    for y in range(0, H, 160):
        d.line([(0, y), (W, y)], fill=(23, 24, 26))
    # 暗角
    vig = Image.new("L", (W, H), 0)
    ImageDraw.Draw(vig).ellipse([-W // 4, -H // 3, W + W // 4, H + H // 3], fill=46)
    vig = vig.filter(ImageFilter.GaussianBlur(220))
    dark = Image.new("RGB", (W, H), (0, 0, 0))
    img = Image.composite(img, dark, vig.point(lambda v: 255 - v // 3))
    return img.convert("RGBA")


BG = build_bg()

# 一次性生成的胶片颗粒
_rng = np.random.default_rng(7)
_noise = (_rng.normal(0, 7, (H, W, 1))).repeat(3, axis=2)
GRAIN = Image.fromarray(np.clip(128 + _noise, 0, 255).astype("uint8"), "RGB")


def grain(img, amount=0.10):
    return Image.blend(img, GRAIN, amount)


# ---------------------------------------------------------------- 卡片
_cards = {}


def card(name, scale, radius=22, ghost=True):
    """界面截图 → 圆角卡片 + 碳复写残影。"""
    key = (name, scale, radius, ghost)
    if key in _cards:
        return _cards[key]
    src = Image.open(os.path.join(ROOT, name)).convert("RGBA")
    w, h = int(src.width * scale), int(src.height * scale)
    body = src.resize((w, h), Image.LANCZOS)
    mask = Image.new("L", (w, h), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, w - 1, h - 1], radius=radius, fill=255)
    body.putalpha(mask)

    pad = 70
    out = Image.new("RGBA", (w + pad * 2, h + pad * 2), (0, 0, 0, 0))
    # 投影
    sh = Image.new("L", out.size, 0)
    ImageDraw.Draw(sh).rounded_rectangle(
        [pad - 4, pad + 12, pad + w + 4, pad + h + 22], radius=radius, fill=140)
    sh = sh.filter(ImageFilter.GaussianBlur(26))
    out.paste(Image.new("RGBA", out.size, (0, 0, 0, 255)), (0, 0), sh)
    if ghost:
        # 碳复写残影：向右下偏移的一份淡墨
        g = body.copy()
        g.putalpha(g.getchannel("A").point(lambda v: v * 30 // 255))
        out.alpha_composite(g, (pad + 12, pad + 14))
    out.alpha_composite(body, (pad, pad))
    _cards[key] = out
    return out


def put_card(img, c, cx, cy, alpha=255, reveal=1.0, strip=None):
    """reveal < 1 时由扫描条从上往下"印"出卡片。"""
    if alpha <= 0 or reveal <= 0:
        return
    c = c.copy()
    if reveal < 1.0:
        # 只保留顶部 reveal 比例（含投影留白）
        pad = 70
        cut = int(pad + (c.height - pad) * reveal)
        c = c.crop((0, 0, c.width, cut))
    if alpha < 255:
        c.putalpha(c.getchannel("A").point(lambda v: v * alpha // 255))
    x = int(cx - c.width / 2)
    y = int(cy - (c.height + (0 if reveal >= 1 else 0)) / 2) if reveal >= 1 else \
        int(cy - 300)
    img.alpha_composite(c, (x, y))
    if strip is not None and reveal < 1.0:
        sy = y + c.height
        draw_strip(img, x, sy, c.width, strip)


def draw_strip(img, x, y, w, strength):
    """扫描条：一条发亮的细横条 + 上方余辉。"""
    if strength <= 0:
        return
    hh = 46
    band = Image.new("RGBA", (w, hh), (0, 0, 0, 0))
    bd = ImageDraw.Draw(band)
    for i in range(hh):
        t = i / (hh - 1)
        a = int(120 * (1 - t) ** 2 * strength)
        bd.line([(0, i), (w, i)], fill=(255, 255, 255, a))
    bd.line([(0, hh - 1), (w, hh - 1)], fill=(255, 255, 255, int(220 * strength)), width=2)
    img.alpha_composite(band, (int(x), int(y - hh + 2)))


# ---------------------------------------------------------------- 文本
def eyebrow(d, xy, s, color=SILVER, size=24):
    # 字距拉开，像机器铭牌；中英混排交给 mixed
    mixed(d, xy, s, size + 2, size, color, gap=6)


def head(d, xy, s, size=76, color=PAPER):
    d.text(xy, s, font=cjk(size, True), fill=color, anchor="ls")


def sub(d, xy, s, color=SILVER, size=31):
    mixed(d, xy, s, size - 1, size, color)




def _is_cjk(ch):
    o = ord(ch)
    return o > 0x2E80 or ch in "①②③④⑤⑥⑦⑧⑨"


def mixed(d, xy, s, size_cjk, size_mono, color=PAPER, anchor="ls", gap=0):
    """中文用雅黑、ASCII 用等宽，逐段绘制并累加宽度。

    直接用 mono 画含中文的行会出豆腐块（Consolas 没有 CJK 字形），
    而整行换成雅黑又丢掉"数据用等宽"的设计意图，所以按字符类别分段。
    """
    x, y = xy
    runs = []
    for ch in s:
        kind = "cjk" if _is_cjk(ch) else "mono"
        if runs and runs[-1][0] == kind:
            runs[-1][1] += ch
        else:
            runs.append([kind, ch])
    widths = []
    for kind, text in runs:
        f = cjk(size_cjk, True) if kind == "cjk" else mono(size_mono, True)
        widths.append(d.textlength(text, font=f))
    total = sum(widths) + gap * (len(runs) - 1)
    if anchor == "ms":
        x -= total / 2
    elif anchor == "ls":
        pass
    for (kind, text), w in zip(runs, widths):
        f = cjk(size_cjk, True) if kind == "cjk" else mono(size_mono, True)
        d.text((x, y), text, font=f, fill=color, anchor="ls")
        x += w + gap


# ---------------------------------------------------------------- 场景
CAP_X = 168          # 文案左基线，全片统一
RAIL_X = 76


def rail(img, idx, total, t):
    d = ImageDraw.Draw(img)
    d.line([(RAIL_X, 120), (RAIL_X, H - 150)], fill=(52, 55, 60), width=1)
    for i in range(total):
        y = 300 + i * 78
        active = i == idx
        col = PAPER if active else (60, 63, 68)
        d.line([(RAIL_X - 12, y), (RAIL_X + 12, y)], fill=col, width=2 if active else 1)
        if active:
            d.text((RAIL_X + 26, y), f"{i:02d}", font=mono(22, True), fill=PAPER, anchor="lm")


def footer(img, t):
    d = ImageDraw.Draw(img)
    mixed(d, (CAP_X, H - 96), "CLIPHIST / RUST + WIN32 / 便携", 19, 21, (74, 78, 84))
    # 进度条：真实的 30 秒时间轴
    d.line([(CAP_X, H - 74), (W - CAP_X, H - 74)], fill=(44, 46, 50), width=2)
    d.line([(CAP_X, H - 74), (CAP_X + int((W - CAP_X * 2) * (t / DUR)), H - 74)],
           fill=PAPER, width=2)


def s_open(t, dur):
    d = ImageDraw.Draw(layer)
    p = ease(t / 0.9)
    eyebrow(d, (CAP_X, 330), "COPY LEDGER")
    # 标题分两行：中文压重，英文用机器铭牌体（不等宽）
    d.text((CAP_X, 430), "剪贴板", font=cjk(96), fill=PAPER, anchor="ls")
    d.text((CAP_X, 530), "历史", font=cjk(96), fill=PAPER, anchor="ls")
    if t > 1.0:
        q = ease((t - 1.0) / 0.9)
        col = tuple(int(PAPER[i] * q) for i in range(3))
        mixed(d, (CAP_X, 640), "复制的东西，不该丢。", 40, 40, col)
        mixed(d, (CAP_X, 700), "Rust + Win32   ·   常驻 4.5 MB   ·   单文件 data.db", 26, 27,
              (int(SILVER[0] * q), int(SILVER[1] * q), int(SILVER[2] * q)))


def s_hotkey(t, dur):
    d = ImageDraw.Draw(layer)
    eyebrow(d, (CAP_X, 330), "01 / 呼出")
    head(d, (CAP_X, 420), "Alt+V", 88)
    sub(d, (CAP_X, 500), "光标处弹出，不打断打字")
    if t > 0.5:
        q = ease((t - 0.5) / 0.7)
        mixed(d, (CAP_X, 570), "enter 粘贴    esc 关闭", 24, 26,
              tuple(int(GRAPHITE[i] + 52 * q) for i in range(3)))
    pr = ease_out((t - 0.15) / 0.85)
    put_card(img, card("popup@2x.png", 0.70), 1290, 560, 255, min(1.0, pr + 0.02), 1.0 - min(1.0, pr))


def s_search(t, dur):
    d = ImageDraw.Draw(layer)
    eyebrow(d, (CAP_X, 330), "02 / 搜索")
    head(d, (CAP_X, 420), "空格分词", 80)
    sub(d, (CAP_X, 500), "多个词同时命中")
    chips = ["vs", "code", "配置"]
    for i, s in enumerate(chips):
        q = ease((t - 0.5 - i * 0.22) / 0.5)
        if q <= 0:
            continue
        x = CAP_X + i * 150
        y = 570
        w = 128
        box = Image.new("RGBA", (w, 56), (0, 0, 0, 0))
        bd = ImageDraw.Draw(box)
        bd.rounded_rectangle([0, 0, w - 1, 55], radius=6, outline=(255, 255, 255, int(90 * q)), width=2)
        _cd = ImageDraw.Draw(box)
        mixed(_cd, (w // 2, 40), s, 28, 30, (255, 255, 255, int(255 * q)), anchor="ms")
        layer.alpha_composite(box, (x, y - 30))
    if t > 1.3:
        q = ease((t - 1.3) / 0.7)
        mixed(d, (CAP_X, 690), "结果按相关度排 · 命中处带下划线", 24, 25,
              (int(SILVER[0] * q), int(SILVER[1] * q), int(SILVER[2] * q)))
    pr = ease_out((t - 0.2) / 0.85)
    put_card(img, card("popup@2x.png", 0.70), 1290, 560, 255, min(1.0, pr + 0.02), 1.0 - min(1.0, pr))


def s_main(t, dur):
    d = ImageDraw.Draw(layer)
    eyebrow(d, (CAP_X, 300), "03 / 挑选")
    head(d, (CAP_X, 390), "托盘单击", 78)
    sub(d, (CAP_X, 470), "主窗口在屏幕正中")
    if t > 0.7:
        q = ease((t - 0.7) / 0.7)
        mixed(d, (CAP_X, 540), "单击选中 · 双击或回车粘贴", 24, 25,
              (int(SILVER[0] * q), int(SILVER[1] * q), int(SILVER[2] * q)))
    pr = ease_out((t - 0.15) / 0.85)
    put_card(img, card("main@2x.png", 0.52), 1330, 620, 255, min(1.0, pr + 0.02), 1.0 - min(1.0, pr))


def s_queue(t, dur):
    d = ImageDraw.Draw(layer)
    eyebrow(d, (CAP_X, 300), "04 / 排队")
    head(d, (CAP_X, 390), "粘贴队列", 78)
    sub(d, (CAP_X, 470), "空格入队，Ctrl+Alt+V 逐条粘")
    if t > 1.6:
        q = ease((t - 1.6) / 0.7)
        mixed(d, (CAP_X, 540), "Ctrl+Shift+Enter 合并成一段", 24, 25,
              (int(SILVER[0] * q), int(SILVER[1] * q), int(SILVER[2] * q)))
    slips = ["张三", "138xxxx0000", "浙江省杭州市……"]
    for i in range(3):
        st = 0.3 + i * 0.5
        q = ease_out((t - st) / 0.5)
        if q <= 0:
            continue
        bw, bh = 520, 104
        x = int(760 + (1 - q) * 90)
        y = int(430 + i * 132 - (1 - q) * 26)
        a = int(255 * q)
        slip = Image.new("RGBA", (bw + 40, bh + 40), (0, 0, 0, 0))
        sd = ImageDraw.Draw(slip)
        sd.rounded_rectangle([24, 30, 24 + bw, 30 + bh], radius=8, fill=(0, 0, 0, 90))
        sd.rounded_rectangle([20, 20, 20 + bw, 20 + bh], radius=8, fill=(PAPER[0], PAPER[1], PAPER[2], 250))
        sd.rectangle([20, 20, 100, 20 + bh], fill=INK)
        sd.rectangle([84, 20, 100, 20 + bh], fill=INK)
        sd.text((60, 20 + bh // 2), ["①", "②", "③"][i], font=cjk(40), fill=PAPER, anchor="mm")
        sd.text((124, 20 + bh // 2), slips[i], font=cjk(36, False), fill=INK, anchor="lm")
        if a < 255:
            slip.putalpha(slip.getchannel("A").point(lambda v: v * a // 255))
        layer.alpha_composite(slip, (x - 20, y - 20))



def s_settings(t, dur):
    d = ImageDraw.Draw(layer)
    eyebrow(d, (CAP_X, 300), "05 / 设置")
    head(d, (CAP_X, 390), "文本·文件·图片", 68)
    sub(d, (CAP_X, 462), "全部可搜可粘")
    lines = ["热键与启动动画可改", "保留策略：条数 / 天数 / 图片总量", "数字调节：点左边减、点右边加"]
    for i, s in enumerate(lines):
        q = ease((t - 0.55 - i * 0.3) / 0.55)
        if q <= 0:
            continue
        col = tuple(int(PAPER[i2] * q) for i2 in range(3))
        d.line([(CAP_X, 540 + i * 64), (CAP_X + 26, 540 + i * 64)], fill=col, width=2)
        d.text((CAP_X + 44, 540 + i * 64), s, font=cjk(32, False), fill=col, anchor="lm")
    pr = ease_out((t - 0.15) / 0.85)
    put_card(img, card("settings@2x.png", 0.50), 1360, 560, 255, min(1.0, pr + 0.02), 1.0 - min(1.0, pr))


def s_data(t, dur):
    d = ImageDraw.Draw(layer)
    eyebrow(d, (CAP_X, 300), "06 / 实测")
    rows = [("常驻私有内存", "4.5 MB"), ("第三方运行库", "0"), ("单元测试", "135 通过")]
    for i, (label, val) in enumerate(rows):
        st = 0.25 + i * 0.38
        q = ease((t - st) / 0.6)
        if q <= 0:
            continue
        y = 400 + i * 118
        mixed(d, (CAP_X, y), label, 33, 33,
              tuple(int(SILVER[c] * q) for c in range(3)))
        mixed(d, (660, y), val, 42, 46,
              tuple(int(PAPER[c] * q) for c in range(3)))
        d.line([(600, y + 14), (W - CAP_X, y + 14)], fill=(255, 255, 255, int(16 * q)), width=1)
    if t > 1.5:
        q = ease((t - 1.5) / 0.7)
        d.text((CAP_X, 800), "一个 exe，一个 data.db，零安装。", font=cjk(34, False),
               fill=tuple(int(PAPER[c] * q) for c in range(3)), anchor="ls")


def s_end(t, dur):
    d = ImageDraw.Draw(layer)
    p = ease(t / 0.8)
    d.text((CAP_X, 430), "ClipHist", font=latin(120), fill=PAPER, anchor="ls")
    d.text((CAP_X + 10, 502), "剪贴板历史", font=cjk(40, False), fill=SILVER, anchor="ls")
    if t > 0.5:
        q = ease((t - 0.5) / 0.6)
        d.text((CAP_X, 620), "github.com/hunterlihao/cliphist", font=mono(36),
               fill=tuple(int(PAPER[c] * q) for c in range(3)), anchor="ls")
        mixed(d, (CAP_X, 680), "MIT License   ·   便携   ·   无需安装", 24, 26,
              tuple(int(SILVER[c] * q) for c in range(3)))
    if t > 0.85:
        q = ease_out((t - 0.85) / 0.45)
        # 朱红印章：全片唯一的彩色，只出现一次
        ss = 1.0 + 0.14 * (1 - q)
        box = Image.new("RGBA", (240, 240), (0, 0, 0, 0))
        bd = ImageDraw.Draw(box)
        bd.rectangle([8, 8, 231, 231], outline=(STAMP[0], STAMP[1], STAMP[2], int(235 * q)), width=6)
        bd.text((120, 96), "MIT", font=latin(74), fill=(STAMP[0], STAMP[1], STAMP[2], int(235 * q)), anchor="mm")
        bd.text((120, 172), "2026", font=mono(34, True), fill=(STAMP[0], STAMP[1], STAMP[2], int(205 * q)), anchor="mm")
        box = box.rotate(-7, expand=True, resample=Image.BICUBIC)
        box = box.resize((int(box.width * ss), int(box.height * ss)), Image.LANCZOS)
        layer.alpha_composite(box, (1300, 400))


SCENES = [
    (0.0, 3.4, s_open, "COPY LEDGER"),
    (3.4, 8.2, s_hotkey, "呼出"),
    (8.2, 12.8, s_search, "搜索"),
    (12.8, 17.4, s_main, "挑选"),
    (17.4, 22.0, s_queue, "排队"),
    (22.0, 25.6, s_settings, "设置"),
    (25.6, 28.2, s_data, "实测"),
    (28.2, 30.0, s_end, "end"),
]

FADE = 0.42
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
        for idx, (s0, s1, fn, _tag) in enumerate(SCENES):
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
            if idx > 0:
                rail(img, idx, len(SCENES), t)
            footer(img, t)
        img = grain(img.convert("RGB"), 0.09)
        proc.stdin.write(img.tobytes())
        if fi % 90 == 0:
            print(f"  frame {fi}/{NF}", flush=True)
    proc.stdin.close()
    proc.wait()
    print("完成:", OUT, os.path.getsize(OUT) // 1024, "KB")


if __name__ == "__main__":
    render()
