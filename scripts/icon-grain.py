#!/usr/bin/env python3
"""Bake luminance grain into an RGBA PNG, in place.

The app icon gets its grain here rather than in the SVG: the artwork stays clean
for every other consumer (Finder previews, browsers, cargo-bundle) and only the
built .icns carries the texture. Noise is bilinearly interpolated from a coarse
grid, so neighbours share a value and the result reads as grain instead of
per-pixel static, and it is skipped where alpha is zero so the icon keeps its
transparent surround.

Usage: icon-grain.py <file.png> [cell px] [amplitude] [seed]

Grain is incompressible, so a 1024px master with grain costs roughly 40% more
than the same master without it. CELL and AMPLITUDE are the look knobs; a larger
CELL and a smaller AMPLITUDE both shrink the encoded result. At CELL=20 the
mottle reads about 5px wide in a 256px preview and averages away by icon size.
"""

import random
import struct
import sys
import zlib

CELL = 20
AMPLITUDE = 5.0
SEED = 11


def unfilter(filter_type, line, previous, stride):
    for x in range(stride):
        left = line[x - 4] if x >= 4 else 0
        up = previous[x]
        upleft = previous[x - 4] if x >= 4 else 0
        if filter_type == 1:
            line[x] = (line[x] + left) & 255
        elif filter_type == 2:
            line[x] = (line[x] + up) & 255
        elif filter_type == 3:
            line[x] = (line[x] + ((left + up) >> 1)) & 255
        elif filter_type == 4:
            estimate = left + up - upleft
            nearest_left = abs(estimate - left)
            nearest_up = abs(estimate - up)
            nearest_upleft = abs(estimate - upleft)
            # Ties resolve towards left, then up, as the PNG spec requires;
            # picking by smallest predictor value instead shifts every tied pixel.
            if nearest_left <= nearest_up and nearest_left <= nearest_upleft:
                predictor = left
            elif nearest_up <= nearest_upleft:
                predictor = up
            else:
                predictor = upleft
            line[x] = (line[x] + predictor) & 255


def read_png(path):
    blob = open(path, "rb").read()
    position, header, compressed = 8, None, bytearray()
    while position < len(blob):
        length, kind = struct.unpack(">I4s", blob[position:position + 8])
        body = blob[position + 8:position + 8 + length]
        if kind == b"IHDR":
            header = body
        elif kind == b"IDAT":
            compressed += body
        position += 12 + length
    if header is None:
        raise SystemExit(f"{path}: not a PNG")
    width, height, depth, color, _, _, interlace = struct.unpack(">IIBBBBB", header[:13])
    if depth != 8 or color != 6 or interlace:
        raise SystemExit(f"{path}: expected a non-interlaced 8-bit RGBA PNG")

    raw, stride, offset = zlib.decompress(bytes(compressed)), width * 4, 0
    lines, previous = [], bytearray(stride)
    for _ in range(height):
        filter_type = raw[offset]
        offset += 1
        line = bytearray(raw[offset:offset + stride])
        offset += stride
        unfilter(filter_type, line, previous, stride)
        lines.append(line)
        previous = line
    return header, width, height, lines


def add_grain(width, lines, cell, amplitude, seed):
    random.seed(seed)
    grid = width // cell + 2
    noise = [[random.uniform(-amplitude, amplitude) for _ in range(grid)] for _ in range(grid)]
    for y, line in enumerate(lines):
        row, fraction_y = divmod(y, cell)
        for x in range(width):
            if not line[x * 4 + 3]:
                continue
            column, fraction_x = divmod(x, cell)
            top = noise[row][column] * (cell - fraction_x) + noise[row][column + 1] * fraction_x
            bottom = noise[row + 1][column] * (cell - fraction_x) + noise[row + 1][column + 1] * fraction_x
            delta = (top * (cell - fraction_y) + bottom * fraction_y) / (cell * cell)
            for channel in range(3):
                value = line[x * 4 + channel] + delta
                line[x * 4 + channel] = 0 if value < 0 else (255 if value > 255 else int(value))


def write_png(path, header, lines):
    def chunk(kind, body):
        return (
            struct.pack(">I", len(body))
            + kind
            + body
            + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF)
        )

    # Filter 0 throughout: PNG filters amplify noise instead of shrinking it.
    packed = bytearray()
    for line in lines:
        packed.append(0)
        packed += line
    open(path, "wb").write(
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(bytes(packed), 9))
        + chunk(b"IEND", b"")
    )


def main():
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    path = sys.argv[1]
    cell = int(sys.argv[2]) if len(sys.argv) > 2 else CELL
    amplitude = float(sys.argv[3]) if len(sys.argv) > 3 else AMPLITUDE
    seed = int(sys.argv[4]) if len(sys.argv) > 4 else SEED
    header, width, _, lines = read_png(path)
    add_grain(width, lines, cell, amplitude, seed)
    write_png(path, header, lines)


if __name__ == "__main__":
    main()
