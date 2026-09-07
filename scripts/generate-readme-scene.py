#!/usr/bin/env python3
"""Generate the script-free SVG scene embedded in both READMEs.

Run with Python and Node/npm: python3 scripts/generate-readme-scene.py.
The default output passes through the pinned, animation-safe SVGO configuration.
Use --unoptimized --output <path> to inspect the generated source without Node.
Geometry, palette, face numbers and sampled trails follow src/scene3d/;
the looping trajectories are illustrative, not decoded media.
"""

from math import cos, pi, radians, sin
from pathlib import Path
import argparse
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "assets/readme/spatial-orbit.svg"
WIDTH, HEIGHT = 880, 560
SCALE = 215
ORIGIN = (WIDTH / 2, 264)
DURATION = 4
# Match scene3d::params: forty fixed-size samples, one every 40 ms. The
# illustrative orbit is fast enough for individual samples to remain legible.
TRAIL_SAMPLES = 40
TRAIL_INTERVAL = 0.04
STEPS = round(DURATION / TRAIL_INTERVAL)
FLOOR, CEILING = -0.7, 0.5
EDGE = 0.11
TRAIL_EDGE = EDGE * 0.30
TRAIL_FADE = 0.85
FLOOR_TRAIL_WEIGHT = 0.45
# A front quarter view makes the listener's facing cues visible.
AZIMUTH, ELEVATION = radians(215), radians(20)
RIGHT = (cos(AZIMUTH), 0, -sin(AZIMUTH))
TOWARD = (sin(AZIMUTH), 0, cos(AZIMUTH))
UP = (-sin(ELEVATION) * TOWARD[0], cos(ELEVATION), -sin(ELEVATION) * TOWARD[2])
EYE = (cos(ELEVATION) * TOWARD[0], sin(ELEVATION), cos(ELEVATION) * TOWARD[2])
STAGE, INK, ACCENT = "#f8f3ea", "#332a1f", "#ce7a3b"
MUTED, TEXT, BORDER = "#9a8b76", "#372c20", "#ece3d4"


def dot(a, b):
    return sum(x * y for x, y in zip(a, b))


def add(a, b):
    return tuple(x + y for x, y in zip(a, b))


def multiply(a, value):
    return tuple(x * value for x in a)


def project(point, local=False):
    x, y = SCALE * dot(point, RIGHT), -SCALE * dot(point, UP)
    return (x, y) if local else (x + ORIGIN[0], y + ORIGIN[1])


def fmt(value):
    return f"{value:.2f}".rstrip("0").rstrip(".") if abs(value) > 0.005 else "0"


def blend(start, end, amount):
    a = [int(start[i:i + 2], 16) for i in (1, 3, 5)]
    b = [int(end[i:i + 2], 16) for i in (1, 3, 5)]
    return "#" + "".join(f"{round(x + (y - x) * amount):02x}" for x, y in zip(a, b))


def faded(color, point):
    # The non-degenerate view's aerial perspective from scene3d::mesh::faded.
    depth = dot(point, EYE)
    amount = max(0, min(1, .5 - depth / 3.5)) * .18
    return blend(color, STAGE, amount)


def polygon(points, fill, local=False, **attributes):
    coords = " ".join(",".join(fmt(v) for v in project(p, local)) for p in points)
    attrs = " ".join(f'{k.replace("_", "-")}="{v}"' for k, v in attributes.items())
    return f'<polygon points="{coords}" fill="{fill}" {attrs}/>'


def line(a, b, color, width=1, local=False):
    x1, y1 = project(a, local)
    x2, y2 = project(b, local)
    # Share number styling while retaining separate strokes and their original
    # antialiasing/compositing at intersections.
    style = 'class="n"' if color == INK and width == 1.15 else (
        f'fill="none" stroke="{color}" stroke-width="{fmt(width)}"'
    )
    return (f'<path d="M{fmt(x1)} {fmt(y1)}L{fmt(x2)} {fmt(y2)}" '
            f'{style}/>')


# The same outward face bases and seven-segment digits as scene.rs.
FACES = [
    ((0, 1, 0), (1, 0, 0), (0, 0, -1), 1.00),
    ((-1, 0, 0), (0, 0, 1), (0, 1, 0), 0.88),
    ((0, 0, -1), (-1, 0, 0), (0, 1, 0), 0.76),
]
SEGMENTS = [
    ((-.27, .50), (.27, .50)), ((-.35, .42), (-.35, .08)),
    ((.35, .42), (.35, .08)), ((-.27, 0), (.27, 0)),
    ((-.35, -.08), (-.35, -.42)), ((.35, -.08), (.35, -.42)),
    ((-.27, -.50), (.27, -.50)),
]
DIGITS = [0b1110111, 0b0100100, 0b1011101, 0b1101101, 0b0101110, 0b1101011, 0b1111011]


