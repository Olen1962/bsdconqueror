#!/usr/bin/env python3
"""Gera os ícones do BSDConqueror (PNG e SVG) a partir de formas simples.

Domínio público: usa, altera e distribui como quiseres.

Uso (na raiz do projeto):
    python3 tools/make_icons.py

Precisa de Python 3 e da biblioteca Pillow (pkg install py311-pillow, ou pip install pillow).
Escreve em src/icons/:  <nome>-48x48.png, app-128x128.png e svg/<nome>.svg
"""
import math
import os

from PIL import Image, ImageDraw

# ───────────── paleta ─────────────
INK = "#2d3b4e"
BLUE = "#2a6fb8"
GREEN = "#2f9e59"
RED = "#d6453d"
AMBER = "#e0a21b"
AMBER_DARK = "#a8730f"
GRAY = "#9fb3c8"
PALE = "#e8f1fb"
PINK = "#f08c8c"
WHITE = "#ffffff"

VB = 32          # as formas são desenhadas num quadrado de 32 x 32 unidades
SS = 16          # sobre-amostragem para suavizar as arestas


# ───────────── primitivas ─────────────
def circle(cx, cy, r, fill=None, stroke=None, sw=0):
    return dict(t="circle", cx=cx, cy=cy, r=r, fill=fill, stroke=stroke, sw=sw)


def line(pts, color, w):
    return dict(t="line", pts=pts, color=color, w=w)


def poly(pts, fill=None, stroke=None, sw=0):
    return dict(t="poly", pts=pts, fill=fill, stroke=stroke, sw=sw)


def arc(cx, cy, r, a0, a1, color, w):
    """Arco no sentido horário (ângulos em graus, 0 = direita, -90 = cima)."""
    return dict(t="arc", cx=cx, cy=cy, r=r, a0=a0, a1=a1, color=color, w=w)


def rrect(x0, y0, x1, y1, r, fill=None, stroke=None, sw=0):
    return dict(t="rrect", x0=x0, y0=y0, x1=x1, y1=y1, r=r, fill=fill, stroke=stroke, sw=sw)


def arc_points(p):
    n = max(8, int(abs(p["a1"] - p["a0"]) / 3))
    out = []
    for i in range(n + 1):
        a = math.radians(p["a0"] + (p["a1"] - p["a0"]) * i / n)
        out.append((p["cx"] + p["r"] * math.cos(a), p["cy"] + p["r"] * math.sin(a)))
    return out


def arrow_head(px, py, dx, dy, size, color):
    L = math.hypot(dx, dy)
    dx, dy = dx / L, dy / L
    nx, ny = -dy, dx
    tip = (px + dx * size * 0.9, py + dy * size * 0.9)
    b1 = (px - dx * size * 0.5 + nx * size * 0.95, py - dy * size * 0.5 + ny * size * 0.95)
    b2 = (px - dx * size * 0.5 - nx * size * 0.95, py - dy * size * 0.5 - ny * size * 0.95)
    return poly([tip, b1, b2], fill=color)


def mirror(prims):
    out = []
    for p in prims:
        q = dict(p)
        t = p["t"]
        if t == "circle":
            q["cx"] = VB - p["cx"]
        elif t in ("line", "poly"):
            q["pts"] = [(VB - x, y) for x, y in p["pts"]]
        elif t == "arc":
            q["cx"] = VB - p["cx"]
            q["a0"], q["a1"] = 180 - p["a1"], 180 - p["a0"]
        elif t == "rrect":
            q["x0"], q["x1"] = VB - p["x1"], VB - p["x0"]
        out.append(q)
    return out


def rot(pt, theta, c):
    x, y = pt
    cs, sn = math.cos(theta), math.sin(theta)
    return (c[0] + x * cs - y * sn, c[1] + x * sn + y * cs)


# ───────────── desenho dos ícones ─────────────
def icon_add():
    return [
        circle(16, 16, 13, fill=GREEN),
        line([(16, 9), (16, 23)], WHITE, 3.4),
        line([(9, 16), (23, 16)], WHITE, 3.4),
    ]


def icon_remove():
    return [
        circle(16, 16, 13, fill=RED),
        line([(9, 16), (23, 16)], WHITE, 3.4),
    ]


def icon_quit():
    return [
        arc(16, 17, 9.5, -55, 235, RED, 3.4),
        line([(16, 5.5), (16, 15)], RED, 3.4),
    ]


