#!/usr/bin/env python3
"""生成 Freeport 图标（零依赖）。

产出：
  build/trayTemplate.png      22x22  macOS 托盘（template image，纯黑 + alpha）
  build/trayTemplate@2x.png   44x44
  build/icon.png              512x512  应用图标（indigo 底 + 白色「港口」标记）

标记语义：一个开口向右的环 + 中心点 —— 「港口」，也是 gateway 的抽象。
"""

import math
import os
import struct
import zlib

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "build")


def write_png(path, w, h, pixels):
    """pixels: list of (r,g,b,a) 行优先"""
    raw = bytearray()
    for y in range(h):
        raw.append(0)
        for x in range(w):
            r, g, b, a = pixels[y * w + x]
            raw += bytes((r, g, b, a))

    def chunk(tag, data):
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    png += chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(png)


def smooth(v):
    return 0.0 if v <= 0 else (1.0 if v >= 1 else v)


def mark_alpha(px, py, soft=1.0):
    """归一化坐标 [-1,1]，返回标记覆盖率 0..1"""
    r = math.hypot(px, py)
    # 环：r 0.50 ~ 0.80
    ring = abs(r - 0.65) - 0.15
    a_ring = smooth(0.5 - ring * 16 * soft)

    # 右侧开口（±24°）
    ang = abs(math.degrees(math.atan2(py, px)))
    a_gap = smooth((24.0 - ang) * 0.55)

    a = a_ring * (1.0 - a_gap)

    # 中心点：r <= 0.17
    a_dot = smooth(0.5 - (r - 0.17) * 16 * soft)
    return max(a, a_dot)


def render(size, color, bg=None, radius=0.22, mark_scale=1.0, pad=0.0):
    px = []
    cr, cg, cb = color
    for y in range(size):
        for x in range(size):
            # 像素中心归一化
            nx = (x + 0.5) / size * 2 - 1
            ny = (y + 0.5) / size * 2 - 1

            if bg is None:
                a = mark_alpha(nx / mark_scale, ny / mark_scale)
                px.append((cr, cg, cb, int(round(a * 255))))
                continue

            # 圆角矩形底
            br, bgc, bb = bg
            hw = 1.0 - pad
            qx = abs(nx) - (hw - radius)
            qy = abs(ny) - (hw - radius)
            dx = max(qx, 0.0)
            dy = max(qy, 0.0)
            d = math.hypot(dx, dy) + min(max(qx, qy), 0.0) - radius
            a_bg = smooth(0.5 - d * size / 2 * 0.5)

            m = mark_alpha(nx / mark_scale, ny / mark_scale)
            a = max(a_bg, m)
            # 底与标记合成：底色按 a_bg 铺，标记为白色按 m 叠加
            rr = int(round(br * a_bg * (1 - m) + cr * m))
            gg = int(round(bgc * a_bg * (1 - m) + cg * m))
            bl = int(round(bb * a_bg * (1 - m) + cb * m))
            px.append((min(rr, 255), min(gg, 255), min(bl, 255), int(round(a * 255))))
    return px


def main():
    os.makedirs(OUT, exist_ok=True)

    # 托盘：纯黑 template image
    for name, size in (("trayTemplate.png", 22), ("trayTemplate@2x.png", 44)):
        write_png(
            os.path.join(OUT, name), size, size, render(size, (0, 0, 0), mark_scale=0.94)
        )

    # 应用图标
    write_png(
        os.path.join(OUT, "icon.png"),
        512,
        512,
        render(512, (255, 255, 255), bg=(79, 70, 229), pad=0.06, mark_scale=1.18),
    )
    print("icons written to", os.path.abspath(OUT))


if __name__ == "__main__":
    main()
