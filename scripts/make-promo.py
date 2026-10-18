# -*- coding: utf-8 -*-
"""生成 ClipHist 宣传片（1920×1080 / 30fps / 30s）。

素材全部来自程序自己的离屏渲染（--render-* --dpi 192），
所以片子里出现的界面就是用户真正看到的样子，没有手绘示意图。
"""
import math
import os
import subprocess
import sys

from PIL import Image, ImageDraw, ImageFilter, ImageFont

W, H, FPS, DUR = 1920, 1080, 30, 30.0
NF = int(FPS * DUR)
ROOT = r"D:\code\AI-WorkSpace\clipboard-history\target\promo"
OUT = os.path.join(ROOT, "ClipHist-promo.mp4")

INK = (16, 16, 17)
PAPER = (255, 255, 255)
GRAY = (138, 138, 146)
DIM = (96, 96, 104)
LINE = (255, 255, 255, 14)

FONT_BD = r"C:\Windows\Fonts\msyhbd.ttc"
FONT_RG = r"C:\Windows\Fonts\msyh.ttc"

_cache = {}


def F(size, bold=True):
    key = (size, bold)
    if key not in _cache:
        f = ImageFont.truetype(FONT_BD if bold else FONT_RG, size)
        if hasattr(f, "set_variation_by_name"):
            pass
        _cache[key] = f
    return _cache[key]


def ease(p):
    p = max(0.0, min(1.0, p))
    return p * p * (3 - 2 * p)


