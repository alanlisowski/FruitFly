"""
Reference drawing for the desktop fly, cartoon style (pycairo).
Port this to tiny-skia; do not ship it.
Run: python fly_reference.py <outdir>      (needs: pip install pycairo)

Body space: fly faces +x, y down, 1 unit = 1 px at 100% display scale.
"""
import math
import random
import cairo

INK = (0.11, 0.08, 0.13)   # warm dark ink, softer than pure black

PALETTES = {
    "green": dict(
        # yellow-green highlight -> muted green -> grey-green shade -> grey rim
        body=[(0.86, 0.90, 0.48), (0.30, 0.58, 0.36), (0.20, 0.32, 0.27), (0.15, 0.17, 0.17)],
        eye=[(1.00, 0.52, 0.58), (0.80, 0.16, 0.26), (0.40, 0.04, 0.12)],
        wing=(0.92, 0.93, 0.92, 0.40), smoke=(0.45, 0.47, 0.48, 0.24),   # grey wings
        sheen=((1.00, 0.88, 0.35), 0.70, (0.58, 0.60, 0.62), 0.55),       # yellow glint, grey rim
        segment=(0.92, 0.80, 0.30),                                        # yellow stripes
    ),
}

PALETTE = "green"
FLY_SCALE = 1.9   # ~70 px nose to wingtip at 100% display scale ("medium")

LIGHT = (-0.55, -0.83)  # screen space, top-left


def light_in_body(heading):
    """Rotate the screen light into body space so highlights stay top-left on
    screen as the fly turns (otherwise the shading looks painted on)."""
    c, s = math.cos(-heading), math.sin(-heading)
    return (LIGHT[0] * c - LIGHT[1] * s, LIGHT[0] * s + LIGHT[1] * c)


def ellipse_path(cr, cx, cy, rx, ry, ang=0.0):
    cr.save()
    cr.translate(cx, cy)
    cr.rotate(ang)
    cr.scale(rx, ry)
    cr.new_path()
    cr.arc(0, 0, 1, 0, 2 * math.pi)
    cr.restore()
    return cr.copy_path()


def inked_part(cr, path, cx, cy, size, stops, L, weight=1.0, sheen=None):
    """Fill + ink the way an illustrator would: a dark offset silhouette first,
    so the outline is heavier on the side away from the light."""
    # 0. faint light rim so a dark body still separates from a dark background
    cr.append_path(path)
    cr.set_source_rgba(1, 1, 1, 0.14)
    cr.set_line_width(2.4 * weight)
    cr.stroke()
    # 1. heavy ink on the shadow side
    cr.save()
    cr.translate(-L[0] * 0.6 * weight, -L[1] * 0.6 * weight)
    cr.append_path(path)
    cr.set_source_rgb(*INK)
    cr.fill()
    cr.restore()
    # 2. shaded fill, highlight pushed toward the light
    g = cairo.RadialGradient(cx + L[0] * size * 0.45, cy + L[1] * size * 0.45, 0,
                             cx, cy, size * 1.25)
    for t, col in zip((0.0, 0.35, 0.75, 1.0), stops):
        g.add_color_stop_rgb(t, *col)
    cr.append_path(path)
    cr.set_source(g)
    cr.fill_preserve()
    # 2b. green accents on a lilac body -- small and placed, not a wash over it:
    #     a tight metallic glint where the light hits, and a thin iridescent rim
    #     along the shadow-side edge only.
    if sheen is not None:
        (hr, hg, hb), ha, (rr, rg, rb), ra = sheen
        hx, hy = cx + L[0] * size * 0.42, cy + L[1] * size * 0.42
        sg = cairo.RadialGradient(hx, hy, 0, hx, hy, size * 0.36)
        sg.add_color_stop_rgba(0.0, hr, hg, hb, ha)
        sg.add_color_stop_rgba(1.0, hr, hg, hb, 0.0)
        cr.set_source(sg)
        cr.fill_preserve()
        cr.save()
        cr.clip_preserve()
        lg = cairo.LinearGradient(cx + L[0] * size, cy + L[1] * size,
                                  cx - L[0] * size, cy - L[1] * size)
        lg.add_color_stop_rgba(0.0, rr, rg, rb, 0.0)
        lg.add_color_stop_rgba(0.55, rr, rg, rb, 0.0)
        lg.add_color_stop_rgba(1.0, rr, rg, rb, ra)
        cr.set_source(lg)
        cr.set_line_width(size * 0.30)   # half of it is clipped away, leaving a thin inner rim
        cr.stroke_preserve()
        cr.restore()
    # 3. thin ink line all round
    cr.set_source_rgb(*INK)
    cr.set_line_width(1.15 * weight)
    cr.stroke()


