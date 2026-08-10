"""Generate RusticDL brand icons from a drawn master (no external source).

Produces:
  - assets/brand/logo.png, icon-*.png, icon.ico
  - assets/icon.png
  - apps/extension/src/icons/icon-*.png

Design: full-bleed square matched to the app's default light appearance
(gpui-component "Default Light"): primary tile #171717 + light glyph #fafafa.
No rounded mask / no transparent corners (avoids white corner artifacts).
"""
from __future__ import annotations

from pathlib import Path

from PIL import Image, ImageChops, ImageDraw

ROOT = Path(__file__).resolve().parents[1]

# Match gpui-component default-theme.json → "Default Light"
# primary.background / primary.foreground (AccentPreset::Default keeps these).
BG = (0x17, 0x17, 0x17, 255)  # #171717 — stock primary
GLYPH = (0xFA, 0xFA, 0xFA, 255)  # #fafafa — primary foreground

MASTER = 1024


def draw_master(size: int = MASTER) -> Image.Image:
    """Draw the brand mark at `size`×`size` as a full-bleed square.

    Design grid is 128 units; everything scales from that.
    Corners are solid primary (never white, never transparent).
    """
    s = size
    # Full-bleed primary — no rounded cutout, no alpha holes at corners
    img = Image.new("RGBA", (s, s), BG)
    draw = ImageDraw.Draw(img)

    def u(v: float) -> float:
        """Map design units (0–128) to pixel coordinates."""
        return v * s / 128.0

    cx = u(64)

    # ---- Arrow shaft (rounded capsule) ----
    shaft_w = u(15)
    shaft_top = u(24)
    shaft_bot = u(66)
    draw.rounded_rectangle(
        (cx - shaft_w / 2, shaft_top, cx + shaft_w / 2, shaft_bot),
        radius=max(1, int(u(7.5))),
        fill=GLYPH,
    )

    # ---- Arrow head (solid triangle — reads cleanly at 16px) ----
    tip_y = u(86)
    head_top = u(52)
    wing = u(30)
    draw.polygon(
        [
            (cx, tip_y),
            (cx - wing, head_top),
            (cx + wing, head_top),
        ],
        fill=GLYPH,
    )
    # Blend shaft into head
    draw.rectangle(
        (cx - shaft_w / 2, head_top, cx + shaft_w / 2, u(70)),
        fill=GLYPH,
    )

    # ---- Open tray: thick U via outer−inner mask ----
    stroke = max(2, int(round(u(10))))
    outer_l, outer_r = u(26), u(102)
    outer_t, outer_b = u(92), u(114)
    outer_rx = max(1, int(round(u(12))))

    outer_m = Image.new("L", (s, s), 0)
    om = ImageDraw.Draw(outer_m)
    om.rounded_rectangle((outer_l, outer_t, outer_r, outer_b), radius=outer_rx, fill=255)

    inner_l = outer_l + stroke
    inner_r = outer_r - stroke
    inner_b = outer_b - stroke
    inner_rx = max(1, int(round(u(6))))
    inner_m = Image.new("L", (s, s), 0)
    imd = ImageDraw.Draw(inner_m)
    imd.rounded_rectangle((inner_l, outer_t, inner_r, inner_b), radius=inner_rx, fill=255)
    # Open the top fully so it's a U not an O
    imd.rectangle((inner_l, 0, inner_r, outer_t + stroke), fill=255)

    tray_mask = ImageChops.subtract(outer_m, inner_m)
    glyph_layer = Image.new("RGBA", (s, s), GLYPH)
    img.paste(glyph_layer, (0, 0), mask=tray_mask)

    return img


def _sized(master: Image.Image, s: int) -> Image.Image:
    """Prefer supersampled redraw for small sizes; lanczos from master for large."""
    if s <= 48:
        return draw_master(s * 4).resize((s, s), Image.Resampling.LANCZOS)
    return master.resize((s, s), Image.Resampling.LANCZOS)


def main() -> None:
    brand = ROOT / "assets" / "brand"
    brand.mkdir(parents=True, exist_ok=True)

    master = draw_master(MASTER)
    master.save(brand / "logo.png")
    master.save(brand / "icon-1024.png")
    print("wrote", brand / "logo.png")
    print("wrote", brand / "icon-1024.png")

    for s in [16, 20, 24, 32, 40, 48, 64, 96, 128, 256, 512]:
        out = brand / f"icon-{s}.png"
        _sized(master, s).save(out)
        print("wrote", out)

    # Largest-first helps Windows shell pick a sharp default bitmap.
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

    master.resize((256, 256), Image.Resampling.LANCZOS).save(ROOT / "assets" / "icon.png")
    print("wrote", ROOT / "assets" / "icon.png")
    print("done")


if __name__ == "__main__":
    main()
