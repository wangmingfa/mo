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
from collections import deque
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parent.parent
OUT_PNG = ROOT / "assets" / "icon.png"
OUT_ICNS = ROOT / "assets" / "Mo.icns"

# Apple Big Sur 风格：squircle 圆角比例约 22.37%。
# 内容占比 0.824 = 官方网格 824/1024——占比过大会在 Dock 里
# 显得比其它应用图标大一圈。
CORNER_RATIO = 0.2237
CONTENT_RATIO = 0.824
CANVAS = 1024

# 近白判定：三通道都够亮且彼此接近（灰白，而不是彩色高光）。
NEAR_WHITE = 240


def strip_border_white(img: Image.Image) -> Image.Image:
    """把与画布边缘连通的近白色背景抠成透明。

    只清「连通到边界」的近白区域——图标内部的白色图形（如文件夹面）
    与边界不连通，不受影响。返回的图 alpha 已反走样。
    """
    img = img.convert("RGBA")
    w, h = img.size
    px = img.load()

    def near_white(x: int, y: int) -> bool:
        r, g, b, _ = px[x, y]
        return r > NEAR_WHITE and g > NEAR_WHITE and b > NEAR_WHITE

    bg = bytearray(w * h)  # 1 = 背景
    queue: deque[tuple[int, int]] = deque()
    for x in range(w):
        for y in (0, h - 1):
            if near_white(x, y) and not bg[y * w + x]:
                bg[y * w + x] = 1
                queue.append((x, y))
    for y in range(h):
        for x in (0, w - 1):
            if near_white(x, y) and not bg[y * w + x]:
                bg[y * w + x] = 1
                queue.append((x, y))
    while queue:
        x, y = queue.popleft()
        for nx, ny in ((x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)):
            if 0 <= nx < w and 0 <= ny < h and not bg[ny * w + nx] and near_white(nx, ny):
                bg[ny * w + nx] = 1
                queue.append((nx, ny))

    mask = Image.frombytes("L", (w, h), bytes(0 if v else 255 for v in bg))
    # 轻微收缩 + 模糊，去掉白边并反走样。
    mask = mask.filter(ImageFilter.MinFilter(3)).filter(ImageFilter.GaussianBlur(1.2))
    img.putalpha(mask)
    return img


def find_squircle_bbox(img: Image.Image) -> tuple[int, int, int, int] | None:
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
    img = strip_border_white(img)  # 源图常是白底而非真透明，先抠掉连通背景。
    bbox = find_squircle_bbox(img)
    if bbox is None:
        sys.exit("找不到图标主体（alpha 与非白检测都为空）")
    img = img.crop(make_square(bbox)).resize((CANVAS, CANVAS), Image.LANCZOS)

    # 统一按比例重绘：squircle 缩到画布 90%，居中，圆角比例 22.37%。
    # 蒙版只作安全兜底（图案自身圆角为准），4x 超采样再缩小以反走样。
    content = int(CANVAS * CONTENT_RATIO)
    icon = img.resize((content, content), Image.LANCZOS)
    radius = round(content * CORNER_RATIO)

    ss = 4
    mask = Image.new("L", (content * ss, content * ss), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        (0, 0, content * ss - 1, content * ss - 1), radius=radius * ss, fill=255
    )
    mask = mask.resize((content, content), Image.LANCZOS)
    icon.putalpha(Image.composite(icon.getchannel("A"), Image.new("L", (content, content), 0), mask))

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
