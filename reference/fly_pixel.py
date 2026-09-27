"""
Reference drawing for the desktop fly, pixel-art style (pycairo).
Port this to tiny-skia; do not ship it.
Run: python fly_pixel.py <outdir>      (needs: pip install pycairo)

How the pixel look works:
  1. Draw the fly into a SMALL canvas where 1 pixel = 1 "art pixel", with
     anti-aliasing OFF, using a few flat colours per part (a ramp), never gradients.
  2. Scale that canvas up by an integer factor (ART_PX) with nearest-neighbour, so each
     art pixel becomes a crisp ART_PX x ART_PX block on screen.
  At other display scales: art_px = floor(dpi scale) (see art_px_for) and the body is
  drawn at FLY_SCALE * dpi scale, so the fly keeps its size and only the chunkiness changes.
  The fly still rotates to any angle and its legs still move: the shapes are re-drawn
  each frame onto the fixed, screen-aligned art grid, so pixels rearrange, never blur.

Body space: fly faces +x, y down. Units are screen px at FLY_SCALE = 1.0, 100% display.
"""
import math
import cairo

FLY_SCALE = 1.3   # ~60 px across legs at 100% display: ~0.6x the old "medium"
ART_PX = 1        # screen pixels per art pixel at 100% display.


def art_px_for(dpi_scale):
    """Art pixels must be whole screen pixels, so: floor, never below 1.
    100-175% -> 1, 200-275% -> 2. The fly keeps the same size on screen at every
    scale because render() is called with scale = FLY_SCALE * dpi_scale."""
    return max(1, int(math.floor(dpi_scale + 1e-6)))

# ---- palette (from the chosen pixel-art image) -------------------------------------------
OUTLINE = (0.27, 0.08, 0.17)                    # dark maroon, never pure black
GREEN = [(0.33, 0.49, 0.32), (0.47, 0.65, 0.40), (0.62, 0.77, 0.49)]      # dark, mid, light
YELLOW = [(0.80, 0.72, 0.30), (0.95, 0.89, 0.50), (1.00, 0.97, 0.74)]
GREY = [(0.24, 0.25, 0.27), (0.36, 0.38, 0.40), (0.50, 0.52, 0.54)]
WING = [(0.78, 0.79, 0.84), (0.88, 0.89, 0.92), (0.96, 0.96, 0.98)]
VEIN = (0.66, 0.67, 0.73)
EYE = [(0.50, 0.04, 0.13), (0.78, 0.10, 0.19), (0.94, 0.34, 0.37)]
WHITE = (1.0, 1.0, 1.0)
SHADOW = (0.45, 0.30, 0.38, 0.28)               # soft pinkish grey, flat

LIGHT = (-0.55, -0.83)                          # screen space, top-left


def light_in_body(heading):
    """Screen light rotated into body space, so the lit side stays top-left on
    screen however the fly turns."""
    c, s = math.cos(-heading), math.sin(-heading)
    return (LIGHT[0] * c - LIGHT[1] * s, LIGHT[0] * s + LIGHT[1] * c)


class Ctx:
    """Drawing context in body units, with helpers that think in art pixels."""

    def __init__(self, cr, k, L):
        self.cr = cr      # cairo context already transformed into body space
        self.k = k        # art pixels per body unit
        self.L = L        # light direction in body space

    def ap(self, n):
        """n art pixels, expressed in body units."""
        return n / self.k


def ellipse(cr, cx, cy, rx, ry):
    cr.save()
    cr.translate(cx, cy)
    cr.scale(rx, ry)
    cr.new_path()
    cr.arc(0, 0, 1, 0, 2 * math.pi)
    cr.restore()
    return cr.copy_path()


def blob(c, path, ramp, hi_scale=0.55, hi_extra=None):
    """A flat-shaded pixel blob: 1-art-px outline, dark base, mid shifted toward the
    light, a light patch nearest the light. No gradients anywhere."""
    cr = c.cr
    dark, mid, light = ramp
    # outline: a 2-art-px stroke in outline colour, then the fill covers its inner half
    cr.append_path(path)
    cr.set_source_rgb(*OUTLINE)
    cr.set_line_width(c.ap(2))
    cr.stroke()
    cr.save()
    cr.append_path(path)
    cr.clip()
    cr.append_path(path)
    cr.set_source_rgb(*dark)
    cr.fill()
    # mid tone: the same shape nudged toward the light
    cr.save()
    cr.translate(c.L[0] * c.ap(1.5), c.L[1] * c.ap(1.5))
    cr.append_path(path)
    cr.set_source_rgb(*mid)
    cr.fill()
    cr.restore()
    if hi_extra is not None:
        hi_extra()
    cr.restore()


