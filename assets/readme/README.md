# README artwork

Five SVGs are shared by the Chinese and English READMEs and by
[`docs/MANUAL.md`](../../docs/MANUAL.md) and its English twin: one animated
scene, and four static diagrams that annotate what the player draws. All of
them follow the palette and geometry in `src/theme.rs` and `src/scene3d/`.

Every one is self-contained: SVG geometry, local fragment references, and CSS
only. Embed them as Markdown images; GitHub does not accept raw inline SVG
markup in a README. No JavaScript, external fonts, or network assets are used.

## The scene

`spatial-orbit.svg` shows six illustrative objects rather than a recording of
decoded audio. Two orbit among four stationary objects, so individual trails
remain easy to distinguish.

The four-second loop includes floor projections and forty position samples at
40 ms intervals, matching the player's 1.6-second history. The sample cubes are
30% of an object's edge length. Their positions update on the sample clock;
objects move continuously between samples. Solid face colors fade toward the
stage color before shading, with 45% color weight for the smaller floor marks.
Object numbers use the same near-black ink as the player (`#332a1f`).
Matching front and back layers let objects pass behind the listener.

`prefers-reduced-motion: reduce` disables the animations. Base transforms and
visibility retain the starting scene for reduced-motion users and SVG viewers
that support CSS but do not run animations.

## The diagrams

| File | What it answers |
| --- | --- |
| `playback-paths.svg` | Where the decoded objects go in each mode, and why HpTF exists in the binaural path alone |
| `object-footprint.svg` | The floor mark's two readings — the gain ring and the level core — and the three cases they produce |
| `silent-objects.svg` | What a persistently silent object loses, and the gain ring it keeps |
| `meter-row.svg` | One meter bank row's four marks, and the bank they sit in |

They are still pictures rather than animations, and they carry labels only: the
prose that explains them lives in the manual beside each image, where it can be
translated and searched. Their frame, palette and isometric camera are the
scene's, so the five read as one set.

`generate-readme-diagrams.py` imports the palette, the camera and the voxel
geometry from `generate-readme-scene.py` rather than restating them, and
transcribes the constants it annotates — the silence floor, the footprint
scales, the nameplate dim, the silence hold, the peak hold — from
`src/scene3d/params.rs` and `src/app.rs` into one block at the top. Change one
of those in the player and this block has to follow; it exists so that the
drift is visible rather than buried in coordinates. Leader lines aim at
geometry the generator projects, never at coordinates measured by hand.

## Regenerating

From the repository root, with Python and Node.js/npm:

```sh
python3 scripts/generate-readme-scene.py
python3 scripts/generate-readme-diagrams.py
```

`generate-readme-diagrams.py --only meter-row` rebuilds one diagram. Both
generators invoke SVGO **4.0.0** through `npm exec`, using
[`scripts/readme-svgo.config.mjs`](../../scripts/readme-svgo.config.mjs). The
first invocation may download that tool. This is only needed to regenerate the
artwork; viewing the README and building the player need no Node dependencies.
To inspect the intermediate SVG using Python alone, pass `--unoptimized`:

```sh
python3 scripts/generate-readme-scene.py --unoptimized --output target/readme-scene.svg
python3 scripts/generate-readme-diagrams.py --unoptimized --output-dir target/
```

The cube faces and floor marks share geometry, with CSS palettes for the forty
sample ages. Phase classes share animation delays; each airborne instance runs
its position and front/back opacity animations on the same element. Number
strokes share styling while keeping their original draw order and compositing.
The optimizer preserves initially transparent layers, animation transforms,
custom properties and separate strokes. Do not substitute SVGO's default
configuration: it can delete hidden animation layers and change antialiased
intersections when merging paths.