def fuzz(cr, cx, cy, rx, ry, n, seed, length=1.6, width=0.6, skip_front=False):
    """Soft curved tufts along an elliptical rim. Seeded, so identical every
    frame -- regenerate them per frame and the fly shimmers."""
    rnd = random.Random(seed)
    cr.set_source_rgb(*INK)
    cr.set_line_width(width)
    cr.set_line_cap(cairo.LINE_CAP_ROUND)
    for i in range(n):
        a = 2 * math.pi * i / n + rnd.uniform(-0.15, 0.15)
        if skip_front and abs(math.atan2(math.sin(a), math.cos(a))) < 0.8:
            continue
        x, y = cx + math.cos(a) * rx, cy + math.sin(a) * ry
        nx, ny = math.cos(a) / rx, math.sin(a) / ry
        nn = math.hypot(nx, ny)
        nx, ny = nx / nn, ny / nn
        l = length * rnd.uniform(0.7, 1.2)
        # curl each hair backward (toward -x), like it's been combed
        ex, ey = x + nx * l - 0.5 * l, y + ny * l
        cr.move_to(x, y)
        cr.curve_to(x + nx * l * 0.6, y + ny * l * 0.6, ex + 0.3, ey, ex, ey)
        cr.stroke()


def two_bone(ax, ay, fx, fy, l1, l2):
    """Knee for a leg of fixed segment lengths; returns both solutions."""
    dx, dy = fx - ax, fy - ay
    d = min(math.hypot(dx, dy), l1 + l2 - 1e-3)
    a = (l1 * l1 - l2 * l2 + d * d) / (2 * d)
    h = math.sqrt(max(l1 * l1 - a * a, 0.0))
    ux, uy = dx / d, dy / d
    mx, my = ax + ux * a, ay + uy * a
    return (mx - uy * h, my + ux * h), (mx + uy * h, my - ux * h)


def ink_line(cr, pts, w, halo=True):
    if halo:
        cr.set_source_rgba(1, 1, 1, 0.13)
        cr.set_line_width(w + 1.2)
        cr.set_line_cap(cairo.LINE_CAP_ROUND)
        cr.set_line_join(cairo.LINE_JOIN_ROUND)
        cr.move_to(*pts[0])
        for p in pts[1:]:
            cr.line_to(*p)
        cr.stroke()
    cr.set_source_rgb(*INK)
    cr.set_line_width(w)
    cr.set_line_cap(cairo.LINE_CAP_ROUND)
    cr.set_line_join(cairo.LINE_JOIN_ROUND)
    cr.move_to(*pts[0])
    for p in pts[1:]:
        cr.line_to(*p)
    cr.stroke()