def shaded_ellipse(c, cx, cy, rx, ry, ramp, highlight=None, hi_size=0.45):
    path = ellipse(c.cr, cx, cy, rx, ry)

    def hi():
        if highlight is None:
            return
        hx, hy = cx + c.L[0] * rx * 0.45, cy + c.L[1] * ry * 0.45
        ellipse(c.cr, hx, hy, rx * hi_size, ry * hi_size)
        c.cr.set_source_rgb(*highlight)
        c.cr.fill()

    blob(c, path, ramp, hi_extra=hi)


def leg_line(c, pts, colour, width_ap):
    cr = c.cr
    cr.set_line_cap(cairo.LINE_CAP_ROUND)
    cr.set_line_join(cairo.LINE_JOIN_ROUND)
    for w, col in ((width_ap + 2, OUTLINE), (width_ap, colour)):
        cr.set_line_width(c.ap(w))
        cr.set_source_rgb(*col)
        cr.move_to(*pts[0])
        for p in pts[1:]:
            cr.line_to(*p)
        cr.stroke()


def two_bone(ax, ay, fx, fy, l1, l2):
    dx, dy = fx - ax, fy - ay
    d = min(math.hypot(dx, dy), l1 + l2 - 1e-3)
    a = (l1 * l1 - l2 * l2 + d * d) / (2 * d)
    h = math.sqrt(max(l1 * l1 - a * a, 0.0))
    ux, uy = dx / d, dy / d
    mx, my = ax + ux * a, ay + uy * a
    return (mx - uy * h, my + ux * h), (mx + uy * h, my - ux * h)


# attach (x, |y|), rest foot (x, |y|), femur, tibia   -- measured off the chosen image
LEGS = [
    ((2.8, 6.6), (9.8, 16.2), 7.2, 7.6),     # front: up and out
    ((-0.8, 7.2), (-5.4, 19.2), 7.4, 8.2),   # middle: straight out
    ((-7.6, 3.6), (-20.2, 13.4), 9.2, 10.4),  # hind: back and down
]


def draw_legs(c, stride):
    for side in (-1, 1):
        for i, ((ax, ay), (fx, fy), f, t) in enumerate(LEGS):
            ay, fy = ay * side, fy * side
            phase = 1 if (i % 2 == 0) == (side > 0) else -1      # alternating tripod
            fx += stride * phase
            k1, k2 = two_bone(ax, ay, fx, fy, f, t)
            if i == 1:   # middle: knee bends backward, clear of the front leg
                kx, ky = k1 if k1[0] < k2[0] else k2
            else:        # front, hind: knee away from the body
                kx, ky = k1 if abs(k1[1]) > abs(k2[1]) else k2
            leg_line(c, [(ax, ay), (kx, ky), (fx, fy)], GREY[1], 1.2)
            # round foot
            cr = c.cr
            cr.new_path()
            cr.arc(fx, fy, c.ap(1.6), 0, 2 * math.pi)
            cr.set_source_rgb(*OUTLINE)
            cr.fill()
            cr.new_path()
            cr.arc(fx, fy, c.ap(1.0), 0, 2 * math.pi)
            cr.set_source_rgb(*GREY[1])
            cr.fill()


def draw_wing(c, side):
    cr = c.cr
    cr.save()
    cr.translate(-2.6, 2.4 * side)
    cr.rotate(math.radians(180 - 33 * side))
    cr.scale(1, side)
    cr.new_path()
    # a broad, slightly squared paddle like the image
    cr.move_to(0, -1.2)
    cr.line_to(12.5, -3.4)
    cr.curve_to(15.2, -3.6, 15.6, 3.6, 12.8, 3.8)
    cr.line_to(0.6, 3.0)
    cr.close_path()
    cr.restore()                 # restore FIRST: copy_path returns the current user space
    path = cr.copy_path()
    blob(c, path, WING)
    # two veins, 1 art px, clipped inside the wing
    cr.save()
    cr.append_path(path)
    cr.clip()
    cr.set_source_rgb(*VEIN)
    cr.set_line_width(c.ap(1))
    cr.translate(-2.6, 2.4 * side)
    cr.rotate(math.radians(180 - 33 * side))
    cr.scale(1, side)
    for y0, y1 in ((-0.4, -1.4), (1.2, 1.6)):
        cr.move_to(1.5, y0)
        cr.line_to(13.5, y1)
        cr.stroke()
    cr.restore()


