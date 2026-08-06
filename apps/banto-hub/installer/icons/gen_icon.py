"""banto-hub インストーラ用プレースホルダアイコン生成スクリプト。

apps/banto-hub/static/favicon.svg (青い角丸背景 #2563eb + 提灯絵文字) を
SVG->ICO 変換できる手元ツールが無いため、同じ配色の簡易プレースホルダを
手続き的に生成する（T5-2 タスク指示: 見た目の作り込みより「インストーラが
ビルドできること」を優先、との判断）。実行後は削除してよい（成果物の
icon.png/icon.ico のみを Cargo 側で参照する）。
"""

from PIL import Image, ImageDraw, ImageFont

BG = (37, 99, 235, 255)  # favicon.svg の #2563eb と同じ
FG = (255, 255, 255, 255)

def make_base(size: int) -> Image.Image:
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)
    radius = max(2, round(size * 6 / 32))
    draw.rounded_rectangle([0, 0, size - 1, size - 1], radius=radius, fill=BG)

    # "B" (BantoHub) を中央に白抜きで描画。
    font = None
    for candidate in (
        "C:/Windows/Fonts/segoeuib.ttf",
        "C:/Windows/Fonts/arialbd.ttf",
        "C:/Windows/Fonts/arial.ttf",
    ):
        try:
            font = ImageFont.truetype(candidate, size=round(size * 0.62))
            break
        except OSError:
            continue
    if font is None:
        font = ImageFont.load_default()

    text = "B"
    bbox = draw.textbbox((0, 0), text, font=font)
    tw, th = bbox[2] - bbox[0], bbox[3] - bbox[1]
    pos = ((size - tw) / 2 - bbox[0], (size - th) / 2 - bbox[1])
    draw.text(pos, text, font=font, fill=FG)
    return img


base = make_base(256)
base.save("icon.png")

# Pillow's ICO writer resizes a single source image down to each requested
# size itself (it ignores `append_images` for this format) - render each
# size explicitly instead so small glyphs stay crisp rather than being
# downscaled from the 256px source.
sizes = [16, 24, 32, 48, 64, 128, 256]
imgs = {s: make_base(s) for s in sizes}
imgs[256].save(
    "icon.ico",
    format="ICO",
    sizes=[(s, s) for s in sizes],
    bitmap_format="bmp",
)

print("wrote icon.png + icon.ico")
