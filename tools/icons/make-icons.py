#!/usr/bin/env python3
"""Regenerates every app icon and logo from the master artwork.

    python tools/icons/make-icons.py [MASTER]

MASTER defaults to apps/desktop-mobile/src-tauri/icons/source.png: a square PNG of at least
1024 px with a transparent background and the artwork already centred with a small margin.

`pnpm tauri icon` makes the desktop and Android sets, but three of its outputs are unusable as
they come, so this script runs it and then replaces them:

* Android adaptive foregrounds. Tauri scales the artwork to the full 108 dp layer, but launchers
  mask the layer to a shape inside the central 66 dp, which cuts the artwork off. The script
  redraws them with the artwork inset into that safe zone.
* iOS. Tauri writes per-size RGBA images for the light icon only. App Store validation rejects a
  marketing icon with an alpha channel, and iOS 18 shows a dark and a tinted home-screen icon.
  The script writes a single-size asset catalog instead (`icons/ios/`): an opaque light icon, a
  dark icon on a transparent background (the system draws the dark backdrop), an opaque
  grayscale tinted icon, and the `Contents.json` that names them. Xcode derives every other size.
  `gen/apple` isn't checked in, so this folder is copied into it after `tauri ios init`
  (docs/platforms/ipad.md).
* Tauri's own `icons/android/` copy, which nothing reads (the build uses `gen/android`). The
  script removes it so a stale logo can't linger there.

It also writes the in-app logo and favicon (`src/assets/logo.png`) and the README logo
(`docs/images/logo.png`). Needs Pillow and the app's Node dependencies.
"""
import json, pathlib, shutil, subprocess, sys

from PIL import Image, ImageOps

ROOT = pathlib.Path(__file__).resolve().parents[2]
APP = ROOT / "apps/desktop-mobile"
ICONS = APP / "src-tauri/icons"
ANDROID_RES = APP / "src-tauri/gen/android/app/src/main/res"
# Adaptive-icon layer size in px per density (108 dp each).
ANDROID_LAYER = {"mdpi": 108, "hdpi": 162, "xhdpi": 216, "xxhdpi": 324, "xxxhdpi": 432}
# Share of the 108 dp layer the master (with its own margin) may fill. Launcher masks can be as
# small as the 66 dp safe-zone circle, and the artwork runs corner to corner, so the master's
# square has to fit inside that circle: 66 / 108 / sqrt(2), with a little of the margin reclaimed.
ANDROID_SCALE = 0.5
# Share of the iOS canvas the master fills. iOS clips to a rounded square, so the artwork needs
# more breathing room than the master's own margin.
IOS_SCALE = 0.86
IOS_SIZE = 1024
LIGHT_BACKGROUND = (255, 255, 255)


def placed(master, canvas, scale, background=None):
    """The master scaled to `scale` of a square `canvas` and centred, on `background` (RGB) or
    on transparency when that is None."""
    side = round(canvas * scale)
    art = master.resize((side, side), Image.LANCZOS)
    out = Image.new("RGBA", (canvas, canvas), (*(background or (0, 0, 0)), 255 if background else 0))
    offset = (canvas - side) // 2
    out.alpha_composite(art, (offset, offset))
    return out.convert("RGB") if background else out


def tinted(master):
    """iOS tinted icon: the artwork's luminance stretched to full range, opaque on black. The
    system multiplies it by the user's tint, so white is full tint and black is none. The
    mid-tones are lifted because the blue pen body is dark in luminance and would barely show."""
    art = placed(master, IOS_SIZE, IOS_SCALE)
    gray = ImageOps.autocontrast(art.convert("L"), cutoff=1).point(lambda v: round(255 * (v / 255) ** 0.6))
    out = Image.new("L", art.size, 0)
    out.paste(gray, mask=art.getchannel("A"))
    return out.convert("RGB")


def ios_catalog(master):
    target = ICONS / "ios"
    shutil.rmtree(target, ignore_errors=True)
    target.mkdir()
    placed(master, IOS_SIZE, IOS_SCALE, LIGHT_BACKGROUND).save(target / "AppIcon-1024.png", optimize=True)
    placed(master, IOS_SIZE, IOS_SCALE).save(target / "AppIcon-1024-dark.png", optimize=True)
    tinted(master).save(target / "AppIcon-1024-tinted.png", optimize=True)

    def entry(filename, appearance=None):
        e = {"filename": filename, "idiom": "universal", "platform": "ios", "size": "1024x1024"}
        if appearance:
            e["appearances"] = [{"appearance": "luminosity", "value": appearance}]
        return e

    contents = {
        "images": [
            entry("AppIcon-1024.png"),
            entry("AppIcon-1024-dark.png", "dark"),
            entry("AppIcon-1024-tinted.png", "tinted"),
        ],
        "info": {"author": "xcode", "version": 1},
    }
    (target / "Contents.json").write_text(json.dumps(contents, indent=2) + "\n", encoding="utf-8")


def android_foregrounds(master):
    for density, size in ANDROID_LAYER.items():
        path = ANDROID_RES / f"mipmap-{density}/ic_launcher_foreground.png"
        placed(master, size, ANDROID_SCALE).save(path, optimize=True)


def main():
    source = pathlib.Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else ICONS / "source.png"
    master = Image.open(source).convert("RGBA")
    if master.width != master.height or master.width < 1024:
        sys.exit(f"{source}: the master must be square and at least 1024 px")

    # Tauri rewrites the Android XML resources too. Keep their bytes so an unchanged file doesn't
    # show up as a line-ending-only diff.
    xml = {p: p.read_bytes() for p in ANDROID_RES.glob("*/ic_launcher*.xml")}
    subprocess.run(["pnpm", "tauri", "icon", str(source), "-o", "src-tauri/icons"],
                   cwd=APP, check=True, shell=sys.platform == "win32")
    for path, data in xml.items():
        path.write_bytes(data)

    shutil.rmtree(ICONS / "android", ignore_errors=True)
    android_foregrounds(master)
    ios_catalog(master)
    master.resize((512, 512), Image.LANCZOS).save(APP / "src/assets/logo.png", optimize=True)
    master.resize((128, 128), Image.LANCZOS).save(ROOT / "docs/images/logo.png", optimize=True)


if __name__ == "__main__":
    main()
