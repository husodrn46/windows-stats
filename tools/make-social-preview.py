"""Compose a 1280x640 social preview from reviewed, real Windows Stats PNGs.

Usage: python tools/make-social-preview.py docs/images .devtmp/social-preview.png
Requires Pillow. Never captures the desktop or fabricates application pixels.
"""

import argparse
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont, ImageOps


def font(size, bold=False):
    candidates = [
        Path("C:/Windows/Fonts") / ("segoeuib.ttf" if bold else "segoeui.ttf"),
        Path("/usr/share/fonts/truetype/dejavu") / ("DejaVuSans-Bold.ttf" if bold else "DejaVuSans.ttf"),
    ]
    for candidate in candidates:
        if candidate.exists():
            return ImageFont.truetype(str(candidate), size)
    return ImageFont.load_default(size=size)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("images", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    required = ["strip.png", "popup-cpu.png", "popup-net.png", "settings-0.png"]
    missing = [name for name in required if not (args.images / name).is_file()]
    if missing:
        parser.error("Real, reviewed captures required: " + ", ".join(missing))

    preview = Image.new("RGB", (1280, 640), "#11161d")
    draw = ImageDraw.Draw(preview)
    draw.text((48, 26), "Windows Stats", font=font(54, True), fill="#f1f5f9")
    draw.text((50, 98), "System monitoring in your Windows taskbar", font=font(23), fill="#b8c6d5")
    draw.text((50, 134), "Windows 10 / 11  ·  Open source  ·  No telemetry", font=font(18), fill="#73c8a9")

    def place(name, box):
        with Image.open(args.images / name) as image:
            image = ImageOps.contain(image.convert("RGBA"), (box[2] - box[0], box[3] - box[1]))
            x = box[0] + (box[2] - box[0] - image.width) // 2
            y = box[1] + (box[3] - box[1] - image.height) // 2
            preview.paste(image, (x, y), image)

    place("strip.png", (48, 176, 1232, 236))
    for name, title, box in [
        ("popup-cpu.png", "CPU", (48, 278, 368, 566)),
        ("popup-net.png", "Network", (398, 278, 718, 566)),
        ("settings-0.png", "Settings", (748, 278, 1232, 566)),
    ]:
        draw.text((box[0], 249), title, font=font(17, True), fill="#b8c6d5")
        place(name, box)
    draw.text((48, 600), "github.com/husodrn46/windows-stats", font=font(18), fill="#8a9bac")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    preview.save(args.output, format="PNG")
    print(f"Saved {args.output} (1280x640); review before uploading to GitHub.")


if __name__ == "__main__":
    main()
