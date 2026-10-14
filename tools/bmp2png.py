"""Convert a 24-bit BMP (from tools/uitest) to PNG, optionally cropped and scaled up.

usage: python -I tools/bmp2png.py in.bmp out.png [scale] [x y w h]
"""
import struct, sys, zlib

data = open(sys.argv[1], "rb").read()
off = struct.unpack_from("<I", data, 10)[0]
w, h = struct.unpack_from("<ii", data, 18)
assert struct.unpack_from("<H", data, 28)[0] == 24
scale = int(sys.argv[3]) if len(sys.argv) > 3 else 1
cx, cy, cw, ch = (int(v) for v in sys.argv[4:8]) if len(sys.argv) >= 8 else (0, 0, w, abs(h))
stride = (w * 3 + 3) & ~3
rows = []
for y in range(cy, cy + ch):
    src = (abs(h) - 1 - y) if h > 0 else y
    r = data[off + src * stride + cx * 3: off + src * stride + (cx + cw) * 3]
    rgb = bytearray(cw * 3)
    rgb[0::3] = r[2::3]; rgb[1::3] = r[1::3]; rgb[2::3] = r[0::3]
    if scale > 1:
        rgb = b"".join(bytes(rgb[i * 3:i * 3 + 3]) * scale for i in range(cw))
    rows.extend([b"\x00" + bytes(rgb)] * scale)


def chunk(t, d):
    return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)


png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", cw * scale, ch * scale, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(b"".join(rows), 6)) + chunk(b"IEND", b"")
open(sys.argv[2], "wb").write(png)
print("png", cw * scale, ch * scale)
