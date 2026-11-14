#!/usr/bin/env python3
"""Convert an XWD capture to PNG, so M4.1's pixel sign-off can be done.

The M4.1 blocker was never the comparison itself -- it was the file format. The
reference screenshots are PNG, `xwd` is the only capture tool on this machine,
and there is no ImageMagick to bridge them. So a human was asked to compare a
PNG against an XWD they could only open with `xwud`, one at a time, at a
different resolution. That is why the item sat at 未做.

XWD (X Window Dump) is a header, an optional colour table, then raw pixels. The
header is 25 big-endian 32-bit words; the colour table is `ncolors` entries of
16 bytes; the pixels are `bytes_per_line` per row. Everything needed to decode
is in the header, which is the point -- no guessing, no library.

Usage:
    xwd2png.py input.xwd [output.png] [--scale WxH]

Exits non-zero on a file that is not an XWD, rather than writing a garbage PNG.
"""

import struct
import sys

# The 25 header words, in file order. Named because the offsets are the contract:
# `pixmap_width`/`pixmap_height` size the image, `bits_per_pixel` and the three
# masks say how a pixel's bits become a colour, `ncolors` says how many colour
# entries follow the header, and `header_size` says where the pixels start.
HEADER_WORDS = (
    "header_size", "file_version", "pixmap_format", "pixmap_depth", "pixmap_width",
    "pixmap_height", "xoffset", "byte_order", "bitmap_unit", "bitmap_bit_order",
    "bitmap_pad", "bits_per_pixel", "bytes_per_line", "visual_class", "red_mask",
    "green_mask", "blue_mask", "bits_per_rgb", "colormap_entries", "ncolors",
    "window_width", "window_height", "window_x", "window_y", "window_bdrwidth",
)
HEADER_BYTES = len(HEADER_WORDS) * 4
# An XWDColor is one 32-bit pixel plus three 16-bit channels plus two flag
# bytes: 12, not 16. Getting this wrong shears the image rather than erroring,
# because the pixels simply start at the wrong offset -- so it is pinned here
# with the arithmetic that proves it: a 1280x800 capture at 5120 bytes per line
# is 4096000 bytes of pixels, and the 3072 bytes between the header and the
# pixels over 256 colour entries is 12 each.
XWD_COLOR_BYTES = 12


def parse_header(blob):
    if len(blob) < HEADER_BYTES:
        raise ValueError("file is too short to hold an XWD header")
    values = struct.unpack(">25I", blob[:HEADER_BYTES])
    header = dict(zip(HEADER_WORDS, values))
    if header["header_size"] < HEADER_BYTES:
        raise ValueError(
            f"header_size {header['header_size']} is smaller than the {HEADER_BYTES} bytes read"
        )
    if header["pixmap_width"] == 0 or header["pixmap_height"] == 0:
        raise ValueError("a zero-sized image is not a capture")
    if header["bits_per_pixel"] % 8 != 0:
        raise ValueError(f"{header['bits_per_pixel']} bits per pixel is not byte-aligned")
    return header


def mask_shift(mask):
    """Where a colour's bits sit, and how wide the field is.

    A mask of 0 means the channel is absent -- a capture from a display with no
    blue, say. Reported as (0, 0) and the channel is left at zero rather than
    looping forever on a mask that is already 0.
    """
    if mask == 0:
        return 0, 0
    shift = 0
    while mask & 1 == 0:
        mask >>= 1
        shift += 1
    return shift, mask.bit_count()


def convert(source, target, scale=None):
    from PIL import Image

    with open(source, "rb") as handle:
        blob = handle.read()

    header = parse_header(blob)
    width, height = header["pixmap_width"], header["pixmap_height"]
    stride = header["bytes_per_line"]
    bytes_per_pixel = header["bits_per_pixel"] // 8

    # The colour table sits between the header and the pixels. Its size comes
    # from `ncolors`, not from `colormap_entries`: a TrueColor display still
    # writes its (unused) colormap, and skipping the wrong count lands the
    # pixels at the wrong offset, which shows up as a sheared image rather than
    # an error.
    pixels_at = header["header_size"] + header["ncolors"] * XWD_COLOR_BYTES
    needed = pixels_at + stride * height
    if len(blob) < needed:
        raise ValueError(
            f"file holds {len(blob)} bytes but {needed} are needed for {width}x{height}"
        )

    channels = [mask_shift(header[name]) for name in ("red_mask", "green_mask", "blue_mask")]
    image = Image.new("RGB", (width, height))
    output = image.load()
    for y in range(height):
        base = pixels_at + y * stride
        for x in range(width):
            start = base + x * bytes_per_pixel
            pixel = int.from_bytes(blob[start:start + bytes_per_pixel], "big")
            colour = []
            for shift, width_bits in channels:
                if width_bits == 0:
                    colour.append(0)
                    continue
                # A field narrower than 8 bits is scaled up, not shifted: a
                # 5-bit blue of 31 is full blue, not a near-black.
                value = (pixel >> shift) & ((1 << width_bits) - 1)
                colour.append(value * 255 // ((1 << width_bits) - 1))
            output[x, y] = tuple(colour)

    if scale is not None:
        image = image.resize(scale, Image.LANCZOS)
    image.save(target)
    return width, height, image.size


def main(argv):
    args = [a for a in argv[1:] if not a.startswith("--")]
    scale = None
    for index, arg in enumerate(argv):
        if arg == "--scale" and index + 1 < len(argv):
            want_w, _, want_h = argv[index + 1].partition("x")
            scale = (int(want_w), int(want_h))
    if not args:
        print(__doc__)
        return 2
    source = args[0]
    target = args[1] if len(args) > 1 else source.rsplit(".", 1)[0] + ".png"
    try:
        width, height, written = convert(source, target, scale)
    except (OSError, ValueError) as error:
        print(f"xwd2png: {source}: {error}", file=sys.stderr)
        return 1
    sized = f", scaled to {written[0]}x{written[1]}" if written != (width, height) else ""
    print(f"{source}: {width}x{height}{sized} -> {target}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