def box(center, size, color, number=None, local=False, face_colors=None):
    result = []
    for index, (normal, right, up, tone) in enumerate(FACES):
        extent = lambda axis: sum(abs(a) * b for a, b in zip(axis, size))
        face = add(center, multiply(normal, extent(normal) / 2))
        corner = lambda x, y: add(face, add(multiply(right, x), multiply(up, y)))
        w, h = extent(right) / 2, extent(up) / 2
        fill = face_colors[index] if face_colors else faded(blend(color, INK, 1 - tone), center)
        result.append(polygon([corner(-w, -h), corner(w, -h), corner(w, h), corner(-w, h)], fill, local))
        if number is not None:
            scale = min(2 * w / .7, 2 * h) * .72
            for bit, (a, b) in enumerate(SEGMENTS):
                if DIGITS[number] & (1 << bit):
                    offset = -.35 if number == 1 else 0
                    result.append(line(corner((a[0] + offset) * scale, a[1] * scale),
                                       corner((b[0] + offset) * scale, b[1] * scale),
                                       INK, 1.15, local))
    return "".join(result)


def floor_mark(center, edge, color, local=False):
    x, y, z = center
    h = edge / 2
    return polygon([(x - h, y, z - h), (x + h, y, z - h),
                    (x + h, y, z + h), (x - h, y, z + h)], color, local)


def position(ring, theta):
    radius = .80 if ring == "upper" else .67
    height = .12 + .085 * sin(2 * theta) if ring == "upper" else -.24 + .075 * sin(2 * theta)
    ground = add(multiply(RIGHT, radius * cos(theta)), multiply(TOWARD, radius * sin(theta)))
    return (ground[0], height, ground[2])


def motion_name(ring, floor=False, drop=False):
    return ("u" if ring == "upper" else "l") + ("d" if drop else "f" if floor else "")


def trajectory(ring, floor=False, drop=False):
    frames = []
    for step in range(STEPS + 1):
        # Use exactly the same endpoint to make the loop seamless.
        theta = 2 * pi * (step % STEPS) / STEPS
        point = position(ring, theta)
        if drop:
            height = (point[1] - EDGE / 2 - FLOOR) * cos(ELEVATION) * SCALE
            value = f"scaleY({fmt(height)})"
        else:
            if floor:
                point = (point[0], FLOOR, point[2])
            x, y = project(point)
            value = f"translate({fmt(x)}px,{fmt(y)}px)"
        frames.append(f"{fmt(step / STEPS * 100)}%{{transform:{value}}}")
    name = motion_name(ring, floor, drop)
    return f"@keyframes {name}{{" + "".join(frames) + "}"


def phase_index(theta, age=0):
    return (round(theta / (2 * pi) * STEPS) - round(age / TRAIL_INTERVAL)) % STEPS


def moving(ring, theta, *, sprite=None, content="", layer=None, floor=False,
           age=0, opacity=1, sampled=False, color_class=""):
    phase = phase_index(theta, age)
    angle = phase / STEPS * 2 * pi
    point = position(ring, angle)
    if floor:
        point = (point[0], FLOOR, point[2])
    x, y = project(point)
    classes = ["m", motion_name(ring, floor), f"p{phase}"]
    if sampled:
        classes.append("s")
    if color_class:
        classes.append(color_class)
    if layer:
        # Independent transform and opacity animations on one <use> replace
        # the old nested motion/depth groups without changing their timing.
        classes.append("f" if layer == "front" else "b")
        opacity = int((phase < STEPS / 2) == (layer == "front"))
    attrs = f'class="{" ".join(classes)}" transform="translate({fmt(x)} {fmt(y)})"'
    if opacity != 1:
        attrs += f' opacity="{fmt(opacity)}"'
    if sprite:
        return f'<use href="#{sprite}" {attrs}/>'
    return f'<g {attrs}>{content}</g>'


