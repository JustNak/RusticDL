"""Generate RusticDL brand icons from an Imagine master (or redraw fallback).

Produces:
  - assets/brand/logo.png (dark theme), logo-light.png (light theme)
  - assets/brand/icon-*.png, icon.ico
  - assets/icon.png
  - apps/extension/src/icons/icon-*.png

Pipeline:
  1. Load a square master image (prefer Imagine output).
  2. Quantize to Default Light primary palette (#171717 / #fafafa).
  3. Full-bleed slate corners (no white / no alpha holes).
  4. Export PNG sizes + multi-size ICO.
  5. Invert the mark for a light-theme title-bar variant.
"""
from __future__ import annotations

import sys
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parents[1]

# gpui-component Default Light primary tokens (dark-theme mark: light glyph on dark field)
BG = (0x17, 0x17, 0x17, 255)  # #171717
GLYPH = (0xFA, 0xFA, 0xFA, 255)  # #fafafa

# Light-theme mark: inverted field / glyph
LIGHT_BG = GLYPH
LIGHT_GLYPH = BG

MASTER = 1024

# Checked-in processed master (from Imagine). Override: python scripts/gen_icons.py path.jpg
DEFAULT_SRC = ROOT / "assets" / "brand" / "masters" / "icon-master-1024.png"


def to_brand_palette(img: Image.Image, *, already_brand: bool = False) -> Image.Image:
    """Force full-bleed 2-tone brand colors from a generated master."""
    rgba = img.convert("RGBA")
    if rgba.size != (MASTER, MASTER):
        rgba = rgba.resize((MASTER, MASTER), Image.Resampling.LANCZOS)

    if already_brand:
        # Ensure opaque full-bleed; keep existing two-tone (plus AA greys on resize).
        out = Image.new("RGBA", (MASTER, MASTER), BG)
        out.paste(rgba, (0, 0))
        return out

    rgb = rgba.convert("RGB")
    # Luminance threshold: dark → BG, light → GLYPH
    # JPEG noise sits near (27,27,27) / (245,242,233); mid-gray rare in flat icons.
    out = Image.new("RGBA", (MASTER, MASTER), BG)
    px_in = rgb.load()
    px_out = out.load()
    for y in range(MASTER):
        for x in range(MASTER):
            r, g, b = px_in[x, y]
            yv = 0.2126 * r + 0.7152 * g + 0.0722 * b
            px_out[x, y] = GLYPH if yv > 90 else BG
    return out


def load_master(src: Path | None) -> Image.Image:
    path = src or DEFAULT_SRC
    if not path.is_file():
        raise FileNotFoundError(
            f"Master image not found: {path}\n"
            "Pass a path: python scripts/gen_icons.py path/to/imagine.jpg"
        )
    print("source", path)
    # Already-quantized repo master only needs resize consistency.
    already = path.resolve() == DEFAULT_SRC.resolve() and path.suffix.lower() == ".png"
    return to_brand_palette(Image.open(path), already_brand=already)


def _sized(master: Image.Image, s: int) -> Image.Image:
    return master.resize((s, s), Image.Resampling.LANCZOS)


def invert_mark(master: Image.Image) -> Image.Image:
    """Swap field/glyph for light-theme chrome (dark glyph on light field)."""
    rgba = master.convert("RGBA")
    out = Image.new("RGBA", rgba.size, LIGHT_BG)
    px_in = rgba.load()
    px_out = out.load()
    w, h = rgba.size
    # Luminance midpoint between brand BG (#171717 ≈ 23) and GLYPH (#fafafa ≈ 250).
    for y in range(h):
        for x in range(w):
            r, g, b, _a = px_in[x, y]
            yv = 0.2126 * r + 0.7152 * g + 0.0722 * b
            px_out[x, y] = LIGHT_GLYPH if yv > 90 else LIGHT_BG
    return out


def main(argv: list[str] | None = None) -> None:
    args = list(sys.argv[1:] if argv is None else argv)
    src = Path(args[0]) if args else None

    brand = ROOT / "assets" / "brand"
    brand.mkdir(parents=True, exist_ok=True)

    master = load_master(src)
    master.save(brand / "logo.png")
    master.save(brand / "icon-1024.png")
    print("wrote", brand / "logo.png")

    light = invert_mark(master)
    light.save(brand / "logo-light.png")
    print("wrote", brand / "logo-light.png")

    for s in [16, 20, 24, 32, 40, 48, 64, 96, 128, 256, 512]:
        out = brand / f"icon-{s}.png"
        _sized(master, s).save(out)
        print("wrote", out)

    ico_sizes = [256, 128, 64, 48, 32, 24, 16]
    frames = [_sized(master, s) for s in ico_sizes]
    ico_path = brand / "icon.ico"
    frames[0].save(
        ico_path,
        format="ICO",
        sizes=[(s, s) for s in ico_sizes],
        append_images=frames[1:],
    )
    print("wrote", ico_path)

    ext = ROOT / "apps" / "extension" / "src" / "icons"
    ext.mkdir(parents=True, exist_ok=True)
    for s in [16, 32, 48, 128]:
        p = ext / f"icon-{s}.png"
        _sized(master, s).save(p)
        print("wrote", p)

    # Keep a project-local copy of the processed masters for reproducibility
    masters_dir = brand / "masters"
    masters_dir.mkdir(parents=True, exist_ok=True)
    master.save(masters_dir / "icon-master-1024.png")
    print("wrote", masters_dir / "icon-master-1024.png")
    light.save(masters_dir / "icon-master-light-1024.png")
    print("wrote", masters_dir / "icon-master-light-1024.png")

    master.resize((256, 256), Image.Resampling.LANCZOS).save(ROOT / "assets" / "icon.png")
    print("wrote", ROOT / "assets" / "icon.png")
    print("done")


if __name__ == "__main__":
    main()
