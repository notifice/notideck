#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Generate the Notideck app icon (stdlib only).

Outputs (relative to the repo root):
  host/crates/nd-agent/res/icon.png      256x256 RGBA
  host/crates/nd-agent/res/notideck.ico  16,24,32,48,64,256 (PNG entries)
  packaging/linux/notideck.png           256x256 RGBA
"""
import math, os, struct, zlib

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
SS = 4
TILE = (0x1C, 0x1F, 0x24)
MUTED = (0x4A, 0x50, 0x59)
ACCENT = (0xA8, 0xC4, 0x78)


def fill_rrect(buf, n, x0, y0, x1, y1, r, col):
    """Fill rounded rect (float coords, in supersampled px) into RGBA buf."""
    r = min(r, (x1 - x0) / 2, (y1 - y0) / 2)
    for y in range(max(0, int(y0)), min(n, int(math.ceil(y1)))):
        cy = y + 0.5
        if cy < y0 or cy > y1:
            continue
        dy = 0.0
        if cy < y0 + r:
            dy = y0 + r - cy
        elif cy > y1 - r:
            dy = cy - (y1 - r)
        dx = math.sqrt(max(0.0, r * r - dy * dy)) if dy else r
        left = x0 + r - dx
        right = x1 - r + dx
        xa, xb = int(math.ceil(left - 0.5)), int(math.floor(right - 0.5)) + 1
        row = y * n * 4
        for x in range(max(0, xa), min(n, xb)):
            i = row + x * 4
            buf[i:i + 4] = bytes((col[0], col[1], col[2], 255))


def render(size):
    n = size * SS
    buf = bytearray(n * n * 4)  # transparent
    s = float(n)
    margin = s * (0.03 if size >= 32 else 0.0)
    tile_r = s * (0.22 if size >= 32 else 0.25)
    fill_rrect(buf, n, margin, margin, s - margin, s - margin, tile_r, TILE)
    if size <= 24:
        cells, pad, gap, kr, accent = 2, 0.20, 0.12, 0.10, (1, 0)
        pad = 0.19 if size == 24 else 0.17
        gap = 0.12
    elif size <= 32:
        cells, pad, gap, kr, accent = 3, 0.17, 0.08, 0.10, (2, 0)
    else:
        cells, pad, gap, kr, accent = 3, 0.20, 0.06, 0.07, (2, 0)
    inner = s - 2 * margin
    p = margin + inner * pad
    g = inner * gap
    k = (s - 2 * p - g * (cells - 1)) / cells
    for cy in range(cells):
        for cx in range(cells):
            col = ACCENT if (cx, cy) == accent else MUTED
            x0 = p + cx * (k + g)
            y0 = p + cy * (k + g)
            fill_rrect(buf, n, x0, y0, x0 + k, y0 + k, k * (0.28 if size >= 48 else 0.22), col)
    # box downsample, premultiplied
    out = bytearray(size * size * 4)
    for y in range(size):
        for x in range(size):
            ar = ag = ab = aa = 0
            for sy in range(SS):
                base = ((y * SS + sy) * n + x * SS) * 4
                for sx in range(SS):
                    i = base + sx * 4
                    a = buf[i + 3]
                    aa += a
                    ar += buf[i] * a
                    ag += buf[i + 1] * a
                    ab += buf[i + 2] * a
            o = (y * size + x) * 4
            if aa:
                out[o] = round(ar / aa)
                out[o + 1] = round(ag / aa)
                out[o + 2] = round(ab / aa)
                out[o + 3] = round(aa / (SS * SS))
    return bytes(out)


def png(size, rgba):
    def chunk(t, d):
        c = struct.pack(">I", len(d)) + t + d
        return c + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    raw = b"".join(b"\x00" + rgba[y * size * 4:(y + 1) * size * 4] for y in range(size))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def ico(images):
    head = struct.pack("<HHH", 0, 1, len(images))
    off = 6 + 16 * len(images)
    ents, data = b"", b""
    for size, p in images:
        ents += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(p), off + len(data))
        data += p
    return head + ents + data


def write(path, b):
    path = os.path.join(ROOT, path)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(b)


if __name__ == "__main__":
    pngs = {s: png(s, render(s)) for s in (16, 24, 32, 48, 64, 256)}
    write("host/crates/nd-agent/res/icon.png", pngs[256])
    write("packaging/linux/notideck.png", pngs[256])
    write("host/crates/nd-agent/res/notideck.ico", ico(sorted(pngs.items())))
    print("icons written")
