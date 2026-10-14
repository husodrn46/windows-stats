"""Convert a 32-bit top-down BMP (Canvas::save_bmp, premultiplied BGRA) to an opaque PNG on a
grey checker-free background. usage: python -I tools/bmp32topng.py in.bmp out.png [bg_hex]"""
import struct, sys, zlib

data = open(sys.argv[1], "rb").read()
w, h = struct.unpack_from("<ii", data, 18)
h = abs(h)
bg = bytes.fromhex(sys.argv[3]) if len(sys.argv) > 3 else bytes([0x20, 0x20, 0x20])
px = data[54:]
rows = []
for y in range(h):
    row = bytearray(b"\x00")
    for x in range(w):
        b, g, r, a = px[(y * w + x) * 4:(y * w + x) * 4 + 4]
        # premultiplied over the background
        row += bytes((min(255, r + bg[0] * (255 - a) // 255), min(255, g + bg[1] * (255 - a) // 255), min(255, b + bg[2] * (255 - a) // 255)))
    rows.append(bytes(row))


def chunk(t, d):
    return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)


open(sys.argv[2], "wb").write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(b"".join(rows), 6)) + chunk(b"IEND", b""))
print("png", w, h)
