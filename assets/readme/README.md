# README artwork

Eight SVGs are shared by the Chinese and English READMEs and by
[`docs/MANUAL.md`](../../docs/MANUAL.md) and its English twin: the animated
scene, six static diagrams that annotate what the player draws, and one more
animation for the single thing a still picture cannot show. All of them follow
the palette and geometry in `src/theme.rs` and `src/scene3d/`.

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

The orbit's steps are deliberately not evenly spaced in angle. A circle seen
from twenty degrees projects to a narrow ellipse, so at a constant angular rate
the object crosses the screen seven times faster at the sides than at the turns
— and a trail sampled on a fixed clock then fuses into a rope at one end of that
range and comes apart into separate dots at the other, twice per revolution.
That pumping is easier to see than to name, and it showed up where the page gave
the image less of a frame budget while the same file looked fine opened on its
own. `SPEED_EVENNESS` pulls the angles toward constant screen speed, holding
every step between 4.7 and 8.6 px against a 9.9 px cube. It stops short of fully
even so the gap between marks still reads as speed, varying 1.5x rather than 7x.
Slowing the whole orbit is not a substitute: it moves both ends of the range down
together, fuses more of the trail, and costs the motion its life.

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
| `reference-frames.svg` | What scene-relative and head-locked each keep constant when the listener turns |
| `projection-modes.svg` | Why the projection toggle exists: which footprints a straight-down orthographic view hides |
| `head-tracking.svg` | The same distinction in motion — one bearing arc rigid, the other breathing |

They carry labels only: the prose that explains them lives in the manual beside
each image, where it can be translated and searched. Their frame, palette and
isometric camera are the scene's, so all eight read as one set.

`head-tracking.svg` is the one diagram that moves, because the distinction it
draws only exists while the head turns — two objects standing still are
identical whatever frame they are in. It is also the reason the reference-frame
pictures measure a **bearing** rather than only a position: scene relative keeps
its place and gives up its bearing, head locked keeps its bearing and gives up
its place, and the bearing is the part you hear. A diagram that only asked
"which one moved" would teach the inversion backwards.

Its poses are stepped: everything but the head is a pure translation and rides a
transform animation, while a turning cube changes shape and needs its own
geometry per pose. A pose's negative delay is a whole loop minus its own slice —
delaying by the slice alone plays the poses backwards, silently desynchronising
them from the transform animation.

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
