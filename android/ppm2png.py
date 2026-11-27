#!/usr/bin/env python3
"""PPM (P6) -> PNG só com stdlib. Uso: ppm2png.py entrada.ppm saida.png"""
import struct, sys, zlib

def ppm_to_png(src, dst):
    data = open(src, 'rb').read()
    parts = data.split(maxsplit=4)  # P6, largura, altura, maxval, pixels
    assert parts[0] == b'P6', 'não é PPM P6'
    w, h, px = int(parts[1]), int(parts[2]), parts[4]
    assert len(px) >= w * h * 3, 'PPM truncado'
    raw = b''.join(b'\0' + px[y * w * 3:(y + 1) * w * 3] for y in range(h))
    chunk = lambda t, d: struct.pack('>I', len(d)) + t + d + struct.pack('>I', zlib.crc32(t + d) & 0xffffffff)
    png = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 2, 0, 0, 0)) \
        + chunk(b'IDAT', zlib.compress(raw, 6)) + chunk(b'IEND', b'')
    open(dst, 'wb').write(png)

if __name__ == '__main__':
    ppm_to_png(sys.argv[1], sys.argv[2])
