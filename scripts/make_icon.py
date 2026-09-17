#!/usr/bin/env python3
"""把 assets/icon-src/*.png 的源图加工成 macOS 应用图标。

产出：
  assets/icon.png   1024×1024 带透明圆角（运行时 Dock 图标用，被 include_bytes! 嵌入）
  assets/Mo.icns    完整 iconset（未来打包 Mo.app 用）

用法：python3 scripts/make_icon.py [源图路径]
"""

import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
OUT_PNG = ROOT / "assets" / "icon.png"
OUT_ICNS = ROOT / "assets" / "Mo.icns"

# Apple Big Sur 风格：squircle 圆角比例约 22.37%，图标本体占画布约 90%。
CORNER_RATIO = 0.2237
CONTENT_RATIO = 0.90
CANVAS = 1024


def find_squircle_bbox(img: Image.Image) -> tuple[int, int, int, int]:
    """定位画面中 squircle 的包围盒：优先 alpha，退化为非白像素。"""
    if img.mode != "RGBA":
        img = img.convert("RGBA")
    alpha = img.getchannel("A")
    lo, hi = alpha.getextrema()
    if lo < 250:  # 有真透明背景，直接用 alpha 包围盒。
        return alpha.getbbox()
    # 白底：找显著偏离白色的像素。
    gray = img.convert("L")
    mask = gray.point(lambda v: 255 if v < 245 else 0)
    return mask.getbbox()


def make_square(bbox: tuple[int, int, int, int]) -> tuple[int, int, int, int]:
    """把包围盒扩成正方形（以中心为准）。"""
    l, t, r, b = bbox
    w, h = r - l, b - t
    side = max(w, h)
    cx, cy = (l + r) // 2, (t + b) // 2
    half = side // 2
    return (cx - half, cy - half, cx + half, cy + half)


def main() -> None:
    src = Path(sys.argv[1]) if len(sys.argv) > 1 else sorted((ROOT / "assets/icon-src").glob("*.png"))[-1]
    print(f"源图：{src}")

    img = Image.open(src).convert("RGBA")
    bbox = find_squircle_bbox(img)
    if bbox is None:
        sys.exit("找不到图标主体（alpha 与非白检测都为空）")
    img = img.crop(make_square(bbox)).resize((CANVAS, CANVAS), Image.LANCZOS)

    # 统一按比例重绘：squircle 缩到画布 90%，居中，圆角比例 22.37%。
    content = int(CANVAS * CONTENT_RATIO)
    icon = img.resize((content, content), Image.LANCZOS)
    radius = round(content * CORNER_RATIO)

    mask = Image.new("L", (content, content), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, content - 1, content - 1), radius=radius, fill=255)
    icon.putalpha(mask)

    canvas = Image.new("RGBA", (CANVAS, CANVAS), (0, 0, 0, 0))
    off = (CANVAS - content) // 2
    canvas.paste(icon, (off, off), icon)
    canvas.save(OUT_PNG)
    print(f"已写出 {OUT_PNG}")

    # iconset → icns。
    sizes = [16, 32, 64, 128, 256, 512, 1024]
    with tempfile.TemporaryDirectory() as td:
        iconset = Path(td) / "Mo.iconset"
        iconset.mkdir()
        for s in sizes:
            canvas.resize((s, s), Image.LANCZOS).save(iconset / f"icon_{s}x{s}.png")
            if s < 512:  # @2x 命名只到 512（1024 就是 512@2x 的原始图）。
                canvas.resize((s * 2, s * 2), Image.LANCZOS).save(iconset / f"icon_{s}x{s}@2x.png")
        subprocess.run(
            ["iconutil", "-c", "icns", str(iconset), "-o", str(OUT_ICNS)],
            check=True,
        )
    print(f"已写出 {OUT_ICNS}")


if __name__ == "__main__":
    main()
