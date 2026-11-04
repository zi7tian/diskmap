"""生成应用图标（开发时运行一次即可，生成的文件已提交，打包时不需要 Pillow）。

    python packaging/make_icons.py
"""
import os

from PIL import Image, ImageDraw

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
S = 4  # 超采样倍数，缩小后得到平滑边缘


def box(d, x0, y0, x1, y1, fill, r=34):
    d.rounded_rectangle([x0 * S, y0 * S, x1 * S, y1 * S], radius=r * S, fill=fill)


def draw():
    img = Image.new("RGBA", (1024 * S, 1024 * S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    # 底板
    box(d, 92, 92, 932, 932, "#2A313B", r=190)
    box(d, 100, 100, 924, 924, "#151A21", r=184)
    # 方框套方框：左侧大块（系统文件），里面再套一层
    box(d, 164, 164, 500, 860, "#F08C2E")
    box(d, 196, 520, 468, 828, "#B9641A", r=22)
    box(d, 216, 540, 448, 680, "#F6A85C", r=16)
    box(d, 216, 700, 448, 808, "#F6A85C", r=16)
    # 右侧：用户数据 / 正在使用 / 缓存 / 系统核心
    box(d, 524, 164, 860, 500, "#4C8DFF")
    box(d, 524, 524, 700, 860, "#FF5A5F")
    box(d, 724, 524, 860, 700, "#3FBF6E")
    box(d, 724, 724, 860, 860, "#A66BFF")
    return img.resize((1024, 1024), Image.LANCZOS)


def main():
    out = os.path.join(HERE, "icons")
    os.makedirs(out, exist_ok=True)
    img = draw()
    img.save(os.path.join(out, "diskmap.png"))
    img.resize((512, 512), Image.LANCZOS).save(os.path.join(out, "diskmap-512.png"))
    img.save(os.path.join(out, "diskmap.ico"),
             sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    img.save(os.path.join(out, "diskmap.icns"))
    img.resize((64, 64), Image.LANCZOS).save(os.path.join(ROOT, "web", "favicon.png"))
    print("图标已生成：", out)


if __name__ == "__main__":
    main()
