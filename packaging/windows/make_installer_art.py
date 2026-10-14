#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
# Copyright (c) 2026 JustMyth / Tan Ming Jing
"""MSI setup UI branding (WiX WixUI): writes, next to this script,

  installer/dialog.bmp   493x312  Welcome / Finish dialogs: the MythDesk sky panel with the app
                                  icon on the left 164 px (the text area on the right stays white,
                                  WixUI draws its text there)
  installer/banner.bmp   493x58   top banner of the inner dialogs: white (text on the left) with
                                  a sky strip and the icon on the right
  installer/License.rtf           the LICENSE text (+ the NOTICE pointer) as RTF for the license page

Same palette as the app icon (packaging/icons/make_icons.py, ui/design/tokens.css).
Run: python3 packaging/windows/make_installer_art.py   (needs Pillow). Outputs are committed.
"""
import os
from PIL import Image, ImageDraw, ImageFilter

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
OUT = os.path.join(HERE, "installer")
ICON = os.path.join(ROOT, "packaging", "icons", "png", "mythbrowse-256.png")
TOP, BOTTOM = (0x2B, 0x43, 0x86), (0x66, 0x98, 0xD2)


def lerp(a, b, t):
    return tuple(round(a[i] + (b[i] - a[i]) * t) for i in range(3))


def sky(w, h, horizontal=False):
    """The dusk-to-day sky gradient with a few soft cloud puffs."""
    img = Image.new("RGB", (w, h))
    px = img.load()
    for y in range(h):
        for x in range(w):
            t = (x / max(1, w - 1)) if horizontal else (y / max(1, h - 1))
            px[x, y] = lerp(TOP, BOTTOM, t)
    clouds = Image.new("L", (w, h), 0)
    d = ImageDraw.Draw(clouds)
    for cx, cy, r in [(0.22, 0.78, 0.20), (0.48, 0.84, 0.24), (0.80, 0.80, 0.18), (0.62, 0.20, 0.10)]:
        rr = r * min(w, h) if not horizontal else r * h * 2
        d.ellipse([cx * w - rr, cy * h - rr * 0.6, cx * w + rr, cy * h + rr * 0.6], fill=70)
    clouds = clouds.filter(ImageFilter.GaussianBlur(radius=max(2, min(w, h) // 12)))
    white = Image.new("RGB", (w, h), (255, 255, 255))
    return Image.composite(white, img, clouds)


def paste_icon(dst, size, center):
    icon = Image.open(ICON).convert("RGBA").resize((size, size), Image.LANCZOS)
    # A soft shadow so the tile lifts off the sky.
    shadow = Image.new("RGBA", (size + 24, size + 24), (0, 0, 0, 0))
    alpha = icon.split()[3].point(lambda a: a * 0.35)
    shadow.paste((10, 20, 50, 255), (12, 16), alpha)
    shadow = shadow.filter(ImageFilter.GaussianBlur(6))
    x, y = center[0] - size // 2, center[1] - size // 2
    dst.paste(shadow, (x - 12, y - 12), shadow)
    dst.paste(icon, (x, y), icon)


def dialog():
    w, h, panel = 493, 312, 164
    img = Image.new("RGB", (w, h), (255, 255, 255))
    img.paste(sky(panel, h), (0, 0))
    paste_icon(img, 96, (panel // 2, 132))
    d = ImageDraw.Draw(img)
    # A hairline where the panel meets the text area.
    d.line([(panel, 0), (panel, h)], fill=(226, 232, 242))
    return img


def banner():
    w, h = 493, 58
    img = Image.new("RGB", (w, h), (255, 255, 255))
    strip_w = 120
    strip = sky(strip_w, h, horizontal=True)
    # Fade the strip in from white on its left edge.
    mask = Image.new("L", (strip_w, h))
    mp = mask.load()
    for x in range(strip_w):
        for y in range(h):
            mp[x, y] = min(255, int(255 * (x / 40))) if x < 40 else 255
    img.paste(strip, (w - strip_w, 0), mask)
    paste_icon(img, 40, (w - 34, h // 2))
    ImageDraw.Draw(img).line([(0, h - 1), (w, h - 1)], fill=(226, 232, 242))
    return img


def rtf_escape(text):
    out = []
    for ch in text:
        o = ord(ch)
        if ch in "\\{}":
            out.append("\\" + ch)
        elif ch == "\n":
            out.append("\\par\n")
        elif o > 127:
            out.append("\\u%d?" % (o if o < 32768 else o - 65536))
        else:
            out.append(ch)
    return "".join(out)


def license_rtf():
    with open(os.path.join(ROOT, "LICENSE"), encoding="utf-8") as f:
        text = f.read().strip()
    text += (
        "\n\nMythBrowse is built on the Servo web engine and other open-source components "
        "(MPL-2.0, MIT, Apache-2.0 and others). Their licenses and notices are in the NOTICE "
        "file installed next to mythbrowse.exe.\n"
    )
    return (
        "{\\rtf1\\ansi\\ansicpg1252\\deff0{\\fonttbl{\\f0\\fswiss\\fcharset0 Segoe UI;}}\n"
        "\\viewkind4\\uc1\\pard\\sa120\\f0\\fs18\n" + rtf_escape(text) + "\n}\n"
    )


def main():
    os.makedirs(OUT, exist_ok=True)
    dialog().save(os.path.join(OUT, "dialog.bmp"), "BMP")
    banner().save(os.path.join(OUT, "banner.bmp"), "BMP")
    with open(os.path.join(OUT, "License.rtf"), "w", encoding="ascii", newline="\r\n") as f:
        f.write(license_rtf())
    print("wrote", OUT)


if __name__ == "__main__":
    main()