def draw_fly(c, stride=0.0):
    cr = c.cr
    draw_legs(c, stride)

    # abdomen: a rounded shield pointing backward, with two yellow stripes
    cr.new_path()
    cr.move_to(-9.6, -8.2)
    cr.curve_to(-9.0, -3.0, -9.0, 3.0, -9.6, 8.2)
    cr.curve_to(-14.0, 8.0, -19.5, 3.5, -21.4, 0.0)
    cr.curve_to(-19.5, -3.5, -14.0, -8.0, -9.6, -8.2)
    cr.close_path()
    ab = cr.copy_path()

    def stripes():
        for x0, w in ((-12.6, 1.8), (-16.0, 1.6)):
            cr.new_path()
            cr.rectangle(x0 - w / 2, -9, w, 18)
            cr.set_source_rgb(*YELLOW[1])
            cr.fill()
        # a light glint on the lit side
        cr.new_path()
        hx, hy = -13.5 + c.L[0] * 3.5, c.L[1] * 4.0
        cr.arc(hx, hy, 1.4, 0, 2 * math.pi)
        cr.set_source_rgb(*GREEN[2])
        cr.fill()

    blob(c, ab, GREEN, hi_extra=stripes)

    for side in (-1, 1):
        draw_wing(c, side)

    # waist
    shaded_ellipse(c, -7.6, 0, 2.2, 2.4, GREEN)

    # thorax: round, with the big yellow highlight on the lit side
    shaded_ellipse(c, 0.0, 0, 5.6, 7.6, GREEN, highlight=YELLOW[1], hi_size=0.5)

    # the grey moustache tuft between thorax and head
    cr.new_path()
    cr.move_to(5.6, -2.8)
    for i, (x, y) in enumerate(((3.4, -2.2), (4.4, -1.2), (2.6, -0.4), (3.8, 0.4),
                                (2.4, 1.2), (4.2, 1.8), (3.2, 2.6), (5.6, 2.8))):
        cr.line_to(x, y)
    cr.close_path()
    blob(c, cr.copy_path(), GREY)

    # head, with a yellow cap at the front
    head = ellipse(cr, 10.6, 0, 5.4, 3.6)

    def cap():
        cr.new_path()
        cr.arc(14.2, 0, 2.2, 0, 2 * math.pi)
        cr.set_source_rgb(*YELLOW[1])
        cr.fill()

    blob(c, head, GREEN, hi_extra=cap)

    # antennae with round tips
    for side in (-1, 1):
        leg_line(c, [(15.0, 1.2 * side), (18.4, 2.6 * side), (20.6, 4.4 * side)], GREEN[1], 0.6)
        cr.new_path()
        cr.arc(20.8, 4.6 * side, c.ap(1.7), 0, 2 * math.pi)
        cr.set_source_rgb(*OUTLINE)
        cr.fill()
        cr.new_path()
        cr.arc(20.8, 4.6 * side, c.ap(1.0), 0, 2 * math.pi)
        cr.set_source_rgb(*YELLOW[1])
        cr.fill()

    # the eyes: huge, on either side of the head, two white glints each
    for side in (-1, 1):
        ex, ey = 11.0, 5.9 * side
        path = ellipse(cr, ex, ey, 4.4, 4.4)

        def glints(ex=ex, ey=ey):
            cr.new_path()
            cr.arc(ex + c.L[0] * 1.9, ey + c.L[1] * 1.9, 1.35, 0, 2 * math.pi)
            cr.set_source_rgb(*WHITE)
            cr.fill()
            cr.new_path()
            cr.arc(ex - c.L[0] * 0.4 + c.L[1] * 1.6, ey - c.L[1] * 0.4 - c.L[0] * 1.6,
                   0.7, 0, 2 * math.pi)
            cr.set_source_rgb(*WHITE)
            cr.fill()

        def eye_hi(ex=ex, ey=ey, path=path):
            cr.new_path()
            cr.arc(ex + c.L[0] * 1.4, ey + c.L[1] * 1.4, 2.6, 0, 2 * math.pi)
            cr.set_source_rgb(*EYE[2])
            cr.fill()
            glints()

        blob(c, path, EYE, hi_extra=eye_hi)


