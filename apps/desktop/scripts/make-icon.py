"""Draw the 1024x1024 source icon (a cozy dog face on a warm tile) for `tauri icon`.

Usage: python scripts/make-icon.py  ->  src-tauri/icons/source.png
"""
from pathlib import Path
from PIL import Image, ImageDraw, ImageFilter

S = 1024
OUT = Path(__file__).resolve().parent.parent / "src-tauri" / "icons" / "source.png"


def main() -> None:
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))

    # soft drop shadow + warm tile
    shadow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle((70, 90, S - 70, S - 50), radius=230, fill=(90, 50, 20, 120))
    img.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(28)))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle((64, 64, S - 64, S - 64), radius=230, fill=(244, 162, 97, 255))
    d.rounded_rectangle((64, 64, S - 64, S - 64), radius=230, outline=(222, 128, 68, 255), width=14)

    fur = (255, 246, 232, 255)
    ear = (122, 74, 46, 255)
    ink = (52, 34, 24, 255)

    # floppy ears
    d.ellipse((190, 270, 390, 640), fill=ear)
    d.ellipse((634, 270, 834, 640), fill=ear)
    # head + muzzle
    d.ellipse((262, 240, 762, 760), fill=fur)
    d.ellipse((372, 520, 652, 760), fill=(255, 236, 214, 255))
    # brown eye patch
    d.ellipse((560, 340, 720, 520), fill=(214, 170, 130, 255))
    # eyes
    d.ellipse((392, 420, 456, 490), fill=ink)
    d.ellipse((580, 420, 644, 490), fill=ink)
    d.ellipse((410, 432, 430, 452), fill=(255, 255, 255, 255))
    d.ellipse((598, 432, 618, 452), fill=(255, 255, 255, 255))
    # nose + mouth
    d.rounded_rectangle((462, 548, 562, 612), radius=32, fill=ink)
    d.line((512, 612, 512, 660), fill=ink, width=12)
    d.arc((448, 610, 516, 690), start=20, end=160, fill=ink, width=12)
    d.arc((508, 610, 576, 690), start=20, end=160, fill=ink, width=12)
    # tongue
    d.rounded_rectangle((490, 668, 534, 720), radius=20, fill=(240, 110, 120, 255))
    # cheeks
    d.ellipse((350, 560, 410, 600), fill=(250, 180, 170, 200))
    d.ellipse((614, 560, 674, 600), fill=(250, 180, 170, 200))
    # collar tag
    d.ellipse((470, 770, 554, 854), fill=(255, 205, 80, 255), outline=(200, 140, 40, 255), width=8)

    OUT.parent.mkdir(parents=True, exist_ok=True)
    img.save(OUT)
    print(f"wrote {OUT}")


if __name__ == "__main__":
    main()