def icon_search():
    return [
        circle(13, 13, 8.5, fill=PALE),
        circle(13, 13, 8.5, stroke=BLUE, sw=3.4),
        line([(19.5, 19.5), (27, 27)], BLUE, 4.4),
    ]


def icon_refresh():
    cx, cy, r, end = 16, 16, 9.5, 235
    a = math.radians(end)
    px, py = cx + r * math.cos(a), cy + r * math.sin(a)
    dx, dy = -math.sin(a), math.cos(a)
    return [
        arc(cx, cy, r, -30, end, BLUE, 3.4),
        arrow_head(px, py, dx, dy, 4.4, BLUE),
    ]


def icon_reset():
    cx, cy, r, end = 16, 16, 9.5, 235
    a = math.radians(end)
    px, py = cx + r * math.cos(a), cy + r * math.sin(a)
    dx, dy = -math.sin(a), math.cos(a)
    base = [
        arc(cx, cy, r, -30, end, INK, 3.4),
        arrow_head(px, py, dx, dy, 4.4, INK),
    ]
    return mirror(base)


def icon_select_all():
    return [
        rrect(5, 5, 27, 27, 3.5, fill=PALE, stroke=BLUE, sw=2.6),
        line([(10.5, 16.5), (14.5, 20.5), (22, 11.5)], BLUE, 3.2),
    ]


def icon_clear():
    th = math.radians(-40)
    c = (16, 14.5)
    body = [rot(p, th, c) for p in [(-11, -5.5), (3, -5.5), (3, 5.5), (-11, 5.5)]]
    tip = [rot(p, th, c) for p in [(3, -5.5), (11, -5.5), (11, 5.5), (3, 5.5)]]
    return [
        poly(body, fill=GRAY, stroke=INK, sw=1.6),
        poly(tip, fill=PINK, stroke=INK, sw=1.6),
        line([(5, 27), (27, 27)], INK, 2.6),
    ]


def _padlock_body(color):
    return [
        rrect(6, 14, 26, 28, 3, fill=color, stroke=AMBER_DARK, sw=1.2),
        circle(16, 20.2, 2.1, fill=INK),
        line([(16, 21), (16, 24.5)], INK, 2.2),
    ]


def icon_lock():
    return [
        arc(16, 12.5, 6, 180, 360, INK, 3.2),
        line([(10, 12.5), (10, 15)], INK, 3.2),
        line([(22, 12.5), (22, 15)], INK, 3.2),
    ] + _padlock_body(AMBER)


def icon_unlock():
    return [
        arc(16, 9.5, 6, 180, 360, INK, 3.2),
        line([(22, 9.5), (22, 15)], INK, 3.2),
    ] + _padlock_body(GRAY)


def icon_app():
    return [
        rrect(1.5, 1.5, 30.5, 30.5, 7, fill=BLUE),
        circle(14, 14, 7.2, stroke=WHITE, sw=3),
        line([(19.5, 19.5), (25.5, 25.5)], WHITE, 4),
    ]


ICONS = {
    "add": icon_add,
    "remove": icon_remove,
    "quit": icon_quit,
    "search": icon_search,
    "refresh": icon_refresh,
    "reset": icon_reset,
    "select_all": icon_select_all,
    "clear": icon_clear,
    "lock": icon_lock,
    "unlock": icon_unlock,
}