def background():
    img = Image.new("RGB", (W, H), INK)
    d = ImageDraw.Draw(img)
    # 极细的竖线网格：呼应界面里的分隔线语言
    for x in range(0, W, 120):
        d.line([(x, 0), (x, H)], fill=(255, 255, 255, 6))
    # 轻微暗角
    vig = Image.new("L", (W, H), 0)
    dv = ImageDraw.Draw(vig)
    dv.ellipse([-W // 3, -H // 3, W + W // 3, H + H // 3], fill=40)
    vig = vig.filter(ImageFilter.GaussianBlur(180))
    dark = Image.new("RGB", (W, H), (0, 0, 0))
    img = Image.composite(img, dark, vig.point(lambda v: 255 - v // 2))
    return img.convert("RGBA")


BG = background()


def card(path, scale, radius=26):
    """把界面截图做成"卡片"：圆角 + 投影，预渲染一次。"""
    key = (path, scale, radius)
    if key in _cache:
        return _cache[key]
    src = Image.open(os.path.join(ROOT, path)).convert("RGBA")
    w, h = int(src.width * scale), int(src.height * scale)
    body = src.resize((w, h), Image.LANCZOS)
    r = max(6, int(radius * scale / 1.0))
    mask = Image.new("L", (w, h), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, w - 1, h - 1], radius=r, fill=255)
    out = Image.new("RGBA", (w + 80, h + 80), (0, 0, 0, 0))
    # 投影
    sh = Image.new("L", (w + 80, h + 80), 0)
    ImageDraw.Draw(sh).rounded_rectangle([40, 46, 40 + w, 46 + h], radius=r, fill=150)
    sh = sh.filter(ImageFilter.GaussianBlur(22))
    out.paste(Image.new("RGBA", out.size, (0, 0, 0, 255)), (0, 0), sh)
    layer = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    layer.paste(body, (0, 0))
    layer.putalpha(mask)
    out.alpha_composite(layer, (40, 40))
    _cache[key] = out
    return out


def put_card(img, c, cx, cy, alpha=255, scale_anim=1.0):
    if alpha <= 0:
        return
    c = c if abs(scale_anim - 1.0) < 1e-3 else c.resize(
        (int(c.width * scale_anim), int(c.height * scale_anim)), Image.LANCZOS
    )
    x = int(cx - c.width / 2)
    y = int(cy - c.height / 2)
    if alpha < 255:
        c = c.copy()
        c.putalpha(c.getchannel("A").point(lambda v: v * alpha // 255))
    img.alpha_composite(c, (x, y))


def text(d, xy, s, size, color=PAPER, anchor="la", bold=True):
    d.text(xy, s, font=F(size, bold), fill=color, anchor=anchor)


def scene_title(t, dur):
    p = ease(t / 0.9)
    off = int((1 - p) * 40)
    d = ImageDraw.Draw(layer)
    text(d, (W // 2, 420 + off), "剪贴板历史", 132, PAPER, "mm")
    text(d, (W // 2, 545 + off), "C L I P H I S T", 46, GRAY, "mm")
    if t > 0.5:
        q = ease((t - 0.5) / 0.9)
        text(d, (W // 2, 660), "Rust + Win32   ·   常驻 4.5 MB   ·   单文件便携", 40,
             (255 * q // 255, 255, 255) if False else PAPER, "mm")
        # 细横线
        lw = int(520 * q)
        d.line([(W // 2 - lw // 2, 720), (W // 2 + lw // 2, 720)], fill=(255, 255, 255, 90), width=2)


def scene_popup(t, dur, img_path, head, sub, sub2=None):
    d = ImageDraw.Draw(layer)
    p = ease(t / 0.8)
    # 左侧文案
    text(d, (150, 380), head, 84, PAPER, "ls")
    text(d, (156, 500), sub, 40, GRAY, "ls")
    if sub2:
        text(d, (156, 570), sub2, 40, GRAY, "ls")
    # 右侧弹窗（缩放进入）
    c = card(img_path, 0.72)
    put_card(img, c, 1330, 540, int(255 * p), 0.94 + 0.06 * p)
    # 光标提示：一条短竖线闪动
    if t > 0.6:
        cy = 300 + int(6 * math.sin(t * 6))
        d.line([(1180, cy), (1180, cy + 46)], fill=PAPER, width=3)


def scene_main(t, dur):
    d = ImageDraw.Draw(layer)
    p = ease(t / 0.8)
    text(d, (W // 2, 120), "托盘单击 → 屏幕正中的主窗口", 62, PAPER, "mm")
    text(d, (W // 2, 192), "单击选中 · 双击或回车粘贴 · Esc 关闭", 36, GRAY, "mm")
    c = card("main@2x.png", 0.54)
    put_card(img, c, W // 2, 682 + int((1 - p) * 46), int(255 * p), 0.96 + 0.04 * p)


def scene_queue(t, dur):
    d = ImageDraw.Draw(layer)
    text(d, (W // 2, 150), "粘贴队列", 76, PAPER, "mm")
    text(d, (W // 2, 232), "空格入队 ①②③　·　Ctrl+Alt+V 逐条粘出　·　Ctrl+Shift+Enter 合并", 38, GRAY, "mm")
    # 三张序号卡依次滑入
    labels = ["①", "②", "③"]
    samples = ["张三", "138xxxx0000", "浙江省杭州市……"]
    for i in range(3):
        start = 0.35 + i * 0.55
        q = ease((t - start) / 0.7)
        if q <= 0:
            continue
        bw, bh = 560, 116
        x = int(250 + (1 - q) * 120)
        y = int(360 + i * 152)
        a = int(255 * q)
        box = Image.new("RGBA", (bw, bh), (0, 0, 0, 0))
        bd = ImageDraw.Draw(box)
        bd.rounded_rectangle([0, 0, bw - 1, bh - 1], radius=16, fill=(255, 255, 255, 240))
        bd.rounded_rectangle([0, 0, 92, bh - 1], radius=16, fill=INK)
        bd.rectangle([60, 0, 92, bh - 1], fill=INK)
        bd.text((46, bh // 2), labels[i], font=F(52), fill=PAPER, anchor="mm")
        bd.text((122, bh // 2), samples[i], font=F(42), fill=INK, anchor="lm")
        if a < 255:
            box.putalpha(box.getchannel("A").point(lambda v: v * a // 255))
        img.alpha_composite(box, (x, y))
    # 右侧的"目标窗口"，暗示内容被粘到别处（缩小并下移，避免压住队列卡）
    if t > 1.6:
        q = ease((t - 1.6) / 0.7)
        put_card(img, card("popup-empty@2x.png", 0.46), 1480, 690, int(190 * q))


def scene_settings(t, dur):
    d = ImageDraw.Draw(layer)
    text(d, (170, 340), "文本 · 文件 · 图片", 72, PAPER, "ls")
    text(d, (176, 440), "全部可搜可粘，图片带缩略图", 40, GRAY, "ls")
    items = ["热键与启动动画可改", "保留策略：条数 / 天数 / 图片总量", "数字调节：◀ 减　▶ 加"]
    for i, s in enumerate(items):
        q = ease((t - 0.5 - i * 0.35) / 0.6)
        if q > 0:
            col = tuple(int(PAPER[c] * q) for c in range(3))
            d.ellipse([180, 552 + i * 66, 192, 564 + i * 66], fill=col)
            text(d, (216, 546 + i * 66), s, 36, col, "ls")
    put_card(img, card("settings@2x.png", 0.52), 1370, 560, 255, 1.0)


def scene_stats(t, dur):
    d = ImageDraw.Draw(layer)
    text(d, (W // 2, 250), "为什么值得换掉付费工具", 52, GRAY, "mm")
    stats = [("4.5 MB", "常驻私有内存"), ("0", "第三方运行库依赖"), ("135", "单元测试")]
    for i, (num, label) in enumerate(stats):
        q = ease((t - 0.2 - i * 0.4) / 0.7)
        if q <= 0:
            continue
        x = 480 + i * 480
        col = tuple(int(PAPER[c] * q) for c in range(3))
        text(d, (x, 520), num, 128, col, "mm")
        text(d, (x, 640), label, 34,
             (int(GRAY[0] * q), int(GRAY[1] * q), int(GRAY[2] * q)), "ma")


def scene_end(t, dur):
    d = ImageDraw.Draw(layer)
    p = ease(t / 0.9)
    text(d, (W // 2, 430), "ClipHist", 120, PAPER, "mm")
    text(d, (W // 2, 540), "剪贴板历史", 44, GRAY, "mm")
    if t > 0.6:
        q = ease((t - 0.6) / 0.8)
        text(d, (W // 2, 660), "github.com/hunterlihao/cliphist", 42,
             (int(255 * q), int(255 * q), int(255 * q)), "mm")
        text(d, (W // 2, 730), "MIT License　·　便携　·　无需安装", 32,
             (int(GRAY[0] * q), int(GRAY[1] * q), int(GRAY[2] * q)), "mm")


SCENES = [
    (0.0, 3.4, scene_title),
    (3.4, 8.0, lambda t, d: scene_popup(t, d, "popup@2x.png",
                                        "Alt+V 呼出",
                                        "光标处弹出，单击即粘贴",
                                        "不打断你的节奏")),
    (8.0, 12.6, lambda t, d: scene_popup(t, d, "popup@2x.png",
                                          "空格分词搜索",
                                          "多个词同时命中，按相关度排序",
                                          "命中处带下划线")),
    (12.6, 17.2, scene_main),
    (17.2, 21.8, scene_queue),
    (21.8, 26.0, scene_settings),
    (26.0, 30.0, None),  # stats + end 两部分共用
]

FADE = 0.45


def render():
    cmd = [
        "ffmpeg", "-y", "-f", "rawvideo", "-pix_fmt", "rgb24",
        "-s", f"{W}x{H}", "-r", str(FPS), "-i", "-",
        "-an", "-c:v", "libx264", "-preset", "medium", "-crf", "20",
        "-pix_fmt", "yuv420p", "-movflags", "+faststart", OUT,
    ]
    proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL)
    global img, layer
    for fi in range(NF):
        t = fi / FPS
        img = BG.copy()
        for idx, (s0, s1, fn) in enumerate(SCENES):
            if not (s0 <= t < s1):
                continue
            global layer
            layer = Image.new("RGBA", (W, H), (0, 0, 0, 0))
            lt = t - s0
            if idx == 6:
                # 26.0-28.0 数据；28.0-30.0 片尾
                if t < 28.0:
                    scene_stats(lt, 2.0)
                else:
                    layer = Image.new("RGBA", (W, H), (0, 0, 0, 0))
                    scene_end(t - 28.0, 2.0)
            else:
                fn(lt, s1 - s0)
            # 淡入淡出
            a = 255
            if lt < FADE:
                a = int(255 * (lt / FADE))
            if s1 - t < FADE:
                a = min(a, int(255 * ((s1 - t) / FADE)))
            if a < 255:
                layer.putalpha(layer.getchannel("A").point(lambda v: v * a // 255))
            img.alpha_composite(layer)
        proc.stdin.write(img.convert("RGB").tobytes())
        if fi % 60 == 0:
            print(f"  frame {fi}/{NF}", flush=True)
    proc.stdin.close()
    proc.wait()
    print("完成:", OUT, os.path.getsize(OUT) // 1024, "KB")


layer = None
img = None
if __name__ == "__main__":
    render()