def spikes(cr, p0, p1, n, side, seed, length=1.3):
    """Short bristles along a leg segment, pointing outward and toward the foot."""
    rnd = random.Random(seed)
    dx, dy = p1[0] - p0[0], p1[1] - p0[1]
    L = math.hypot(dx, dy) or 1.0
    ux, uy = dx / L, dy / L
    nx, ny = -uy * side, ux * side
    cr.set_source_rgb(*INK)
    cr.set_line_width(0.6)
    cr.set_line_cap(cairo.LINE_CAP_ROUND)
    for i in range(n):
        t = (i + 1) / (n + 1)
        x, y = p0[0] + dx * t, p0[1] + dy * t
        l = length * rnd.uniform(0.7, 1.2)
        cr.move_to(x, y)
        cr.line_to(x + (nx * 0.8 + ux * 0.6) * l, y + (ny * 0.8 + uy * 0.6) * l)
        cr.stroke()


# attach x, |y|, direction from forward axis (deg), femur, tibia, tarsus
LEGS = [
    (7.5, 5.4, 44, 7.0, 7.4, 4.0),
    (3.5, 7.0, 94, 7.4, 8.2, 4.2),
    (-0.5, 6.4, 140, 8.2, 9.4, 4.6),
]


def draw_legs(cr, stride):
    for side in (-1, 1):
        for i, (ax, ay, deg, f, t, ts) in enumerate(LEGS):
            ay *= side
            th = math.radians(deg)
            reach = (f + t) * 0.90
            phase = 1 if (i % 2 == 0) == (side > 0) else -1   # alternating tripod
            fx = ax + math.cos(th) * reach + stride * phase
            fy = ay + math.sin(th) * reach * side
            k1, k2 = two_bone(ax, ay, fx, fy, f, t)
            kx, ky = k1 if abs(k1[1]) > abs(k2[1]) else k2   # knee away from the body
            dx, dy = fx - kx, fy - ky
            n = math.hypot(dx, dy) or 1.0
            tx, ty = fx + dx / n * ts, fy + dy / n * ts + 0.8 * side
            ink_line(cr, [(ax, ay), (kx, ky)], 2.2)
            ink_line(cr, [(kx, ky), (fx, fy)], 1.8)
            ink_line(cr, [(fx, fy), (tx, ty)], 1.4)
            spikes(cr, (kx, ky), (fx, fy), 2, side, seed=31 * i + (side > 0), length=1.4)
            # little round foot instead of claws
            cr.set_source_rgba(1, 1, 1, 0.13)
            cr.arc(tx, ty, 1.6, 0, 2 * math.pi)
            cr.fill()
            cr.set_source_rgb(*INK)
            cr.arc(tx, ty, 1.05, 0, 2 * math.pi)
            cr.fill()


def draw_wing(cr, side, pal):
    cr.save()
    cr.translate(0.5, 3.6 * side)
    cr.rotate(math.radians(180 - 33 * side))   # back, in a wide V
    cr.scale(0.88, -0.88 * side)                # smaller, mirrored so both wings match
    cr.new_path()
    cr.move_to(0, 0)
    cr.curve_to(4, -6.0, 17, -8.0, 23.5, -4.8)
    cr.curve_to(28.0, -2.2, 27.5, 4.0, 22.0, 5.2)
    cr.curve_to(13.0, 6.8, 4.0, 3.6, 0, 0)
    cr.close_path()
    wing = cr.copy_path()
    cr.set_source_rgba(1, 1, 1, 0.22)          # halo, for dark backgrounds
    cr.set_line_width(2.2)
    cr.stroke_preserve()
    cr.set_source_rgba(*pal["wing"])
    cr.fill_preserve()
    if "wing_tip" in pal:
        tg = cairo.LinearGradient(12, 0, 27, 0)
        tr, tgc, tb, ta = pal["wing_tip"]
        tg.add_color_stop_rgba(0, tr, tgc, tb, 0)
        tg.add_color_stop_rgba(1, tr, tgc, tb, ta)
        cr.set_source(tg)
        cr.fill_preserve()
    g = cairo.LinearGradient(0, 0, 12, 0)
    r, gg, b, a = pal["smoke"]
    g.add_color_stop_rgba(0, r, gg, b, a)
    g.add_color_stop_rgba(1, r, gg, b, 0)
    cr.set_source(g)
    cr.fill()
    cr.append_path(wing)
    cr.clip()
    cr.set_source_rgba(*INK, 0.6)
    cr.set_line_width(0.6)
    cr.set_line_cap(cairo.LINE_CAP_ROUND)
    for end in (-3.6, -0.6, 2.4):
        cr.move_to(1.5, 0)
        cr.curve_to(8, end * 0.35, 15, end * 0.8, 23, end * 1.0)
        cr.stroke()
    # sheen: one soft white streak along the leading edge
    cr.set_source_rgba(1, 1, 1, 0.65)
    cr.set_line_width(1.0)
    cr.move_to(6, -3.4)
    cr.curve_to(10, -5.2, 15, -5.6, 19, -5.0)
    cr.stroke()
    cr.reset_clip()
    cr.append_path(wing)
    cr.set_source_rgb(*INK)
    cr.set_line_width(1.0)
    cr.stroke()
    cr.restore()


