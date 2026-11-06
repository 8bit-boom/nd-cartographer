"""Generate the desktop icons (placeholder art: dark field, cyan needle).

Pure stdlib — a minimal PNG writer and a hand-rolled 32bpp ICO. Re-run when
real art lands: `python make_icons.py` inside crates/ndcart-desktop/.
"""
import struct
import sys
import zlib
from pathlib import Path

BG = (0x14, 0x16, 0x1A)
FG = (0x00, 0xB3, 0xE6)
MID = (0x8A, 0x94, 0xA3)


def pixel(x: int, y: int, size: int) -> tuple[int, int, int]:
    """The mark: a cyan needle pointing north-east on a dark field."""
    n = size / 32.0
    x, y = x / n, y / n
    # Border ring.
    if x < 1.5 or y < 1.5 or x > 30.5 or y > 30.5:
        return MID
    # Needle: a thick diagonal from bottom-left to top-right, head at NE.
    d = (30 - x) - y  # 0 on the NE diagonal
    if -2.5 <= d <= 2.5 and 4 <= x <= 28:
        return FG
    if 24 <= x <= 28 and 22 <= y <= 27:  # the head
        return FG
    return BG


def png(size: int) -> bytes:
    rows = bytearray()
    for y in range(size):
        rows.append(0)  # filter: none
        for x in range(size):
            r, g, b = pixel(x, y, size)
            rows += bytes((r, g, b, 0xFF))
    def chunk(kind: bytes, data: bytes) -> bytes:
        c = struct.pack(">I", len(data)) + kind + data
        return c + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)
    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(bytes(rows), 9))
        + chunk(b"IEND", b"")
    )


def ico(size: int) -> bytes:
    # One 32bpp BMP entry, no palette; the AND mask is all-transparent bits.
    xor = bytearray()
    for y in range(size - 1, -1, -1):  # BMP rows are bottom-up
        for x in range(size):
            r, g, b = pixel(x, y, size)
            xor += bytes((b, g, r, 0xFF))
    and_mask = b"\x00" * (((size + 31) // 32) * 4 * size)
    bmp = struct.pack(
        "<IiiHHIIiiII", 40, size, size * 2, 1, 32, 0, len(xor) + len(and_mask), 0, 0, 0, 0
    )
    image = bmp + bytes(xor) + and_mask
    header = struct.pack("<HHH", 0, 1, 1)
    entry = struct.pack(
        "<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(image), 22
    )
    return header + entry + image


def main() -> None:
    out = Path(__file__).parent / "icons"
    out.mkdir(exist_ok=True)
    (out / "icon.ico").write_bytes(ico(32))
    (out / "32x32.png").write_bytes(png(32))
    (out / "128x128.png").write_bytes(png(128))
    (out / "128x128@2x.png").write_bytes(png(256))
    (out / "icon.png").write_bytes(png(256))
    print(f"icons written to {out}")


if __name__ == "__main__":
    sys.exit(main())
