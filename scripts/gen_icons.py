"""Generate RusticDL brand icons from a drawn master (no external source).

Produces:
  - assets/brand/logo.png, icon-*.png, icon.ico
  - assets/icon.png
  - apps/extension/src/icons/icon-*.png

Mark: minimal download arrow over a simple crab head (Rust-adjacent),
using Default Light primary colors. Full-bleed square (no transparent corners).
"""
from __future__ import annotations

from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[1]

# gpui-component Default Light primary tokens
BG = (0x17, 0x17, 0x17, 255)  # #171717
GLYPH = (0xFA, 0xFA, 0xFA, 255)  # #fafafa

MASTER = 1024


def draw_master(size: int = MASTER) -> Image.Image:
    """Minimal download-on-crab mark on a full-bleed primary tile.

    Design grid is 128 units. Keep shapes chunky so 16–32px still reads.
    """
    s = size
    img = Image.new("RGBA", (s, s), BG)
    draw = ImageDraw.Draw(img)

    def u(v: float) -> float:
        return v * s / 128.0

    g = GLYPH
    cx = u(64)

    # ---- Download arrow (on top) ----
    shaft_w = u(13)
    shaft_top = u(12)
    shaft_bot = u(40)
    draw.rounded_rectangle(
        (cx - shaft_w / 2, shaft_top, cx + shaft_w / 2, shaft_bot),
        radius=max(1, int(u(6.5))),
        fill=g,
    )
    tip_y = u(56)
    head_top = u(32)
    wing = u(24)
    draw.polygon(
        [
            (cx, tip_y),
            (cx - wing, head_top),
            (cx + wing, head_top),
        ],
        fill=g,
    )
    draw.rectangle(
        (cx - shaft_w / 2, head_top, cx + shaft_w / 2, u(44)),
        fill=g,
    )

    # ---- Crab head ----
    # Carapace
    draw.ellipse((u(30), u(64), u(98), u(112)), fill=g)

    # Symmetric claws: solid side ellipses (no cutouts — cleaner at small sizes)
    claw = u(18)
    # Left claw — slightly up and out
    draw.ellipse((u(12), u(74), u(12) + claw, u(74) + claw), fill=g)
    # Right claw
    draw.ellipse((u(116) - claw, u(74), u(116), u(74) + claw), fill=g)

    # Eyes (BG pupils on carapace)
    eye_r = u(6)
    for ex in (u(48), u(80)):
        ey = u(84)
        draw.ellipse((ex - eye_r, ey - eye_r, ex + eye_r, ey + eye_r), fill=BG)

    # Short antenna stubs between arrow tip and carapace (characteristic, light)
    ant_w = max(1, int(round(u(3.5))))
    for ax in (u(50), u(78)):
        draw.rounded_rectangle(
            (ax - ant_w / 2, u(56), ax + ant_w / 2, u(68)),
            radius=max(1, ant_w // 2),
            fill=g,
        )
        tip_r = u(3.5)
        draw.ellipse(
            (ax - tip_r, u(52), ax + tip_r, u(52) + tip_r * 2),
            fill=g,
        )

    return img


def _sized(master: Image.Image, s: int) -> Image.Image:
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

    master.resize((256, 256), Image.Resampling.LANCZOS).save(ROOT / "assets" / "icon.png")
    print("wrote", ROOT / "assets" / "icon.png")
    print("done")


if __name__ == "__main__":
    main()