def draw_fly(cr, heading=0.0, stride=0.0, palette=PALETTE):
    pal = PALETTES[palette]
    L = light_in_body(heading)
    body = pal["body"]

    draw_legs(cr, stride)

    # abdomen: chubby and round, with soft segment arcs
    ab = ellipse_path(cr, -10.0, 0, 9.2, 8.0)
    inked_part(cr, ab, -10.0, 0, 9.0, body, L, sheen=pal.get("sheen"))
    cr.save()
    cr.append_path(ab)
    cr.clip()
    seg = pal.get("segment")
    if seg:
        cr.set_source_rgba(*seg, 0.9)
        cr.set_line_width(1.0)
    else:
        cr.set_source_rgba(*INK, 0.55)
        cr.set_line_width(0.8)
    cr.set_line_cap(cairo.LINE_CAP_ROUND)
    for x0 in (-7.0, -11.5):
        cr.move_to(x0 + 1.0, -6.5)
        cr.curve_to(x0 - 1.6, -2.5, x0 - 1.6, 2.5, x0 + 1.0, 6.5)
        cr.stroke()
    cr.restore()
    fuzz(cr, -10.0, 0, 9.2, 8.0, 22, seed=5, length=1.9)

    # thorax: round, not boxy
    th = ellipse_path(cr, 2.8, 0, 7.2, 7.6)
    inked_part(cr, th, 2.8, 0, 7.2, body, L, sheen=pal.get("sheen"))
    fuzz(cr, 2.8, 0, 7.2, 7.6, 16, seed=11, length=1.8, skip_front=True)
    # a little tuft on top: cheap, and it's most of the charm
    cr.set_source_rgb(*INK)
    cr.set_line_width(0.7)
    cr.set_line_cap(cairo.LINE_CAP_ROUND)
    for dy, dl in ((-1.5, 2.8), (0.0, 3.4), (1.5, 2.8)):
        cr.move_to(4.6, dy)
        cr.curve_to(3.2, dy * 1.3, 2.4, dy * 1.6, 4.6 - dl, dy * 2.1)
        cr.stroke()

    # head: big and round (baby proportions read as friendly)
    hd = ellipse_path(cr, 11.6, 0, 5.4, 6.2)
    inked_part(cr, hd, 11.6, 0, 5.8, body, L, weight=0.9, sheen=pal.get("sheen"))
    # goggle eyes: huge, touching in the middle
    eye = pal["eye"]
    for side in (-1, 1):
        ecx, ecy = 13.4, 3.75 * side
        ep = ellipse_path(cr, ecx, ecy, 4.1, 3.9, 0.15 * side)
        inked_part(cr, ep, ecx, ecy, 4.0, [eye[0], eye[1], eye[2], eye[2]], L, weight=0.9)
        # two-point glint: a big soft oval toward the light, a small dot opposite
        gx, gy = ecx + L[0] * 1.5, ecy + L[1] * 1.5
        cr.save()
        cr.translate(gx, gy)
        cr.rotate(math.atan2(L[1], L[0]))
        cr.scale(1.0, 1.45)
        cr.arc(0, 0, 1.0, 0, 2 * math.pi)
        cr.restore()
        cr.set_source_rgba(1, 1, 1, 0.92)
        cr.fill()
        cr.arc(ecx - L[0] * 1.9, ecy - L[1] * 1.9, 0.5, 0, 2 * math.pi)
        cr.set_source_rgba(1, 1, 1, 0.70)
        cr.fill()
    # bead-tipped antennae
    for side in (-1, 1):
        ink_line(cr, [(16.6, 0.7 * side), (18.3, 1.5 * side), (19.3, 2.4 * side)], 0.8)
        cr.set_source_rgb(*INK)
        cr.arc(19.4, 2.5 * side, 0.75, 0, 2 * math.pi)
        cr.fill()

    # wings on top
    for side in (-1, 1):
        draw_wing(cr, side, pal)


