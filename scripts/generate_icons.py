#!/usr/bin/env python3
"""Generate all LanDesk icons from the existing tray mark (requires Pillow)."""
from pathlib import Path
from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent / "assets/icons"
BLUE = (38, 114, 237, 255)
WHITE = (255, 255, 255, 255)
LETTER = ((8, 7, 12, 25), (8, 21, 25, 25))


def mark(size):
    image = Image.new("RGBA", (size, size), BLUE)
    draw = ImageDraw.Draw(image)
    for x1, y1, x2, y2 in LETTER:
        draw.rectangle((x1 * size // 32, y1 * size // 32,
                        x2 * size // 32 - 1, y2 * size // 32 - 1), fill=WHITE)
    return image


def main():
    ROOT.mkdir(parents=True, exist_ok=True)
    # Keep the 32 px tray pixels unchanged; all larger sizes use the same mark.
    (ROOT / "tray.rgba").write_bytes(mark(32).tobytes())
    mark(256).save(ROOT / "LanDesk.ico", sizes=[(n, n) for n in (16, 20, 24, 32, 40, 48, 64, 128, 256)])
    rectangles = ''.join(f'<rect x="{x1}" y="{y1}" width="{x2-x1}" height="{y2-y1}" fill="white"/>' for x1, y1, x2, y2 in LETTER)
    (ROOT / "LanDesk.svg").write_text(
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">'
        '<rect width="32" height="32" fill="#2672ed"/>' + rectangles + '</svg>\n', encoding='utf-8')
    # macOS uses the same mark inside a padded rounded application tile.
    tile = mark(832)
    mask = Image.new("L", tile.size, 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, 831, 831), radius=180, fill=255)
    tile.putalpha(mask)
    mac = Image.new("RGBA", (1024, 1024))
    mac.alpha_composite(tile, (96, 96))
    mac.save(ROOT / "LanDesk.png")
    mac.save(ROOT / "LanDesk.icns")
    print(ROOT)


if __name__ == '__main__':
    main()