def render(heading, stride=0.0, scale=FLY_SCALE, art_px=ART_PX, bg=None, size=None):
    """Render the fly to a low-res art canvas (size x size art pixels), then upscale
    with nearest neighbour. Returns the upscaled cairo surface."""
    k = scale / art_px                         # art pixels per body unit
    size = size or int(86 * scale / art_px)
    low = cairo.ImageSurface(cairo.FORMAT_ARGB32, size, size)
    cr = cairo.Context(low)
    cr.set_antialias(cairo.ANTIALIAS_NONE)     # the whole trick: no smoothing, ever
    cr.translate(size / 2, size / 2)
    # shadow: screen space, flat, offset down-right
    cr.save()
    cr.translate(1.5 * k * 2, 3.0 * k * 2)
    cr.scale(k * 17, k * 8)
    cr.arc(0, 0, 1, 0, 2 * math.pi)
    cr.restore()
    cr.set_source_rgba(*SHADOW)
    cr.fill()
    cr.scale(k, k)
    cr.rotate(heading)
    draw_fly(Ctx(cr, k, light_in_body(heading)), stride)

    out = cairo.ImageSurface(cairo.FORMAT_ARGB32, size * art_px, size * art_px)
    o = cairo.Context(out)
    if bg is not None:
        o.set_source_rgb(*bg)
        o.paint()
    o.scale(art_px, art_px)
    o.set_source_surface(low, 0, 0)
    o.get_source().set_filter(cairo.FILTER_NEAREST)
    o.paint()
    return out


def zoom(surface, factor):
    """Blow a finished image up for viewing, keeping pixels crisp."""
    w, h = surface.get_width(), surface.get_height()
    out = cairo.ImageSurface(cairo.FORMAT_ARGB32, w * factor, h * factor)
    o = cairo.Context(out)
    o.scale(factor, factor)
    o.set_source_surface(surface, 0, 0)
    o.get_source().set_filter(cairo.FILTER_NEAREST)
    o.paint()
    return out


if __name__ == "__main__":
    import sys
    outdir = sys.argv[1]
    WHITE_BG, DARK_BG, GREY_BG = (1, 1, 1), (0.12, 0.12, 0.13), (0.94, 0.95, 0.97)
    UP = -math.pi / 2

    # 1. close-up: facing up like the image, zoomed so the art pixels are visible
    big = zoom(render(UP, bg=WHITE_BG), 5)
    big.write_to_png(f"{outdir}/pixel_closeup.png")

    # 2. turning: same fly at several headings, zoomed 3x, to see pixels rearrange
    heads = [-90, -75, -60, -45, -20, 0, 30, 135]
    sheet = cairo.ImageSurface(cairo.FORMAT_ARGB32, len(heads) * 340, 340)
    s = cairo.Context(sheet)
    for i, d in enumerate(heads):
        t = zoom(render(math.radians(d), bg=WHITE_BG), 3)
        s.set_source_surface(t, i * 340 + 2, 2)
        s.paint()
    sheet.write_to_png(f"{outdir}/pixel_turning.png")

    # 3. walking: tripod phases, facing up, zoomed 3x
    walk = cairo.ImageSurface(cairo.FORMAT_ARGB32, 4 * 340, 340)
    w = cairo.Context(walk)
    for i, st in enumerate((-2.2, -0.8, 0.8, 2.2)):
        t = zoom(render(UP, stride=st, bg=WHITE_BG), 3)
        w.set_source_surface(t, i * 340 + 2, 2)
        w.paint()
    walk.write_to_png(f"{outdir}/pixel_walking.png")

    # 4. actual size at 100% display on white, dark, light grey (no zoom!)
    act = cairo.ImageSurface(cairo.FORMAT_ARGB32, 3 * 160, 140)
    a = cairo.Context(act)
    for i, bg in enumerate((WHITE_BG, DARK_BG, GREY_BG)):
        a.set_source_rgb(*bg)
        a.rectangle(i * 160, 0, 160, 140)
        a.fill()
        t = render(math.radians(-60), stride=0.8)
        a.set_source_surface(t, i * 160 + 24, 14)
        a.paint()
    act.write_to_png(f"{outdir}/pixel_actual_size.png")

    # 5. display scaling: what the fly is in PHYSICAL pixels at 100 / 150 / 200 %
    scales = (1.0, 1.25, 1.5, 2.0)
    dpi = cairo.ImageSurface(cairo.FORMAT_ARGB32, len(scales) * 260, 240)
    d = cairo.Context(dpi)
    d.set_source_rgb(*WHITE_BG)
    d.paint()
    for i, sc in enumerate(scales):
        t = render(math.radians(-60), stride=0.8, scale=FLY_SCALE * sc, art_px=art_px_for(sc))
        d.set_source_surface(t, i * 260 + (260 - t.get_width()) // 2, (240 - t.get_height()) // 2)
        d.paint()
    dpi.write_to_png(f"{outdir}/pixel_dpi_100_125_150_200.png")
    print("ok")
