"""Generate RusticDL brand icon sizes from the master logo."""
from pathlib import Path

from PIL import Image

SRC = Path(
    r"C:\Users\ZeusVeilmon\.grok\sessions\C%3A%5CUsers%5CZeusVeilmon%5CDesktop%5CProject%5CProgram%5CRusticDL\019feadd-364a-7d43-9fb9-7bfca2f5bf4b\images\1.jpg"
)
ROOT = Path(__file__).resolve().parents[1]


def main() -> None:
    img = Image.open(SRC).convert("RGBA")
    brand = ROOT / "assets" / "brand"
    brand.mkdir(parents=True, exist_ok=True)

    master = img.resize((1024, 1024), Image.Resampling.LANCZOS)
    master.save(brand / "logo.png")
    master.save(brand / "icon-1024.png")

    for s in [16, 20, 24, 32, 40, 48, 64, 96, 128, 256, 512]:
        out = brand / f"icon-{s}.png"
        master.resize((s, s), Image.Resampling.LANCZOS).save(out)
        print("wrote", out)

    ico_sizes = [16, 24, 32, 48, 64, 128, 256]
    frames = [master.resize((s, s), Image.Resampling.LANCZOS) for s in ico_sizes]
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
        master.resize((s, s), Image.Resampling.LANCZOS).save(p)
        print("wrote", p)

    master.resize((256, 256), Image.Resampling.LANCZOS).save(ROOT / "assets" / "icon.png")
    print("done")


if __name__ == "__main__":
    main()