# ───────────── saída PNG ─────────────
def render_png(prims, size):
    big = VB * SS
    img = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    def P(p):
        return (p[0] * SS, p[1] * SS)

    def thick(pts, color, w):
        r = w * SS / 2
        for a, b in zip(pts, pts[1:]):
            ax, ay = P(a)
            bx, by = P(b)
            dx, dy = bx - ax, by - ay
            L = math.hypot(dx, dy) or 1
            nx, ny = -dy / L * r, dx / L * r
            d.polygon([(ax + nx, ay + ny), (bx + nx, by + ny), (bx - nx, by - ny), (ax - nx, ay - ny)], fill=color)
        for p in pts:
            x, y = P(p)
            d.ellipse([x - r, y - r, x + r, y + r], fill=color)

    for p in prims:
        t = p["t"]
        if t == "circle":
            x, y, r = p["cx"] * SS, p["cy"] * SS, p["r"] * SS
            if p["fill"]:
                d.ellipse([x - r, y - r, x + r, y + r], fill=p["fill"])
            if p["stroke"]:
                h = p["sw"] * SS / 2
                d.ellipse([x - r - h, y - r - h, x + r + h, y + r + h], outline=p["stroke"], width=int(p["sw"] * SS))
        elif t == "line":
            thick(p["pts"], p["color"], p["w"])
        elif t == "arc":
            thick(arc_points(p), p["color"], p["w"])
        elif t == "poly":
            if p["fill"]:
                d.polygon([P(q) for q in p["pts"]], fill=p["fill"])
            if p["stroke"]:
                thick(list(p["pts"]) + [p["pts"][0]], p["stroke"], p["sw"])
        elif t == "rrect":
            x0, y0, x1, y1, r = p["x0"] * SS, p["y0"] * SS, p["x1"] * SS, p["y1"] * SS, p["r"] * SS
            if p["stroke"]:
                h = p["sw"] * SS / 2
                if p["fill"]:
                    d.rounded_rectangle([x0 - h, y0 - h, x1 + h, y1 + h], radius=r + h, fill=p["stroke"])
                    d.rounded_rectangle([x0 + h, y0 + h, x1 - h, y1 - h], radius=max(r - h, 0), fill=p["fill"])
                else:
                    d.rounded_rectangle([x0 - h, y0 - h, x1 + h, y1 + h], radius=r + h,
                                        outline=p["stroke"], width=int(p["sw"] * SS))
            elif p["fill"]:
                d.rounded_rectangle([x0, y0, x1, y1], radius=r, fill=p["fill"])
    return img.resize((size, size), Image.LANCZOS)


# ───────────── saída SVG ─────────────
def f(v):
    return f"{v:.2f}".rstrip("0").rstrip(".")


def render_svg(prims):
    out = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {VB} {VB}" width="{VB}" height="{VB}">']
    for p in prims:
        t = p["t"]
        if t == "circle":
            out.append(f'<circle cx="{f(p["cx"])}" cy="{f(p["cy"])}" r="{f(p["r"])}" '
                       f'fill="{p["fill"] or "none"}" stroke="{p["stroke"] or "none"}" stroke-width="{f(p["sw"])}"/>')
        elif t == "line":
            pts = " ".join(f"{f(x)},{f(y)}" for x, y in p["pts"])
            out.append(f'<polyline points="{pts}" fill="none" stroke="{p["color"]}" stroke-width="{f(p["w"])}" '
                       f'stroke-linecap="round" stroke-linejoin="round"/>')
        elif t == "arc":
            pts = arc_points(p)
            (x0, y0), (x1, y1) = pts[0], pts[-1]
            large = 1 if (p["a1"] - p["a0"]) % 360 > 180 else 0
            out.append(f'<path d="M {f(x0)} {f(y0)} A {f(p["r"])} {f(p["r"])} 0 {large} 1 {f(x1)} {f(y1)}" '
                       f'fill="none" stroke="{p["color"]}" stroke-width="{f(p["w"])}" stroke-linecap="round"/>')
        elif t == "poly":
            pts = " ".join(f"{f(x)},{f(y)}" for x, y in p["pts"])
            out.append(f'<polygon points="{pts}" fill="{p["fill"] or "none"}" stroke="{p["stroke"] or "none"}" '
                       f'stroke-width="{f(p["sw"])}" stroke-linejoin="round"/>')
        elif t == "rrect":
            out.append(f'<rect x="{f(p["x0"])}" y="{f(p["y0"])}" width="{f(p["x1"] - p["x0"])}" '
                       f'height="{f(p["y1"] - p["y0"])}" rx="{f(p["r"])}" ry="{f(p["r"])}" '
                       f'fill="{p["fill"] or "none"}" stroke="{p["stroke"] or "none"}" stroke-width="{f(p["sw"])}"/>')
    out.append("</svg>")
    return "\n".join(out) + "\n"


def main():
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    icons_dir = os.path.join(root, "src", "icons")
    svg_dir = os.path.join(icons_dir, "svg")
    os.makedirs(svg_dir, exist_ok=True)
    for name, fn in ICONS.items():
        prims = fn()
        render_png(prims, 48).save(os.path.join(icons_dir, f"{name}-48x48.png"))
        with open(os.path.join(svg_dir, f"{name}.svg"), "w", encoding="utf-8") as fh:
            fh.write(render_svg(prims))
    app = icon_app()
    render_png(app, 128).save(os.path.join(icons_dir, "app-128x128.png"))
    with open(os.path.join(svg_dir, "app.svg"), "w", encoding="utf-8") as fh:
        fh.write(render_svg(app))
    print("Ícones escritos em", icons_dir)


if __name__ == "__main__":
    main()
