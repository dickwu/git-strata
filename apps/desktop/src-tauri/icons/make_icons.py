#!/usr/bin/env python3
"""Draw the app and menu-bar icons (gui.md §5.8, §8.2) with the standard
library only. Run from this directory; then `bun run tauri icon
icons/app-icon.png` (in apps/desktop) derives the bundle sizes.

Tray icons are macOS template images: black on transparent, 18 pt drawn
at 2x (36 px). Three strata lines; `pending` adds a dot, `down` a slash.
"""
import struct
import zlib

SS = 4  # supersampling per axis


def png(path, w, h, px):
    raw = b"".join(b"\x00" + bytes(px[y * w * 4:(y + 1) * w * 4]) for y in range(h))
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    data = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
    data += chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")
    open(path, "wb").write(data)


def render(w, h, shade):
    """shade(x, y) -> (r, g, b, a) floats 0..1 at a sample point."""
    px = [0] * (w * h * 4)
    for y in range(h):
        for x in range(w):
            acc = [0.0] * 4
            for sy in range(SS):
                for sx in range(SS):
                    r, g, b, a = shade(x + (sx + 0.5) / SS, y + (sy + 0.5) / SS)
                    acc[0] += r * a; acc[1] += g * a; acc[2] += b * a; acc[3] += a
            n = SS * SS
            a = acc[3] / n
            i = (y * w + x) * 4
            if a > 0:
                px[i:i + 4] = [round(acc[0] / acc[3] * 255), round(acc[1] / acc[3] * 255), round(acc[2] / acc[3] * 255), round(a * 255)]
    return px


def rrect(x, y, x0, y0, x1, y1, r):
    cx = min(max(x, x0 + r), x1 - r)
    cy = min(max(y, y0 + r), y1 - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r and x0 <= x <= x1 and y0 <= y <= y1


def hexc(s):
    return tuple(int(s[i:i + 2], 16) / 255 for i in (1, 3, 5))


def tray(kind):
    S = 36
    lines = [(9, 9, 29), (18, 5, 31), (27, 9, 27)]  # (center y, x0, x1)
    def shade(x, y):
        on = any(rrect(x, y, x0, cy - 2, x1, cy + 2, 2) for cy, x0, x1 in lines)
        if kind == "pending":
            if (x - 30) ** 2 + (y - 9) ** 2 <= 4.5 ** 2:
                return (0, 0, 0, 1)
            if (x - 30) ** 2 + (y - 9) ** 2 <= 7 ** 2:
                on = False  # clear ring around the dot
        if kind == "down":
            # distance to the line from (4,2) to (32,34)
            d = abs(32 * (x - 4) - 28 * (y - 2)) / (32 ** 2 + 28 ** 2) ** 0.5
            inside = 2 <= y <= 34
            if d <= 1.8 and inside:
                return (0, 0, 0, 1)
            if d <= 4 and inside:
                on = False
        return (0, 0, 0, 1 if on else 0)
    png(f"tray/{kind}.png", S, S, render(S, S, shade))


def app_icon():
    S = 1024
    bg = hexc("#F6F5F2")
    bands = [hexc("#C98A3D"), hexc("#2E6F8E"), hexc("#8C9A7A"), hexc("#B8AE9C")]  # chain, session, deps, base
    def shade(x, y):
        if not rrect(x, y, 100, 100, 924, 924, 185):
            return (0, 0, 0, 0)
        for i, c in enumerate(bands):
            top = 300 + i * 120
            if rrect(x, y, 230 + (i % 2) * 40, top, 794 - ((i + 1) % 2) * 40, top + 88, 22):
                return (*c, 1)
        return (*bg, 1)
    png("app-icon.png", S, S, render(S, S, shade))


if __name__ == "__main__":
    for k in ("normal", "pending", "down"):
        tray(k)
    app_icon()