def draw_shadow(cr, scale):
    """Soft contact shadow; fixed down-right in SCREEN space, drawn before rotation."""
    cr.save()
    cr.translate(3 * scale, 5 * scale)
    cr.scale(26 * scale, 13 * scale)
    g = cairo.RadialGradient(0, 0, 0, 0, 0, 1)
    g.add_color_stop_rgba(0, 0, 0, 0, 0.30)
    g.add_color_stop_rgba(1, 0, 0, 0, 0)
    cr.set_source(g)
    cr.arc(0, 0, 1, 0, 2 * math.pi)
    cr.fill()
    cr.restore()


def tile(w, h, bg, heading, scale, palette, stride=0.0):
    s = cairo.ImageSurface(cairo.FORMAT_ARGB32, w, h)
    cr = cairo.Context(s)
    cr.set_source_rgb(*bg)
    cr.paint()
    cr.translate(w / 2, h / 2)
    draw_shadow(cr, scale)
    cr.scale(scale, scale)
    cr.rotate(heading)
    draw_fly(cr, heading, stride, palette)
    return s


if __name__ == "__main__":
    import sys
    outdir = sys.argv[1]
    WHITE, DARK, GREY = (1, 1, 1), (0.12, 0.12, 0.13), (0.94, 0.95, 0.97)

    # zoomed 4.2x: rest, turned -35 deg, turned 140 deg; on white and dark
    out = cairo.ImageSurface(cairo.FORMAT_ARGB32, 1400, 870)
    cr = cairo.Context(out)
    cr.set_source_rgb(0.93, 0.93, 0.92)
    cr.paint()
    for i, hd in enumerate((0.0, math.radians(-35), math.radians(140))):
        for j, bg in enumerate((WHITE, DARK)):
            t = tile(440, 420, bg, hd, 4.2, PALETTE)
            cr.set_source_surface(t, 15 + i * 460, 15 + j * 435)
            cr.paint()
    out.write_to_png(f"{outdir}/ref_zoomed.png")

    # actual size at FLY_SCALE on white, dark, light grey
    out = cairo.ImageSurface(cairo.FORMAT_ARGB32, 960, 240)
    cr = cairo.Context(out)
    for k, bg in enumerate((WHITE, DARK, GREY)):
        t = tile(320, 240, bg, math.radians(-25), FLY_SCALE, PALETTE)
        cr.set_source_surface(t, k * 320, 0)
        cr.paint()
    out.write_to_png(f"{outdir}/ref_actual_size.png")

    # both tripod phases, zoomed: legs must never cross
    out = cairo.ImageSurface(cairo.FORMAT_ARGB32, 900, 420)
    cr = cairo.Context(out)
    for k, st in enumerate((-3.0, 3.0)):
        t = tile(450, 420, WHITE, 0.0, 4.2, PALETTE, stride=st)
        cr.set_source_surface(t, k * 450, 0)
        cr.paint()
    out.write_to_png(f"{outdir}/ref_stride.png")
    print("ok")