def build():
    objects = [(1, "upper", .32), (4, "lower", 1.18)]
    stationary = [(2, (-.91, .41, -.89)), (3, (.88, .02, -.89)),
                  (5, (-.88, -.10, .87)), (6, (.88, .41, .87))]
    objects = [(number, ring, phase_index(theta) / STEPS * 2 * pi)
               for number, ring, theta in objects]
    phases = sorted({phase_index(theta, sample * TRAIL_INTERVAL)
                     for _, _, theta in objects for sample in range(TRAIL_SAMPLES + 1)})
    svg = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{HEIGHT}" '
           f'viewBox="0 0 {WIDTH} {HEIGHT}" role="img" aria-labelledby="title desc">',
           '<title id="title">MacinDecode AC-4 Player — spatial object scene</title>',
           '<desc id="desc">Six orange audio objects surround a voxel listener in a warm, '
           'gridded room. Two orbit while four stay in place. Short trails show recent positions; vertical guides '
           'connect them to the floor. This is an illustrative loop. Reduced-motion '
           'preferences display a still scene.</desc>',
           '<!-- Generated by scripts/generate-readme-scene.py. No scripts or external assets. -->',
           '<style>',
           f'.m{{animation-duration:{DURATION}s;animation-iteration-count:infinite;'
           'animation-timing-function:var(--q,linear),steps(1,end);animation-delay:var(--d,0s);'
           'animation-name:var(--k),var(--z,none)}',
           f'.n{{fill:none;stroke:{INK};stroke-width:1.15}}',
           '.s{--q:steps(1,end)}.f{--z:f}.b{--z:b}',
           '@keyframes f{0%,100%{opacity:1}50%{opacity:0}}',
           '@keyframes b{0%,100%{opacity:0}50%{opacity:1}}']
    for ring in ("upper", "lower"):
        for name in (motion_name(ring), motion_name(ring, floor=True), motion_name(ring, drop=True)):
            svg.append(f'.{name}{{--k:{name}}}')
        svg.extend([trajectory(ring), trajectory(ring, floor=True), trajectory(ring, drop=True)])
    for phase in phases:
        svg.append(f'.p{phase}{{--d:-{fmt(phase * TRAIL_INTERVAL)}s}}')
    for sample in range(1, TRAIL_SAMPLES + 1):
        freshness = (TRAIL_SAMPLES - sample + 1) / TRAIL_SAMPLES
        color = blend(ACCENT, STAGE, TRAIL_FADE * (1 - freshness))
        floor_color = blend(color, STAGE, 1 - FLOOR_TRAIL_WEIGHT)
        # Forty color palettes share one cube and one floor-mark geometry.
        # Keep the same rounded face colors as the original separate templates.
        faces = [faded(blend(color, INK, 1 - tone), (0, 0, 0)) for *_, tone in FACES]
        svg.append(f'.c{sample}{{--a:{faces[0]};--b:{faces[1]};--c:{faces[2]};fill:{floor_color}}}')
    svg.extend(['@media (prefers-reduced-motion:reduce){.m{animation:none}}',
                '</style>', '<defs>'])
    for number, _, _ in objects:
        svg.append(f'<g id="o{number}">{box((0, 0, 0), (EDGE,) * 3, ACCENT, number, True)}</g>')
    svg.append(f'<g id="t">{box((0, 0, 0), (TRAIL_EDGE,) * 3, ACCENT, local=True, face_colors=("var(--a)", "var(--b)", "var(--c)"))}</g>')
    svg.append(f'<g id="tf">{floor_mark((0, 0, 0), TRAIL_EDGE * .7, "inherit", True)}</g>')
    footprint_color = blend(MUTED, STAGE, .4)
    svg.extend([f'<g id="p">{floor_mark((0, 0, 0), EDGE * 1.60, footprint_color, True)}</g>',
                '</defs>',
                f'<path fill="{STAGE}" stroke="{BORDER}" d="M.5 .5H879.5V559.5H.5Z"/>',
                '<path fill="#fffefa" d="M1 1H879V48H1ZM1 516H879V559H1Z"/>',
                f'<path d="M1 48.5H879M1 515.5H879" stroke="{BORDER}"/>',
                '<g font-family="-apple-system,BlinkMacSystemFont,Segoe UI,sans-serif">',
                f'<text x="22" y="30" font-size="15" font-weight="600" fill="{TEXT}">Object scene</text>',
                f'<text x="858" y="29" text-anchor="end" font-size="11" letter-spacing="1.3" '
                f'fill="{MUTED}">AC-4 / SPATIAL AUDIO</text>', '</g>'])

    # Floor, graded 16-by-16 ruler, then the low room's twelve edges.
    svg.append(floor_mark((0, FLOOR, 0), 2, "#f4eee3"))
    for step in range(17):
        v = -1 + step / 8
        width, color = (.95, "#b8ac99") if step == 8 else (.7, "#c9bda9") if step % 4 == 0 else (.5, "#ddd3c3")
        svg.append(line((v, FLOOR, -1), (v, FLOOR, 1), color, width))
        svg.append(line((-1, FLOOR, v), (1, FLOOR, v), color, width))
    for y in (FLOOR, CEILING):
        for v in (-1, 1):
            svg.append(line((v, y, -1), (v, y, 1), "#c8bcaa", .85))
            svg.append(line((-1, y, v), (1, y, v), "#c8bcaa", .85))
    for x in (-1, 1):
        for z in (-1, 1):
            svg.append(line((x, FLOOR, z), (x, CEILING, z), "#c8bcaa", .85))

    # Like the reference scene, only a few objects move at once. This keeps
    # separate trails readable instead of filling the room with overlapping rings.
    for _, (x, y, z) in stationary:
        svg.append(floor_mark((x, FLOOR, z), EDGE * 1.60, footprint_color))
        svg.append(line((x, y - EDGE / 2, z), (x, FLOOR, z), blend(MUTED, BORDER, .35), .8))
    for _, ring, theta in objects:
        for sample in range(TRAIL_SAMPLES, 0, -1):
            svg.append(moving(ring, theta, sprite="tf", floor=True, color_class=f"c{sample}",
                              age=sample * TRAIL_INTERVAL, sampled=True))
        svg.append(moving(ring, theta, sprite="p", floor=True))
        point = position(ring, theta)
        height = (point[1] - EDGE / 2 - FLOOR) * cos(ELEVATION) * SCALE
        guide = (f'<g class="m {motion_name(ring, drop=True)}" transform="scale(1 {fmt(height)})">'
                 f'<path d="M0 0V-1" stroke="{MUTED}" stroke-width=".7" '
                 'vector-effect="non-scaling-stroke"/></g>')
        svg.append(moving(ring, theta, content=guide, floor=True, opacity=.5))

    def moving_layer(layer):
        for number, point in stationary:
            if (dot(point, TOWARD) >= 0) == (layer == "front"):
                svg.append(box(point, (EDGE,) * 3, ACCENT, number))
        for number, ring, theta in objects:
            for sample in range(TRAIL_SAMPLES, 0, -1):
                svg.append(moving(ring, theta, sprite="t", layer=layer, color_class=f"c{sample}",
                                  age=sample * TRAIL_INTERVAL, sampled=True))
            svg.append(moving(ring, theta, sprite=f"o{number}", layer=layer))

    moving_layer("back")
    skin = blend(MUTED, TEXT, .22)
    cloth = blend(MUTED, INK, .5)
    parts = [
        ((-.05, -.55, 0), (.1, .3, .1), cloth),
        ((.05, -.55, 0), (.1, .3, .1), cloth),
        ((0, -.25, 0), (.2, .3, .1), cloth),
        ((-.15, -.25, 0), (.1, .3, .1), skin),
        ((.15, -.25, 0), (.1, .3, .1), skin),
        ((0, 0, 0), (.225,) * 3, blend(skin, INK, .16)),
        ((-.0425, .0225, -.125), (.0325,) * 3, blend(INK, TEXT, .2)),
        ((.0425, .0225, -.125), (.0325,) * 3, blend(INK, TEXT, .2)),
        ((0, -.04, -.13125), (.045,) * 3, ACCENT),
    ]
    svg.append('<g id="listener">')
    for center, size, color in sorted(parts, key=lambda part: dot(part[0], EYE)):
        svg.append(box(center, size, color))
    svg.append('</g>')
    moving_layer("front")
    # The LFE remains a stationary cabinet at the front wall, outside the orbit.
    svg.append(box((0, FLOOR + .11, -.9285), (.5, .22, .143), ACCENT, 0))
    svg.extend(['<g font-family="-apple-system,BlinkMacSystemFont,Segoe UI,sans-serif" font-size="11">',
                f'<rect x="22" y="535" width="7" height="7" fill="{ACCENT}"/>',
                f'<text x="37" y="543" fill="{TEXT}">6 objects + LFE</text>',
                f'<text x="440" y="543" text-anchor="middle" fill="{MUTED}">Position / elevation / history</text>',
                f'<text x="858" y="543" text-anchor="end" fill="{MUTED}">Illustrative orbit</text>',
                '</g>', '</svg>'])
    return "\n".join(svg) + "\n"


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=OUTPUT)
    parser.add_argument("--unoptimized", action="store_true", help="Skip SVGO; only Python is required")
    args = parser.parse_args()
    svg = build()
    if not args.unoptimized:
        npm = shutil.which("npm")
        if npm is None:
            parser.error("Node.js/npm is required for SVGO; use --unoptimized for Python-only output")
        result = subprocess.run(
            [npm, "exec", "--yes", "--package=svgo@4.0.0", "--", "svgo", "--quiet",
             "--config", str(ROOT / "scripts/readme-svgo.config.mjs"), "--input=-", "--output=-"],
            input=svg, text=True, encoding="utf-8", capture_output=True, cwd=ROOT, check=True,
        )
        svg = result.stdout
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(svg, encoding="utf-8")
    print(f"{args.output}: {args.output.stat().st_size:,} bytes")
