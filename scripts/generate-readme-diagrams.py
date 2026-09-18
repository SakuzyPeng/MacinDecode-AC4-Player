#!/usr/bin/env python3
"""Generate the four explanatory SVGs the manual and the READMEs share.

Run with Python and Node/npm: python3 scripts/generate-readme-diagrams.py.
Use --unoptimized to skip SVGO, and --only <name> to rebuild one diagram.

These annotate what the player actually draws, so the palette, the isometric
camera and the voxel geometry are imported from generate-readme-scene.py rather
than restated, and the numbers below are transcribed from the Rust that owns
them. Change one of those constants and this file has to follow; that is the
point of keeping them in one visible block instead of hard-coding pixels.
"""

from math import cos, pi, radians, sin
from pathlib import Path
import argparse
import importlib.util
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
OUTPUT_DIR = ROOT / "assets/readme"


def _scene_module():
    # The scene generator's filename is not an identifier, so it is loaded by
    # path. Its module level only defines constants and helpers.
    path = ROOT / "scripts/generate-readme-scene.py"
    spec = importlib.util.spec_from_file_location("readme_scene", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


scene = _scene_module()
blend, fmt, project = scene.blend, scene.fmt, scene.project
STAGE, INK, ACCENT = scene.STAGE, scene.INK, scene.ACCENT
MUTED, TEXT, BORDER = scene.MUTED, scene.TEXT, scene.BORDER
EDGE = scene.EDGE
# The rest of src/theme.rs, which the scene image happens not to need.
SURFACE, BACKGROUND, HOVER = "#fffefa", "#fbf7f0", "#f1e9dc"
ACCENT_SOFT, WARNING = "#f4e2d1", "#b0483a"

# --- Transcribed from src/scene3d/params.rs and src/app.rs ---------------
# The silence floor, as decibels. OBJECT_SILENT_GAIN is 10^-1.8, and
# app::loudness_fraction maps the scale linearly in decibels from it to zero.
SILENT_DECIBELS = -36.0
FOOTPRINT_MIN_SCALE, FOOTPRINT_MAX_SCALE = 0.45, 1.60
NAMEPLATE_SILENT_ALPHA = 0.32
SILENT_PRESENCE_FLOOR = 0.10
OBJECT_SILENT_FADE = 0.55
SILENCE_HOLD_SECONDS, SILENCE_FADE_SECONDS = 2.0, 0.6
PEAK_HOLD_SECONDS = 1.6

# The straight-down view the projection diagram uses, zoomed into the middle of
# the floor. The height is params::CAMERA_DISTANCE rounded up, so the parallax
# is the player's own rather than a chosen amount.
TOP_SCALE, TOP_CAMERA_HEIGHT = 285.0, 9.0

SANS = "-apple-system,BlinkMacSystemFont,Segoe UI,sans-serif"
MONO = "ui-monospace,SFMono-Regular,Menlo,Consolas,monospace"
# Three fills that answer "who owns this box", used by the paths diagram.
PLAYER_FILL, PLAYER_EDGE = ACCENT_SOFT, blend(ACCENT, STAGE, 0.55)
SYSTEM_FILL, SYSTEM_EDGE = blend(MUTED, STAGE, 0.80), blend(MUTED, STAGE, 0.40)
DEVICE_FILL, DEVICE_EDGE = SURFACE, blend(MUTED, STAGE, 0.35)


def fraction(decibels):
    """A decibel reading's place on the scale, as app::loudness_fraction."""
    return min(1.0, max(0.0, 1.0 + decibels / -SILENT_DECIBELS))


def footprint_scale(decibels):
    """Footprint edge as a multiple of OBJECT_EDGE, read in decibels."""
    span = FOOTPRINT_MAX_SCALE - FOOTPRINT_MIN_SCALE
    return FOOTPRINT_MIN_SCALE + span * fraction(decibels)


def esc(value):
    return value.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


# --- Flat drawing primitives ---------------------------------------------


def text(x, y, content, *, size=11, fill=TEXT, weight=None, anchor=None,
         mono=False, opacity=None, spacing=None):
    attrs = [f'x="{fmt(x)}"', f'y="{fmt(y)}"', f'font-size="{fmt(size)}"',
             f'fill="{fill}"']
    if mono:
        attrs.append(f'font-family="{MONO}"')
    if weight:
        attrs.append(f'font-weight="{weight}"')
    if anchor:
        attrs.append(f'text-anchor="{anchor}"')
    if opacity is not None:
        attrs.append(f'opacity="{fmt(opacity)}"')
    if spacing:
        attrs.append(f'letter-spacing="{fmt(spacing)}"')
    return f'<text {" ".join(attrs)}>{esc(content)}</text>'


def rect(x, y, w, h, *, fill="none", stroke=None, width=1, radius=None,
         opacity=None):
    attrs = [f'x="{fmt(x)}"', f'y="{fmt(y)}"', f'width="{fmt(w)}"',
             f'height="{fmt(h)}"', f'fill="{fill}"']
    if radius:
        attrs.append(f'rx="{fmt(radius)}"')
    if stroke:
        attrs.extend([f'stroke="{stroke}"', f'stroke-width="{fmt(width)}"'])
    if opacity is not None:
        attrs.append(f'opacity="{fmt(opacity)}"')
    return f'<rect {" ".join(attrs)}/>'


def stroke_path(d, color, width=1, *, dash=None, opacity=None):
    attrs = [f'd="{d}"', 'fill="none"', f'stroke="{color}"',
             f'stroke-width="{fmt(width)}"']
    if dash:
        attrs.append(f'stroke-dasharray="{dash}"')
    if opacity is not None:
        attrs.append(f'opacity="{fmt(opacity)}"')
    return f'<path {" ".join(attrs)}/>'


def arrow(x, y, *, size=5, color=MUTED, down=False):
    """A chevron in the gap between two boxes, pointing the way the flow goes."""
    if down:
        d = (f'M{fmt(x - size)} {fmt(y - size / 2)}L{fmt(x)} {fmt(y + size / 2)}'
             f'L{fmt(x + size)} {fmt(y - size / 2)}')
    else:
        d = (f'M{fmt(x - size / 2)} {fmt(y - size)}L{fmt(x + size / 2)} {fmt(y)}'
             f'L{fmt(x - size / 2)} {fmt(y + size)}')
    return (f'<path d="{d}" fill="none" stroke="{color}" stroke-width="1.4" '
            'stroke-linecap="round" stroke-linejoin="round"/>')


def leader(points, *, color=None, dot=True):
    """A thin polyline from a label to the thing it names."""
    color = blend(MUTED, STAGE, 0.25) if color is None else color
    d = "M" + "L".join(f"{fmt(x)} {fmt(y)}" for x, y in points)
    end = points[-1]
    tip = (f'<circle cx="{fmt(end[0])}" cy="{fmt(end[1])}" r="1.8" '
           f'fill="{color}"/>') if dot else ""
    return stroke_path(d, color, 0.9) + tip


def caption(x, y, lines, *, size=10.5, fill=MUTED, anchor=None, leading=13,
            weight=None):
    """SVG text does not wrap, so every break here is deliberate."""
    return "".join(text(x, y + index * leading, line, size=size, fill=fill,
                        anchor=anchor, weight=weight)
                   for index, line in enumerate(lines))


def card(width, height, heading, tag):
    """The framed panel the scene image already uses, so these read as a set."""
    footer = height - 44
    return [
        f'<path fill="{STAGE}" stroke="{BORDER}" '
        f'd="M.5 .5H{fmt(width - .5)}V{fmt(height - .5)}H.5Z"/>',
        f'<path fill="{SURFACE}" d="M1 1H{fmt(width - 1)}V48H1Z'
        f'M1 {fmt(footer)}H{fmt(width - 1)}V{fmt(height - 1)}H1Z"/>',
        f'<path d="M1 48.5H{fmt(width - 1)}M1 {fmt(footer + .5)}H{fmt(width - 1)}" '
        f'stroke="{BORDER}"/>',
        text(22, 30, heading, size=15, weight=600, fill=TEXT),
        text(width - 22, 29, tag, size=11, fill=MUTED, anchor="end", spacing=1.3),
    ]


def document(width, height, title, description, body):
    return "\n".join([
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" '
        f'height="{height}" viewBox="0 0 {width} {height}" role="img" '
        'aria-labelledby="title desc">',
        f'<title id="title">{esc(title)}</title>',
        f'<desc id="desc">{esc(description)}</desc>',
        '<!-- Generated by scripts/generate-readme-diagrams.py. '
        'No scripts or external assets. -->',
        f'<style>.n{{fill:none;stroke:{INK};stroke-width:1.15}}'
        f'.w .n{{stroke:{SURFACE}}}</style>',
        f'<g font-family="{SANS}">',
        *body,
        '</g>', '</svg>']) + "\n"


# --- Pieces of the player's own scene ------------------------------------


def floor_square(edge, *, fill="none", stroke=None, width=1, opacity=None):
    half = edge / 2
    corners = [(-half, 0, -half), (half, 0, -half), (half, 0, half),
               (-half, 0, half)]
    points = " ".join(",".join(fmt(v) for v in project(p, True)) for p in corners)
    attrs = [f'points="{points}"', f'fill="{fill}"']
    if stroke:
        attrs.extend([f'stroke="{stroke}"', f'stroke-width="{fmt(width)}"'])
    if opacity is not None:
        attrs.append(f'opacity="{fmt(opacity)}"')
    return f'<polygon {" ".join(attrs)}/>'


def nameplate(x, y, sign, magnitude, *, level_db=None, opacity=1.0):
    """The readout as app::draw_object_nameplates draws it: a dark pill with a
    fixed cell for the sign, and the same value again as a strip along the
    bottom. Proportions follow NAMEPLATE_*; the size is enlarged to stay
    legible at README width."""
    width, height, strip, pad = 62.0, 24.0, 4.0, 8.0
    parts = [rect(x - width / 2, y - height, width, height, radius=3,
                  fill=INK, opacity=0.70 * opacity)]
    baseline = y - height + (height - strip) / 2 + 4
    parts.append(text(x - width / 2 + pad, baseline, sign, size=12, mono=True,
                      fill=BACKGROUND, opacity=opacity))
    # Infinity has no decimal point to align, so it sits against the sign.
    if magnitude == "∞":
        parts.append(text(x - width / 2 + pad + 9, baseline, magnitude, size=12,
                          mono=True, fill=BACKGROUND, opacity=opacity))
    else:
        parts.append(text(x + width / 2 - pad, baseline, magnitude, size=12,
                          mono=True, fill=BACKGROUND, anchor="end",
                          opacity=opacity))
    parts.append(rect(x - width / 2, y - strip, width, strip, fill=INK,
                      opacity=0.35 * opacity))
    if level_db is not None:
        filled = width * fraction(level_db)
        if filled > 0:
            parts.append(rect(x - width / 2, y - strip, filled, strip,
                              fill=ACCENT, opacity=opacity))
    return "".join(parts)


def object_art(cx, cy, *, scale, gain_db, level_db, number, plate=None,
               faded=False, plate_opacity=1.0, ring_highlight=False):
    """One object above its two-layer footprint, drawn at (cx, cy)."""
    previous = scene.SCALE
    scene.SCALE = scale
    try:
        hover = 0.30
        ring_edge = EDGE * footprint_scale(gain_db)
        core_edge = EDGE * footprint_scale(level_db)
        colour = blend(ACCENT, STAGE, OBJECT_SILENT_FADE) if faded else ACCENT
        opacity = SILENT_PRESENCE_FLOOR + 0.22 if faded else 1.0
        parts = [floor_square(EDGE * FOOTPRINT_MAX_SCALE * 1.9,
                              fill=blend(STAGE, BORDER, 0.55))]
        # The core is filled and the ring is a hairline, so the core can be
        # read against the ring rather than covering it. Neither is the accent:
        # scene::add_object paints the ring theme::MUTED and the core
        # MUTED lerped 0.4 toward the stage, and only the core recedes.
        core_colour = blend(MUTED, STAGE, 0.40)
        if faded:
            core_colour = blend(core_colour, STAGE, OBJECT_SILENT_FADE)
        parts.append(floor_square(core_edge, fill=core_colour))
        parts.append(floor_square(
            ring_edge, stroke=WARNING if ring_highlight else MUTED,
            width=1.8 if ring_highlight else 1.1))
        guide = project((0, hover - EDGE / 2, 0), True)
        base = project((0, 0, 0), True)
        parts.append(stroke_path(
            f"M{fmt(guide[0])} {fmt(guide[1])}L{fmt(base[0])} {fmt(base[1])}",
            MUTED, 0.8, opacity=0.5))
        cube = scene.box((0, hover, 0), (EDGE,) * 3, colour, number, True)
        parts.append(f'<g opacity="{fmt(opacity)}">{cube}</g>'
                     if opacity != 1 else cube)
        if plate is not None:
            top = project((0, hover + EDGE / 2, 0), True)
            parts.append(nameplate(top[0], top[1] - 12, plate[0], plate[1],
                                   level_db=None if plate[1] == "∞" else level_db,
                                   opacity=plate_opacity))
        return f'<g transform="translate({fmt(cx)} {fmt(cy)})">{"".join(parts)}</g>'
    finally:
        scene.SCALE = previous


def yawed_box(centre, size, yaw, colour, *, opacity=None):
    """A box turned about the vertical axis.

    scene.box draws the three faces an axis-aligned box shows this camera. A
    head that turns shows a different three, so the faces are culled against the
    eye instead of assumed, and shaded by orientation rather than by index —
    at yaw zero that reproduces scene.FACES' own 1.00 / 0.88 / 0.76.
    """
    # A bearing of b points at (sin b, 0, -cos b), so a yaw of b has to carry
    # the facing direction (0, 0, -1) to exactly that. The mirrored form turns
    # the head against the object that is supposed to be locked to it.
    c, sn = cos(yaw), sin(yaw)
    turn = lambda v: (v[0] * c - v[2] * sn, v[1], v[0] * sn + v[2] * c)
    axes = [turn((1, 0, 0)), (0, 1, 0), turn((0, 0, 1))]
    faces = []
    for axis, extent in zip(axes, size):
        for direction in (1, -1):
            normal = tuple(direction * v for v in axis)
            if scene.dot(normal, scene.EYE) <= 0:
                continue
            right, up = [a for a in axes if a is not axis]
            centre_of_face = scene.add(centre, scene.multiply(normal, extent / 2))
            half = [s / 2 for a, s in zip(axes, size) if a is not axis]
            corners = [
                scene.add(centre_of_face,
                          scene.add(scene.multiply(right, x * half[0]),
                                    scene.multiply(up, y * half[1])))
                for x, y in ((-1, -1), (1, -1), (1, 1), (-1, 1))
            ]
            tone = (1.0 if axis is axes[1]
                    else 0.76 + 0.12 * (0.5 + 0.5 * scene.dot(normal, scene.RIGHT)))
            faces.append((scene.dot(centre_of_face, scene.EYE), corners, tone))
    parts = []
    for _, corners, tone in sorted(faces):
        fill = scene.faded(blend(colour, INK, 1 - tone), centre)
        points = " ".join(",".join(fmt(v) for v in project(p, True)) for p in corners)
        parts.append(f'<polygon points="{points}" fill="{fill}"/>')
    body = "".join(parts)
    return f'<g opacity="{fmt(opacity)}">{body}</g>' if opacity is not None else body


def top_view(point, *, perspective):
    """Straight down on the room, in the projection the toolbar toggles.

    Returns the screen point and the factor sizes take there. Orthographic
    ignores height entirely, so a cube and the footprint beneath it land on the
    same pixels at their true relative sizes — and a footprint narrower than the
    cube is simply covered. Perspective divides by the distance to the camera,
    which both separates the two and makes the nearer one larger.
    """
    x, y, z = point
    reach = TOP_CAMERA_HEIGHT / (TOP_CAMERA_HEIGHT - y) if perspective else 1.0
    return (TOP_SCALE * x * reach, TOP_SCALE * z * reach, reach)


def meter_track(x, y, w, h, *, level_db, gain_db, peak_db=None, clip=False,
                clip_width=3.0):
    """One row's track, with the four marks app::draw_meter_track paints."""
    parts = [rect(x, y, w, h, fill=HOVER, radius=1)]
    filled = w * fraction(level_db)
    if filled > 0:
        parts.append(rect(x, y, filled, h, fill=ACCENT, radius=1))
    gain_x = x + w * fraction(gain_db)
    parts.append(stroke_path(f"M{fmt(gain_x)} {fmt(y)}V{fmt(y + h)}", MUTED, 1))
    if peak_db is not None:
        peak_x = x + w * fraction(peak_db)
        parts.append(stroke_path(f"M{fmt(peak_x)} {fmt(y)}V{fmt(y + h)}", INK, 1.5))
    if clip:
        parts.append(rect(x + w - clip_width, y, clip_width, h, fill=WARNING))
    return "".join(parts)


# --- The four diagrams ----------------------------------------------------


def build_playback_paths():
    """Where the decoded objects go, and why HpTF exists in one path only."""
    width, height = 880, 440
    body = card(width, height, "Playback paths", "AC-4 / OUTPUT")

    def stage_box(x, y, w, h, title, sub, fill, edge):
        parts = [rect(x, y, w, h, fill=fill, stroke=edge, radius=4)]
        if sub:
            parts.append(text(x + w / 2, y + h / 2 - 1, title, size=11.5,
                              weight=600, anchor="middle", fill=TEXT))
            parts.append(text(x + w / 2, y + h / 2 + 14, sub, size=10,
                              anchor="middle", fill=MUTED))
        else:
            parts.append(text(x + w / 2, y + h / 2 + 4, title, size=11.5,
                              weight=600, anchor="middle", fill=TEXT))
        return "".join(parts)

    # The source column: one decoder, feeding every path. Every box in it is
    # the player's own, so none of them may borrow the device fill.
    body.append(text(97, 118, ".m4a · .mp4 · .ac4", size=10.5, fill=MUTED,
                     anchor="middle"))
    body.append(arrow(97, 134, size=4, down=True))
    for index, (title, sub) in enumerate([("AC-4 decoder", "no system codec"),
                                          ("Objects + LFE", "per-object metadata")]):
        top = 146 + index * 72
        body.append(stage_box(24, top, 146, 48, title, sub, PLAYER_FILL, PLAYER_EDGE))
        if index == 0:
            body.append(arrow(97, top + 60, size=4, down=True))

    lanes = [
        (130, "WINDOWS OBJECT PASSTHROUGH", [
            (200, 244, "Dynamic objects + static LFE", "submitted per render quantum",
             PLAYER_FILL, PLAYER_EDGE),
            (460, 244, "Windows Spatial Audio", "one position and gain per quantum",
             SYSTEM_FILL, SYSTEM_EDGE),
        ], "Spatial-audio endpoint",
         "Needs enough dynamic-object slots: 16 for AC-4 L3, 20 for L4."),
        (232, "SYSTEM SPATIAL AUDIO", [
            (200, 244, "SAF VBAP bed", "7.1.4 · 9.1.6 · 22.2, Apple geometry",
             PLAYER_FILL, PLAYER_EDGE),
            (460, 244, "System spatializer", "the OS does the head tracking",
             SYSTEM_FILL, SYSTEM_EDGE),
        ], "Selected output",
         "The bed leaves as many channels; the final two are formed outside the player."),
        (334, "SAF BINAURAL", [
            (200, 158, "SAF HRTF", "KEMAR or your SOFA", PLAYER_FILL, PLAYER_EDGE),
            (374, 158, "HpTF", "AutoEq · Bass · Tilt", PLAYER_FILL, PLAYER_EDGE),
            (548, 156, "Output limiter", "peak ceiling", PLAYER_FILL, PLAYER_EDGE),
        ], "Any stereo device",
         "The one path that forms the two channels itself — which is why HpTF lives here alone."),
    ]
    for centre, label, boxes, device, note in lanes:
        top = centre - 23
        body.append(text(200, top - 8, label, size=9.5, weight=600, fill=MUTED,
                         spacing=1.1))
        body.append(stroke_path(f"M170 242C186 242 186 {fmt(centre)} 200 {fmt(centre)}",
                                blend(MUTED, STAGE, 0.3), 1.1))
        for x, w, title, sub, fill, edge in boxes:
            body.append(stage_box(x, top, w, 46, title, sub, fill, edge))
            body.append(arrow(x + w + 8, centre, size=4))
        body.append(stage_box(720, top, 136, 46, device, None, DEVICE_FILL,
                              DEVICE_EDGE))
        body.append(text(200, centre + 38, note, size=10, fill=MUTED))

    swatches = [(22, PLAYER_FILL, PLAYER_EDGE, "in the player"),
                (150, SYSTEM_FILL, SYSTEM_EDGE, "operating system"),
                (300, DEVICE_FILL, DEVICE_EDGE, "output device")]
    for x, fill, edge, label in swatches:
        body.append(rect(x, height - 25, 8, 8, fill=fill, stroke=edge, radius=1))
        body.append(text(x + 14, height - 18, label, size=11, fill=TEXT))
    body.append(text(width - 22, height - 18,
                     "Automatic: passthrough on Windows, system spatial on macOS",
                     size=11, fill=MUTED, anchor="end"))
    return "playback-paths.svg", document(
        width, height, "MacinDecode AC-4 Player — playback paths",
        "One AC-4 decoder feeds three output paths. Windows object passthrough "
        "hands dynamic objects and a static LFE to Windows Spatial Audio. System "
        "spatial audio renders a VBAP speaker bed on Apple geometry and hands it "
        "to the system spatializer. SAF binaural renders HRTF, applies headphone "
        "compensation and an output limiter, and reaches any stereo device. The "
        "first two paths leave the player before two channels exist, which is "
        "why headphone compensation is available in the binaural path only.",
        body)


def footprint_corner(scale, decibels, extreme):
    """A footprint's extreme corner on screen, so leaders find it by geometry
    rather than by arithmetic done once in a comment."""
    previous = scene.SCALE
    scene.SCALE = scale
    try:
        half = EDGE * footprint_scale(decibels) / 2
        corners = [(-half, 0, -half), (half, 0, -half), (half, 0, half),
                   (-half, 0, half)]
        points = [project(corner, True) for corner in corners]
        return (max if extreme == "right" else min)(points, key=lambda pt: pt[0])
    finally:
        scene.SCALE = previous


def build_object_footprint():
    """The floor mark's two readings: the gain asked for, the level delivered."""
    width, height = 880, 560
    body = card(width, height, "Object footprint", "LVL / TWO READINGS")

    # Drawn large, because the gap between the ring and the core is the entire
    # reading and a thumbnail hides it.
    art_x, art_y, art_scale = 250, 235, 360
    gain, level = -2.0, -18.0
    body.append(object_art(art_x, art_y, scale=art_scale, gain_db=gain,
                           level_db=level, number=3, plate=("−", "18.0")))
    plate_edge = art_x + 31
    plate_y = art_y - art_scale * 0.355 * 0.9397 - 24
    right = [
        (96, "nameplate", "dBFS, and nothing else", (plate_edge, plate_y + 12)),
        (232, "hairline ring", "the gain the metadata asks for",
         footprint_corner(art_scale, gain, "right")),
    ]
    for y, title, sub, target in right:
        body.append(text(480, y, title, size=12, weight=600, fill=TEXT))
        body.append(text(480, y + 15, sub, size=11, fill=MUTED))
        tip = (art_x + target[0], art_y + target[1]) if y > 150 else target
        body.append(leader([(476, y - 4), (tip[0] + 26, y - 4), tip]))
    core = footprint_corner(art_scale, level, "left")
    body.append(text(196, 300, "filled core", size=12, weight=600, fill=TEXT,
                     anchor="end"))
    body.append(text(196, 315, "the level actually measured", size=11,
                     fill=MUTED, anchor="end"))
    body.append(leader([(202, 296), (art_x + core[0] - 22, 296),
                        (art_x + core[0], art_y + core[1])]))

    # Three cases, read against that anatomy.
    cases = [
        (24, 266, "Sounding", -6.0, -9.0, "−9.0 dBFS", False,
         ["Ring and core close together:", "the object delivers about",
          "what it was given."]),
        (306, 266, "Gained, empty", -3.0, -33.0, "−33.0 dBFS", True,
         ["A wide ring around a small core:", "positioned, gained, and almost",
          "nothing in the track."]),
        (588, 268, "Silent", -3.0, -40.0, "−∞", False,
         ["Below −36.0 dB the core bottoms", "out, and the readout has nothing",
          "finite to report."]),
    ]
    for x, box_width, title, gain_db, level_db, readout, warn, lines in cases:
        body.append(rect(x, 336, box_width, 172, fill=SURFACE, stroke=BORDER,
                         radius=4))
        body.append(text(x + 18, 362, title, size=12, weight=600,
                         fill=WARNING if warn else TEXT))
        # The readout belongs on the header line here: a nameplate is anchored
        # to its cube, and at this size it would climb out of the card.
        body.append(text(x + box_width - 18, 362, readout, size=12, mono=True,
                         anchor="end", fill=MUTED if readout.endswith("∞") else TEXT))
        body.append(object_art(x + box_width / 2, 425, scale=220, gain_db=gain_db,
                               level_db=level_db, number=None,
                               ring_highlight=warn))
        body.append(caption(x + 18, 462, lines, size=10.5, fill=MUTED))
    body.append(text(22, height - 18,
                     "One decibel scale for both, with −36.0 dB at the floor",
                     size=11, fill=TEXT))
    body.append(text(width - 22, height - 18,
                     "The core can never exceed the ring", size=11, fill=MUTED,
                     anchor="end"))
    return "object-footprint.svg", document(
        width, height, "MacinDecode AC-4 Player — the object footprint",
        "With loudness enabled, an object's floor mark carries two readings on "
        "one decibel scale: a hairline ring at the gain the metadata asks for, "
        "and a filled core at the level actually measured. Three cases follow: "
        "a sounding object whose ring and core are close together, an object "
        "with a wide ring around a small core because it was positioned and "
        "gained but carries almost no signal, and a silent object whose core "
        "has bottomed out at the -36 dB floor while its readout shows minus "
        "infinity.", body)


def build_silent_objects():
    """What a persistently silent object loses, and the one thing it keeps."""
    width, height = 880, 430
    body = card(width, height, "Persistently silent objects", "FADE / WHAT STAYS")

    panels = [
        (24, "Sounding", -4.0, -18.3, ("−", "18.3"), 1.0, False,
         ["The plate reads the level, the core", "answers the ring."]),
        (310, "Signal stops", -4.0, -40.0, ("−", "∞"), NAMEPLATE_SILENT_ALPHA, False,
         ["The readout turns to −∞ and the plate", "steps back to a dim that stays readable."]),
        (596, f"After {fmt(SILENCE_HOLD_SECONDS)} s", -4.0, -40.0, None, 0.0, True,
         ["The plate is gone and the cube is a ghost —", "but the gain ring is still on the floor."]),
    ]
    for x, title, gain_db, level_db, plate, plate_opacity, faded, lines in panels:
        body.append(rect(x, 62, 260, 208, fill=SURFACE, stroke=BORDER, radius=4))
        body.append(text(x + 18, 88, title, size=12, weight=600, fill=TEXT))
        body.append(object_art(x + 130, 200, scale=150, gain_db=gain_db,
                               level_db=level_db, number=3, plate=plate,
                               plate_opacity=plate_opacity, faded=faded,
                               ring_highlight=faded))
        body.append(caption(x + 18, 240, lines, size=10, fill=MUTED))
    # The ring is the point of the third panel, so it is named there.
    corner = footprint_corner(150, -4.0, "right")
    body.append(leader([(786, 150), (770, 150), (726 + corner[0], 200 + corner[1])],
                       color=blend(WARNING, STAGE, 0.35)))
    body.append(text(790, 146, "stays", size=10.5, weight=600, fill=WARNING))
    # The count only exists once something has faded, so it belongs to the
    # third panel by name rather than by whatever the loop variable held.
    faded_panel = panels[-1][0]
    body.append(rect(faded_panel + 18, 98, 54, 17, fill=blend(MUTED, STAGE, 0.75),
                     radius=8.5))
    body.append(text(faded_panel + 45, 110, "Silent 1", size=9.5, fill=TEXT,
                     anchor="middle"))

    # The clock underneath, because two of the three panels are a wait.
    body.append(stroke_path("M154 300H726", blend(MUTED, STAGE, 0.35), 1))
    for tick in (154, 440, 726):
        body.append(f'<circle cx="{tick}" cy="300" r="3" fill="{MUTED}"/>')
    body.append(text(297, 292, "the signal stops", size=10, fill=MUTED,
                     anchor="middle"))
    body.append(text(583, 292,
                     f"{fmt(SILENCE_HOLD_SECONDS)} s of silence, "
                     f"then a {fmt(SILENCE_FADE_SECONDS)} s fade",
                     size=10, fill=MUTED, anchor="middle"))

    body.append(rect(24, 322, 832, 52, fill=SURFACE, stroke=BORDER, radius=4))
    body.append(rect(24, 322, 3, 52, fill=MUTED))
    body.append(text(44, 344, "Fade persistently silent objects — off",
                     size=11.5, weight=600, fill=TEXT))
    body.append(text(44, 362,
                     "The cube stays fully drawn and the plate rests at its dim; "
                     "the Silent count disappears. The silence clock keeps running, "
                     "so switching back on finds each object where it is.",
                     size=10.5, fill=MUTED))
    body.append(text(22, height - 18,
                     "The plate dims the instant the readout turns to −∞",
                     size=11, fill=TEXT))
    body.append(text(width - 22, height - 18, "The gain ring never fades",
                     size=11, fill=WARNING, anchor="end"))
    return "silent-objects.svg", document(
        width, height, "MacinDecode AC-4 Player — persistently silent objects",
        "Three stages of a silent object. While sounding, its nameplate reads a "
        "level and the footprint's core answers its gain ring. When the signal "
        "stops the readout turns to minus infinity and the plate steps back to a "
        "dim. After two seconds of silence and a six-tenths-of-a-second fade the "
        "plate is gone and the cube is a ghost, but the gain ring stays on the "
        "floor, because full gain with an empty track is the fault worth seeing. "
        "With fading switched off the cube stays fully drawn instead.", body)


def build_meter_row():
    """One row of the meter bank, and the four marks it carries."""
    width, height = 880, 400
    body = card(width, height, "Meter bank row", "LVL / METER BANK")

    # Spread far enough apart to carry a label each; the app's own scale.
    level, gain, peak = -12.0, -2.0, -6.0
    track_x, track_w, track_y, track_h = 260, 400, 100, 40
    at = lambda decibels: track_x + track_w * fraction(decibels)
    body.append(text(236, 126, "3", size=15, mono=True, fill=TEXT, anchor="end"))
    body.append(meter_track(track_x, track_y, track_w, track_h, level_db=level,
                            gain_db=gain, peak_db=peak, clip=True, clip_width=8))
    body.append(text(730, 126, "−12.0", size=15, mono=True, fill=TEXT, anchor="end"))

    # Above the track, left to right, so no two labels share a column.
    body.append(text(260, 72, "level", size=11.5, weight=600, fill=TEXT))
    body.append(text(260, 86, "the bar fills from the left", size=10.5, fill=MUTED))
    body.append(leader([(350, 92), (350, 100)], dot=False))
    body.append(text(600, 72, "peak", size=11.5, weight=600, fill=TEXT,
                     anchor="end"))
    body.append(text(600, 86, f"holds {fmt(PEAK_HOLD_SECONDS)} s, then slides",
                     size=10.5, fill=MUTED, anchor="end"))
    body.append(leader([(at(peak), 92), (at(peak), 100)], dot=False))
    body.append(text(width - 22, 72, "clip", size=11.5, weight=600, fill=WARNING,
                     anchor="end"))
    body.append(text(width - 22, 86, "a sample reached full scale", size=10.5,
                     fill=MUTED, anchor="end"))
    body.append(leader([(740, 92), (track_x + track_w - 4, 100)],
                       color=blend(WARNING, STAGE, 0.4), dot=False))
    # Below it, the two readings that are not the bar itself.
    body.append(text(236, 164, "number", size=11.5, weight=600, fill=TEXT,
                     anchor="end"))
    body.append(leader([(236, 152), (236, 134)], dot=False))
    body.append(text(at(gain), 164, "gain", size=11.5, weight=600, fill=TEXT,
                     anchor="middle"))
    body.append(text(at(gain), 178, "what the metadata asked for, on the same scale",
                     size=10.5, fill=MUTED, anchor="middle"))
    body.append(leader([(at(gain), 152), (at(gain), 140)], dot=False))

    # The bank as it is actually laid out, with the fault case in it.
    body.append(rect(24, 200, 276, 148, fill=SURFACE, stroke=BORDER, radius=4))
    body.append(text(42, 226, "METER BANK", size=10, weight=600, fill=MUTED,
                     spacing=0.6))
    body.append(rect(222, 214, 58, 20, fill=BACKGROUND, stroke=BORDER, radius=3))
    body.append(text(251, 228, "dBFS", size=10, weight=600, fill=TEXT,
                     anchor="middle"))
    body.append(text(42, 248, "−36.0 → 0.0 dBFS", size=10, fill=MUTED))
    body.append(text(42, 261, "tick = gain · line = peak · red = clip", size=10,
                     fill=MUTED))
    rows = [(1, -6.0, -5.0, -4.0, False, ("−", "6.0"), False),
            (2, -2.0, -1.0, -0.2, True, ("−", "2.0"), False),
            (3, -31.0, -3.0, -29.0, False, ("−", "31.0"), True),
            (4, -40.0, -8.0, -38.0, False, ("−", "∞"), False)]
    for index, (number, row_level, row_gain, row_peak, clip, readout,
                flagged) in enumerate(rows):
        y = 276 + index * 18
        silent = readout[1] == "∞"
        ink = WARNING if clip else MUTED if silent else TEXT
        if flagged:
            body.append(rect(38, y - 2, 252, 18,
                             fill=blend(WARNING, SURFACE, 0.90), radius=3))
        body.append(text(56, y + 9, str(number), size=10, mono=True, anchor="end",
                         fill=ink))
        body.append(meter_track(64, y + 1, 168, 10, level_db=row_level,
                                gain_db=row_gain, peak_db=row_peak, clip=clip,
                                clip_width=3))
        body.append(text(282, y + 9, readout[0] + readout[1], size=10, mono=True,
                         anchor="end", fill=ink))

    body.append(text(364, 226, "Two units, one button", size=11.5, weight=600,
                     fill=TEXT))
    body.append(caption(364, 244, [
        "dBFS — a 30 ms window with ballistics: the reading the scene draws.",
        "LUFS-M — the full 400 ms BS.1770 momentary window, unballistic.",
        "The switch changes what the bar and the number both measure.",
    ], size=10.5, leading=15))
    body.append(text(364, 306, "A bar far short of the tick", size=11.5,
                     weight=600, fill=WARNING))
    body.append(caption(364, 324, [
        "Positioned, gained, and nothing in the track — the same case the",
        "footprint shows, read exactly here instead of by eye.",
    ], size=10.5, leading=15))
    # Stops at the card's edge: a leader reaching in would cross the readouts
    # of the rows above the one it names, so the row is tinted instead.
    body.append(leader([(360, 302), (312, 302), (306, 315)],
                       color=blend(WARNING, STAGE, 0.4)))
    body.append(text(22, height - 18, "One row per object, right of the scene",
                     size=11, fill=TEXT))
    body.append(text(width - 22, height - 18,
                     "Clipping is the one reading that is not weighted at all",
                     size=11, fill=MUTED, anchor="end"))
    return "meter-row.svg", document(
        width, height, "MacinDecode AC-4 Player — a meter bank row",
        "One meter bank row enlarged and labelled: the object number, a track "
        "whose bar fills from the left with the measured level, a tick at the "
        "gain the metadata asked for on the same scale, a peak marker that holds "
        "1.6 seconds and then slides, a red segment at full scale when a sample "
        "clipped, and the readout. Beside it the bank as laid out, with a unit "
        "button switching between dBFS and LUFS-M, and a row whose bar falls far "
        "short of its gain tick — an object positioned and gained with nothing "
        "in its track.", body)


# --- Reference frames ------------------------------------------------------
# The listener figure faces -Z, so a bearing b points at (sin b, 0, -cos b):
# zero is straight ahead and positive turns toward the listener's right.
FLOOR_Y = -0.70
BEARING_RADIUS = 0.62


def bearing_point(bearing, radius, height=FLOOR_Y):
    return (radius * sin(bearing), height, -radius * cos(bearing))


def floor_grid(half=1.0, cells=8):
    parts = [f'<polygon points="' + " ".join(
        ",".join(fmt(v) for v in project(p, True)) for p in
        [(-half, FLOOR_Y, -half), (half, FLOOR_Y, -half),
         (half, FLOOR_Y, half), (-half, FLOOR_Y, half)])
        + f'" fill="{blend(STAGE, SURFACE, 0.6)}"/>']
    for step in range(cells + 1):
        v = -half + 2 * half * step / cells
        heavy = step in (0, cells, cells // 2)
        colour = blend(MUTED, STAGE, 0.55 if heavy else 0.78)
        for a, b in (((v, FLOOR_Y, -half), (v, FLOOR_Y, half)),
                     ((-half, FLOOR_Y, v), (half, FLOOR_Y, v))):
            pa, pb = project(a, True), project(b, True)
            parts.append(stroke_path(f"M{fmt(pa[0])} {fmt(pa[1])}L{fmt(pb[0])} {fmt(pb[1])}",
                                     colour, 1.0 if heavy else 0.7))
    return "".join(parts)


def floor_arc(start, end, radius, colour, width=1.4, dash=None):
    steps = max(8, int(abs(end - start) * 12))
    points = [project(bearing_point(start + (end - start) * i / steps, radius), True)
              for i in range(steps + 1)]
    d = "M" + "L".join(f"{fmt(x)} {fmt(y)}" for x, y in points)
    return stroke_path(d, colour, width, dash=dash)


def floor_ray(bearing, radius, colour, width=1.0, dash=None):
    a, b = project(bearing_point(bearing, 0.06), True), project(bearing_point(bearing, radius), True)
    return stroke_path(f"M{fmt(a[0])} {fmt(a[1])}L{fmt(b[0])} {fmt(b[1])}", colour, width, dash=dash)


def listener_body():
    """Everything head tracking does not move, so it is drawn once."""
    skin = blend(MUTED, TEXT, 0.22)
    cloth = blend(MUTED, INK, 0.5)
    body = [((-.05, -.55, 0), (.1, .3, .1), cloth), ((.05, -.55, 0), (.1, .3, .1), cloth),
            ((0, -.25, 0), (.2, .3, .1), cloth),
            ((-.15, -.25, 0), (.1, .3, .1), skin), ((.15, -.25, 0), (.1, .3, .1), skin)]
    return "".join(scene.box(c, size, colour, None, True)
                   for c, size, colour in sorted(body,
                                                 key=lambda part: scene.dot(part[0], scene.EYE)))


def listener_head(yaw):
    """The only part a head pose turns."""
    skin = blend(MUTED, TEXT, 0.22)
    parts = [yawed_box((0, 0, 0), (.225,) * 3, yaw, blend(skin, INK, 0.16))]
    turn = lambda x, z: (x * cos(yaw) - z * sin(yaw), x * sin(yaw) + z * cos(yaw))
    for x, z, size, colour in ((-.0425, -.125, .0325, blend(INK, TEXT, 0.2)),
                               (.0425, -.125, .0325, blend(INK, TEXT, 0.2)),
                               (0, -.13125, .045, ACCENT)):
        tx, tz = turn(x, z)
        parts.append(yawed_box((tx, .0225 if size < .04 else -.04, tz),
                               (size,) * 3, yaw, colour))
    return "".join(parts)


def listener(yaw):
    return listener_body() + listener_head(yaw)


def frame_panel(x, y, width, height, title, yaw, objects, note, scale=112):
    """One room, one head angle, and what each object keeps constant."""
    parts = [rect(x, y, width, height, fill=SURFACE, stroke=BORDER, radius=4),
             text(x + 18, y + 26, title, size=12.5, weight=600, fill=TEXT)]
    previous, scene.SCALE = scene.SCALE, scale
    art = [floor_grid()]
    art.append(floor_ray(yaw, 0.95, blend(MUTED, STAGE, 0.35), 1.1, dash="4 3"))
    for obj in objects:
        world = obj["world"]
        art.append(floor_arc(yaw, world, BEARING_RADIUS, obj["ink"], 1.6))
        art.append(floor_ray(world, BEARING_RADIUS + 0.06, blend(obj["ink"], STAGE, 0.5), 0.9))
        if obj.get("ghost") is not None:
            art.append(floor_square_at(obj["ghost"], obj["radius"], dash="3 3"))
            art.append(floor_arc(obj["ghost"], world, obj["radius"],
                                 blend(obj["ink"], STAGE, 0.35), 1.2, dash="3 3"))
            tip = project(bearing_point(world + (obj["ghost"] - world) * 0.10,
                                        obj["radius"]), True)
            head_of = project(bearing_point(world, obj["radius"]), True)
            art.append(stroke_path(
                f"M{fmt(tip[0])} {fmt(tip[1])}L{fmt(head_of[0])} {fmt(head_of[1])}",
                blend(obj["ink"], STAGE, 0.2), 2.2))
    art.append(listener(yaw))
    for obj in sorted(objects, key=lambda o: scene.dot(
            bearing_point(o["world"], o["radius"], o["height"]), scene.EYE)):
        point = bearing_point(obj["world"], obj["radius"], obj["height"])
        art.append(floor_square_at(obj["world"], obj["radius"]))
        a = project((point[0], obj["height"] - EDGE / 2, point[2]), True)
        b = project((point[0], FLOOR_Y, point[2]), True)
        art.append(stroke_path(f"M{fmt(a[0])} {fmt(a[1])}L{fmt(b[0])} {fmt(b[1])}",
                               MUTED, 0.8, opacity=0.5))
        cube = scene.box(point, (EDGE,) * 3, ACCENT, obj["number"], True)
        # Head-locked objects carry a white element number, as number_colour does.
        art.append(f'<g class="w">{cube}</g>' if obj["white"] else cube)
    parts.append(f'<g transform="translate({fmt(x + width / 2)} {fmt(y + height * 0.46)})">'
                 + "".join(art) + '</g>')
    # The angle each arc subtends, written at the arc's own midpoint.
    for obj in objects:
        middle = project(bearing_point((yaw + obj["world"]) / 2, BEARING_RADIUS + 0.20), True)
        degrees = round(abs(obj["world"] - yaw) * 180 / pi)
        parts.append(text(x + width / 2 + middle[0], y + height * 0.46 + middle[1] + 4,
                          f"{degrees}°", size=12, weight=600, fill=obj["ink"],
                          anchor="middle"))
    scene.SCALE = previous
    parts.append(text(x + 18, y + height - 16, note, size=10.5, fill=MUTED))
    return "".join(parts)


def floor_square_local():
    """The footprint's shape alone, centred on the origin, for a group whose own
    transform already carries the floor position — adding the floor's screen
    offset a second time drops it a room's height below where it belongs."""
    half = EDGE * 1.30 / 2
    base = project((0, FLOOR_Y, 0), True)
    corners = [(-half, FLOOR_Y, -half), (half, FLOOR_Y, -half),
               (half, FLOOR_Y, half), (-half, FLOOR_Y, half)]
    points = " ".join(",".join(fmt(v - b) for v, b in zip(project(c, True), base))
                      for c in corners)
    return f'<polygon points="{points}" fill="{blend(MUTED, STAGE, 0.40)}"/>'


def floor_square_at(bearing, radius, dash=None):
    point = bearing_point(bearing, radius)
    half = EDGE * 1.30 / 2
    corners = [(point[0] - half, FLOOR_Y, point[2] - half), (point[0] + half, FLOOR_Y, point[2] - half),
               (point[0] + half, FLOOR_Y, point[2] + half), (point[0] - half, FLOOR_Y, point[2] + half)]
    pts = " ".join(",".join(fmt(v) for v in project(p, True)) for p in corners)
    if dash:
        return (f'<polygon points="{pts}" fill="none" stroke="{MUTED}" '
                f'stroke-width="1" stroke-dasharray="{dash}"/>')
    return f'<polygon points="{pts}" fill="{blend(MUTED, STAGE, 0.40)}"/>'


def build_reference_frames():
    """What each reference frame keeps constant when the listener turns."""
    width, height = 880, 570
    body = card(width, height, "Reference frames", "IDs / SCENE vs HEAD")
    yaw = radians(45)
    # Bearings measured from straight ahead, positive toward the listener's right.
    scene_rel = dict(number=1, radius=.80, height=.12, ink=INK, white=False)
    head_lock = dict(number=2, radius=.90, height=.66, ink=ACCENT, white=True)
    panels = [
        (24, "Head forward", 0.0, radians(100), radians(-75), None,
         "The listener faces the dashed ray; each arc is a bearing from it."),
        (450, "Head turned 45° to the right", yaw, radians(100), radians(-75) + yaw,
         radians(-75),
         "Dashed: where object 2 was. It swung 45° with the head; object 1 did not move."),
    ]
    for x, title, head, world_scene, world_head, ghost, note in panels:
        objects = [dict(scene_rel, world=world_scene),
                   dict(head_lock, world=world_head, ghost=ghost)]
        body.append(frame_panel(x, 62, 406, 300, title, head, objects, note))

    rows = [
        (376, INK, False, "Scene relative", "1", "same cell", "100° → 55°",
         "Keeps its place and gives up its bearing —",
         "this is what head tracking sounds like: the room stays put while you turn."),
        (448, ACCENT, True, "Head locked", "2", "swung 45°", "75° → 75°",
         "Keeps its bearing and gives up its place —",
         "it sounds unchanged as you turn, and only the picture moves."),
    ]
    for y, ink, white, name, number, place, bearing, lead, tail in rows:
        body.append(rect(24, y, 832, 62, fill=SURFACE, stroke=BORDER, radius=4))
        body.append(rect(24, y, 3, 62, fill=ink))
        body.append(rect(42, y + 20, 22, 22, fill=ACCENT, radius=3))
        body.append(text(53, y + 36, number, size=12, weight=600, anchor="middle",
                         fill=SURFACE if white else INK))
        body.append(text(76, y + 26, name, size=12, weight=600, fill=ink))
        body.append(text(76, y + 45, f"room: {place}  ·  bearing: {bearing}",
                         size=10.5, mono=True, fill=TEXT))
        body.append(text(370, y + 26, lead, size=11, weight=600, fill=TEXT))
        body.append(text(370, y + 45, tail, size=10.5, fill=MUTED))

    body.append(text(22, height - 18,
                     "Black number = scene relative · white number = head locked",
                     size=11, fill=TEXT))
    body.append(text(width - 22, height - 18,
                     "System spatial audio cannot keep per-object head locking",
                     size=11, fill=MUTED, anchor="end"))
    return "reference-frames.svg", document(
        width, height, "MacinDecode AC-4 Player — reference frames",
        "The same two objects with the listener facing forward and then turned "
        "45 degrees to the right. A scene-relative object keeps its place in the "
        "room, so its bearing to the listener changes from 100 to 55 degrees — "
        "which is what head tracking sounds like, the room staying put while you "
        "turn. A head-locked object keeps its bearing at 75 degrees, so its place "
        "in the room swings around with the head: it sounds unchanged, and only "
        "the picture moves. The player marks the first with a black element "
        "number and the second with a white one.", body)


def top_box(centre, edge, number, *, perspective):
    """A cube through the straight-down camera, with the faces it really shows.

    Orthographic keeps every corner's size, so only the top face is drawn and it
    covers the footprint exactly. Perspective gives each corner its own reach, so
    an off-axis cube leans and its inward sides come into view.
    """
    cx, cy, cz = centre
    half = edge / 2
    corner = lambda sx, sy, sz: top_view((cx + sx * half, cy + sy * half, cz + sz * half),
                                         perspective=perspective)[:2]
    top = [corner(-1, 1, -1), corner(1, 1, -1), corner(1, 1, 1), corner(-1, 1, 1)]
    sides = []
    for axis, normal in ((0, 1), (0, -1), (2, 1), (2, -1)):
        # A face is seen when its outward normal points back toward the camera.
        offset = (cx if axis == 0 else cz) + normal * half
        if offset * normal >= 0:
            continue
        if axis == 0:
            quad = [corner(normal, 1, -1), corner(normal, 1, 1),
                    corner(normal, -1, 1), corner(normal, -1, -1)]
        else:
            quad = [corner(-1, 1, normal), corner(1, 1, normal),
                    corner(1, -1, normal), corner(-1, -1, normal)]
        sides.append(quad)
    parts = []
    for quad in sides:
        points = " ".join(f"{fmt(x)},{fmt(y)}" for x, y in quad)
        parts.append(f'<polygon points="{points}" fill="{blend(ACCENT, INK, 0.22)}"/>')
    points = " ".join(f"{fmt(x)},{fmt(y)}" for x, y in top)
    parts.append(f'<polygon points="{points}" fill="{ACCENT}"/>')
    # The element number, as the seven segments the player prints on every face.
    # The top face is a plane at one height, so its projection is affine and the
    # corners can be interpolated directly.
    def on_face(u, v):
        a = (top[0][0] + (top[1][0] - top[0][0]) * u, top[0][1] + (top[1][1] - top[0][1]) * u)
        b = (top[3][0] + (top[2][0] - top[3][0]) * u, top[3][1] + (top[2][1] - top[3][1]) * u)
        return (a[0] + (b[0] - a[0]) * v, a[1] + (b[1] - a[1]) * v)
    for bit, (start, end) in enumerate(scene.SEGMENTS):
        if not scene.DIGITS[number] & (1 << bit):
            continue
        shift = -.35 if number == 1 else 0
        p0 = on_face((start[0] + shift) * .72 + .5, .5 - start[1] * .72)
        p1 = on_face((end[0] + shift) * .72 + .5, .5 - end[1] * .72)
        parts.append(f'<path d="M{fmt(p0[0])} {fmt(p0[1])}L{fmt(p1[0])} {fmt(p1[1])}" '
                     f'fill="none" stroke="{INK}" stroke-width="1.15"/>')
    return "".join(parts)


def build_projection_modes():
    """Why the projection toggle exists, rather than what it looks like."""
    width, height = 880, 560
    body = card(width, height, "Projection", "ORTHO / PERSP")

    # Levels either side of the crossover: below -18.8 dB the footprint is
    # narrower than the cube, so a straight-down orthographic view covers it.
    objects = [(1, -3.0, (-.40, .46, -.30)), (2, -14.0, (.44, .52, -.28)),
               (3, -26.0, (-.38, .50, .32)), (4, -33.0, (.42, .44, .30))]
    square = lambda cx, cy, side, **kw: rect(cx - side / 2, cy - side / 2,
                                             side, side, **kw)
    for index, (label, perspective, note) in enumerate([
            ("ORTHO", False, "Height is ignored, so each cube covers its own footprint."),
            ("PERSP", True, "Distance separates them, and the footprints come back.")]):
        x = 24 + index * 426
        body.append(rect(x, 62, 406, 320, fill=SURFACE, stroke=BORDER, radius=4))
        art = []
        for step in range(11):
            v = -.65 + 1.3 * step / 10
            for a, b in (((v, FLOOR_Y, -.65), (v, FLOOR_Y, .65)),
                         ((-.65, FLOOR_Y, v), (.65, FLOOR_Y, v))):
                pa, pb = top_view(a, perspective=perspective), top_view(b, perspective=perspective)
                art.append(stroke_path(f"M{fmt(pa[0])} {fmt(pa[1])}L{fmt(pb[0])} {fmt(pb[1])}",
                                       blend(MUTED, STAGE, .70), .8))
        for number, decibels, (px, py, pz) in objects:
            fx, fy, freach = top_view((px, FLOOR_Y, pz), perspective=perspective)
            art.append(square(fx, fy, EDGE * footprint_scale(decibels) * freach * TOP_SCALE,
                              fill="none", stroke=MUTED, width=1.3))
            art.append(top_box((px, py, pz), EDGE, number, perspective=perspective))
        # The panel is a viewport onto the floor, so the floor is cut by it
        # rather than shrunk to fit inside it.
        body.append(f'<clipPath id="p{index}">'
                    f'<rect x="{fmt(x + 1)}" y="63" width="404" height="286" rx="4"/>'
                    '</clipPath>')
        body.append(f'<g clip-path="url(#p{index})">'
                    f'<g transform="translate({fmt(x + 203)} {fmt(206)})">'
                    + "".join(art) + '</g></g>')
        body.append(rect(x + 330, 76, 58, 20, fill=BACKGROUND, stroke=BORDER, radius=3))
        body.append(text(x + 359, 90, label, size=10, weight=600, fill=TEXT,
                         anchor="middle"))
        body.append(text(x + 18, 368, note, size=10.5, fill=MUTED))

    body.append(rect(24, 396, 832, 116, fill=SURFACE, stroke=BORDER, radius=4))
    body.append(text(42, 422, "The footprint carries gain by growing, so a quiet "
                     "object's is narrower than its own cube", size=11.5,
                     weight=600, fill=TEXT))
    columns = [("1", "−3 dB", "1.50×", True), ("2", "−14 dB", "1.15×", True),
               ("3", "−26 dB", "0.77×", False), ("4", "−33 dB", "0.55×", False)]
    for index, (number, level, ratio, visible) in enumerate(columns):
        cx = 42 + index * 200
        body.append(rect(cx, 440, 18, 18, fill=ACCENT, radius=2))
        body.append(text(cx + 9, 453, number, size=10, weight=600, fill=SURFACE,
                         anchor="middle"))
        body.append(text(cx + 26, 446, f"{level} → {ratio}", size=10.5, fill=TEXT))
        body.append(text(cx + 26, 460, "visible either way" if visible
                         else "hidden in ORTHO", size=10.5,
                         fill=MUTED if visible else WARNING))
    body.append(text(42, 490, "The crossover is −18.8 dB. Below it only perspective "
                     "separates the two — by more the further the object sits from "
                     "the view axis, so an object near it stays covered.",
                     size=10.5, fill=MUTED))

    body.append(text(22, height - 18, "Straight down on the same four objects",
                     size=11, fill=TEXT))
    body.append(text(width - 22, height - 18,
                     "Parallel edges stay parallel in ORTHO and converge in PERSP",
                     size=11, fill=MUTED, anchor="end"))
    return "projection-modes.svg", document(
        width, height, "MacinDecode AC-4 Player — orthographic and perspective",
        "The same four objects seen straight down in both projections. The floor "
        "footprint carries gain by growing, so an object quieter than -18.8 dB "
        "has a footprint narrower than its own cube. An orthographic straight-down "
        "view puts a cube and the footprint beneath it on exactly the same pixels, "
        "so those footprints are hidden entirely. Perspective divides by distance "
        "to the camera, sliding each cube off its footprint — by more the further "
        "the object sits from the view axis — so the footprints reappear.", body)


# --- Head tracking, the one diagram that has to move -----------------------
# The distinction only exists while the head turns, so this is the only thing
# here that animates. Poses are stepped: everything but the head is a pure
# translation and rides a transform animation, while a turning cube changes
# shape and needs its own geometry per pose.
TRACK_POSES, TRACK_SECONDS, TRACK_SWEEP = 72, 6.0, radians(45)
# Each pose stays up for longer than its own slice. At exactly one slice the
# outgoing pose can reach zero a frame before the incoming one reaches one, and
# the gap shows as the background flashing through; overlapping costs a frame of
# two nearly identical heads drawn on top of each other, which is invisible.
TRACK_OVERLAP = 1.25


def build_head_tracking():
    """One arc stays rigid while the other breathes."""
    width, height = 880, 470
    scale = 150
    previous, scene.SCALE = scene.SCALE, scale
    try:
        body = card(width, height, "Head tracking", "SCENE vs HEAD / LIVE")
        origin = (width / 2, 198)
        fixed, locked = radians(105), radians(-70)
        radius_fixed, radius_locked = 0.80, 0.86
        height_fixed, height_locked = 0.12, 0.60
        yaws = [TRACK_SWEEP * sin(2 * pi * k / TRACK_POSES) for k in range(TRACK_POSES)]

        style = [f'.k{{animation-duration:{fmt(TRACK_SECONDS)}s;'
                 'animation-iteration-count:infinite;animation-name:pose;'
                 'animation-timing-function:steps(1,end);animation-delay:var(--d)}',
                 f'@keyframes pose{{0%{{opacity:1}}'
                 f'{fmt(100 * TRACK_OVERLAP / TRACK_POSES)}%{{opacity:0}}}}',
                 f'.t{{animation:swing {fmt(TRACK_SECONDS)}s linear infinite}}']
        for name, point_of in (("swing", lambda y: bearing_point(locked + y,
                                                                 radius_locked)),):
            frames = []
            for k in range(TRACK_POSES + 1):
                x, y = project(point_of(yaws[k % TRACK_POSES]), True)
                frames.append(f"{fmt(k / TRACK_POSES * 100)}%{{transform:"
                              f"translate({fmt(x)}px,{fmt(y)}px)}}")
            style.append(f"@keyframes {name}{{" + "".join(frames) + "}")
        for k in range(TRACK_POSES):
            # A negative delay runs the animation forward, so pose k has to be
            # held back by a whole loop minus its own slice. Delaying by the
            # slice alone plays the poses backwards, which desynchronises them
            # from the transform animation the moving object rides.
            style.append(f'.k{k}{{--d:-'
                         f'{fmt((TRACK_POSES - k) * TRACK_SECONDS / TRACK_POSES)}s}}')
        style.append("@media (prefers-reduced-motion:reduce){.k,.t{animation:none}}")
        body.insert(0, "<style>" + "".join(style) + "</style>")

        art = [floor_grid()]
        # The scene-relative object never moves, so it is drawn once.
        static_body = listener_body()
        fixed_point = bearing_point(fixed, radius_fixed, height_fixed)
        art.append(floor_square_at(fixed, radius_fixed))
        a = project((fixed_point[0], height_fixed - EDGE / 2, fixed_point[2]), True)
        b = project((fixed_point[0], FLOOR_Y, fixed_point[2]), True)
        art.append(stroke_path(f"M{fmt(a[0])} {fmt(a[1])}L{fmt(b[0])} {fmt(b[1])}",
                               MUTED, 0.8, opacity=0.5))
        art.append(static_body)
        # Everything that turns, one group per pose, one visible at a time.
        for k, yaw in enumerate(yaws):
            pose = [floor_ray(yaw, 0.95, blend(MUTED, STAGE, 0.35), 1.1, dash="4 3"),
                    floor_arc(yaw, fixed, BEARING_RADIUS, INK, 1.6),
                    floor_arc(yaw, locked + yaw, BEARING_RADIUS + 0.14, ACCENT, 1.6),
                    listener_head(yaw)]
            opacity = "" if k == 0 else ' opacity="0"'
            art.append(f'<g class="k k{k}"{opacity}>{"".join(pose)}</g>')
        art.append(scene.box(fixed_point, (EDGE,) * 3, ACCENT, 1, True))
        # One animated group carries everything that swings: the footprint on the
        # floor, the drop line, and the cube lifted by the constant screen
        # distance between its height and the floor.
        lift = project((0, height_locked, 0), True)[1] - project((0, FLOOR_Y, 0), True)[1]
        lift_top = project((0, height_locked - EDGE / 2, 0), True)[1] \
            - project((0, FLOOR_Y, 0), True)[1]
        start = project(bearing_point(locked, radius_locked), True)
        swinging = [floor_square_local(),
                    stroke_path(f"M0 {fmt(lift_top)}L0 0", MUTED, 0.8, opacity=0.5),
                    f'<g class="w" transform="translate(0 {fmt(lift)})">'
                    f'{scene.box((0, 0, 0), (EDGE,) * 3, ACCENT, 2, True)}</g>']
        art.append(f'<g class="t" transform="translate({fmt(start[0])} {fmt(start[1])})">'
                   + "".join(swinging) + '</g>')
        body.append(f'<g transform="translate({fmt(origin[0])} {fmt(origin[1])})">'
                    + "".join(art) + '</g>')

        body.append(text(22, 74, "1  scene relative", size=11.5, weight=600, fill=INK))
        body.append(text(22, 90, "its arc opens and closes: the bearing you hear changes",
                         size=10.5, fill=MUTED))
        body.append(text(width - 22, 74, "2  head locked", size=11.5, weight=600,
                         fill=ACCENT, anchor="end"))
        body.append(text(width - 22, 90, "its arc is rigid: the bearing you hear never changes",
                         size=10.5, fill=MUTED, anchor="end"))
        body.append(text(22, height - 18,
                         "The one that moves on screen is the one that does not move in your ears",
                         size=11, fill=TEXT))
        body.append(text(width - 22, height - 18, "Reduced motion shows the head forward",
                         size=11, fill=MUTED, anchor="end"))
        return "head-tracking.svg", document(
            width, height, "MacinDecode AC-4 Player — head tracking",
            "The listener turns from side to side. A scene-relative object stays "
            "where it is in the room, and the arc from the listener's facing "
            "direction to it opens and closes — the bearing you hear changes, "
            "which is what head tracking sounds like. A head-locked object swings "
            "around the room with the head, and its arc keeps exactly the same "
            "angle — the bearing you hear never changes, so it sounds fixed while "
            "only the picture moves. Reduced-motion preferences show the head "
            "facing forward.", body)
    finally:
        scene.SCALE = previous


BUILDERS = {
    "playback-paths": build_playback_paths,
    "reference-frames": build_reference_frames,
    "projection-modes": build_projection_modes,
    "head-tracking": build_head_tracking,
    "object-footprint": build_object_footprint,
    "silent-objects": build_silent_objects,
    "meter-row": build_meter_row,
}


def optimize(svg, parser):
    npm = shutil.which("npm")
    if npm is None:
        parser.error("Node.js/npm is required for SVGO; use --unoptimized")
    result = subprocess.run(
        [npm, "exec", "--yes", "--package=svgo@4.0.0", "--", "svgo", "--quiet",
         "--config", str(ROOT / "scripts/readme-svgo.config.mjs"),
         "--input=-", "--output=-"],
        input=svg, text=True, encoding="utf-8", capture_output=True, cwd=ROOT,
        check=True,
    )
    return result.stdout


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, default=OUTPUT_DIR)
    parser.add_argument("--only", choices=sorted(BUILDERS), action="append",
                        help="Build one diagram; repeatable")
    parser.add_argument("--unoptimized", action="store_true",
                        help="Skip SVGO; only Python is required")
    args = parser.parse_args()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    for name in args.only or sorted(BUILDERS):
        filename, svg = BUILDERS[name]()
        if not args.unoptimized:
            svg = optimize(svg, parser)
        path = args.output_dir / filename
        path.write_text(svg, encoding="utf-8")
        print(f"{path}: {path.stat().st_size:,} bytes")
