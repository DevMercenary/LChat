#!/usr/bin/env python3
"""Генератор иконки LChat: PNG нужных размеров (тема hicolor) + .ico для Windows.

Рисуем нативно в Pillow со сглаживанием (супер-сэмплинг x4). Дизайн совпадает с
packaging/io.github.DevMercenary.LChat.svg: две перекрывающиеся «пузыри» чата
(зелёный «собеседник» + белый «я» с тремя точками) на сине-фиолетовом градиенте.
"""
from PIL import Image, ImageDraw

APPID = "io.github.DevMercenary.LChat"
HERE = __import__("pathlib").Path(__file__).resolve().parent
PNG_SIZES = [512, 256, 128, 64, 48, 32, 16]
ICO_SIZES = [256, 128, 64, 48, 32, 24, 16]
SS = 4  # супер-сэмплинг

# --- палитра ---
BG_TOP = (61, 123, 232)      # #3D7BE8
BG_BOTTOM = (106, 69, 217)   # #6A45D9
GREEN = (47, 203, 110)       # #2FCB6E
WHITE = (255, 255, 255)
DOT = (106, 69, 217)         # #6A45D9

# Геометрия в базовых координатах 512x512 (та же, что в SVG).
BASE = 512
BG_RADIUS = 114
# зелёный пузырь (собеседник) + хвост снизу-слева
GREEN_RECT = (96, 128, 300, 286)
GREEN_R = 44
GREEN_TAIL = [(140, 278), (140, 340), (198, 280)]
# белый пузырь (я) + хвост снизу-справа
WHITE_RECT = (212, 214, 420, 372)
WHITE_R = 44
WHITE_TAIL = [(374, 364), (374, 426), (318, 366)]
DOTS = [(278, 293), (316, 293), (354, 293)]
DOT_R = 15


def lerp(a, b, t):
    return tuple(round(a[i] + (b[i] - a[i]) * t) for i in range(3))


def s(v):
    return round(v * SS)


def sr(rect):
    return [s(rect[0]), s(rect[1]), s(rect[2]), s(rect[3])]


def render(px):
    """Рисует иконку размером px x px (через супер-сэмплинг)."""
    W = px * SS
    img = Image.new("RGBA", (W, W), (0, 0, 0, 0))

    # 1) вертикальный градиент, обрезанный по скруглённому квадрату
    grad = Image.new("RGB", (W, W))
    gd = ImageDraw.Draw(grad)
    for y in range(W):
        gd.line([(0, y), (W, y)], fill=lerp(BG_TOP, BG_BOTTOM, y / (W - 1)))
    mask = Image.new("L", (W, W), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        [0, 0, W - 1, W - 1], radius=s(BG_RADIUS * px / BASE), fill=255
    )
    # масштаб координат: базовые 512 -> px
    k = px / BASE
    img.paste(grad, (0, 0), mask)

    d = ImageDraw.Draw(img)

    def rect(r):
        return [s(r[0] * k), s(r[1] * k), s(r[2] * k), s(r[3] * k)]

    def poly(pts):
        return [(s(x * k), s(y * k)) for (x, y) in pts]

    # 2) зелёный пузырь (хвост + скруглённый прямоугольник)
    d.polygon(poly(GREEN_TAIL), fill=GREEN)
    d.rounded_rectangle(rect(GREEN_RECT), radius=s(GREEN_R * k), fill=GREEN)

    # 3) белый пузырь поверх (перекрывает зелёный — создаёт глубину)
    d.polygon(poly(WHITE_TAIL), fill=WHITE)
    d.rounded_rectangle(rect(WHITE_RECT), radius=s(WHITE_R * k), fill=WHITE)

    # 4) три точки «набор сообщения»
    rr = s(DOT_R * k)
    for (cx, cy) in DOTS:
        x, y = s(cx * k), s(cy * k)
        d.ellipse([x - rr, y - rr, x + rr, y + rr], fill=DOT)

    return img.resize((px, px), Image.LANCZOS)


def main():
    icons_dir = HERE / "icons" / "hicolor"
    # PNG по размерам в тему hicolor
    base256 = None
    for px in PNG_SIZES:
        img = render(px)
        out = icons_dir / f"{px}x{px}" / "apps"
        out.mkdir(parents=True, exist_ok=True)
        img.save(out / f"{APPID}.png")
        if px == 256:
            base256 = img
        print(f"  PNG {px}x{px}")

    # .ico для Windows (много размеров в одном файле)
    ico = HERE / f"{APPID}.ico"
    base256.save(ico, format="ICO", sizes=[(n, n) for n in ICO_SIZES])
    print(f"  ICO -> {ico.name}  {ICO_SIZES}")

    # отдельная 256-px картинка для предпросмотра/README
    (HERE / f"{APPID}-256.png").write_bytes((icons_dir / "256x256" / "apps" / f"{APPID}.png").read_bytes())
    print("Готово.")


if __name__ == "__main__":
    main()
