# README artwork

Eleven SVGs illustrate the Chinese and English READMEs,
[`docs/MANUAL.md`](../../docs/MANUAL.md) and its English twin: the animated
scene, nine static diagrams that annotate what the player draws, and one more
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
| `trail-jumps.svg` | What a trail's spacing says, and what the hollow marks add where the path is broken |
| `reference-frames.svg` | What scene-relative and head-locked each keep constant when the listener turns |
| `projection-modes.svg` | Why the projection toggle exists: which footprints a straight-down orthographic view hides |
| | Its cubes are boxes, not squares: perspective gives each corner its own reach, so an off-axis cube leans and its inward faces come into view, while orthographic shows the top face alone and covers the footprint exactly. Element numbers are the player's seven segments, laid on the top face. Its objects sit low in the room, and that is what makes the boxes legible: side faces are as tall as a cube is thick, while the gap to the footprint is as wide as the cube is high above the floor, so the two grow together as the camera comes in. High in the room that ratio is about a tenth and the side faces land under two pixels; low it is nearer a third, which buys a much closer camera at the same separation, and with the faces that size they carry `params::TONE_LEFT` and `TONE_RIGHT` unexaggerated — one shading rule across every picture. Nothing distorts from that — every point at one height shares one reach, so the floor grid stays square however near the camera comes, and orthographic is scaled at the floor so both panels draw one room at one size. |
| `head-tracking.svg` | The same distinction in motion — one bearing arc rigid, the other breathing |
| `magnetic-grid.svg` | What the Magnetic page's coverage grid shows — its columns and rows, the target, the latest reading — and why its fill stops deepening |
| `magnetic-turn.svg` | Why that grid's square moves against the headset, so the page's words are the thing to follow |

They carry labels only: the prose that explains them lives in the manual beside
each image, where it can be translated and searched. Their frame, palette and
isometric camera are the scene's, so all eleven read as one set.

The two magnetic pictures show one moment of a sweep, so the second can follow
the first: the latest reading 50 degrees right of forward and 15 below the
horizon, the nearest cell not read yet one column to its right, and "Turn it to
its left" as the page would word it there — checked against
`coverage::towards`, since a target and an instruction that disagree would teach
the one thing the pictures exist to correct. The turn is drawn in the
reference-frames room because the field is scene relative: it keeps its place
and gives up its bearing, and the page's square shows the bearing. The
listener's head stands in for the headset, whose forward is what the grid is
measured from; the card's footer says the headset is really turned in the
hands.

`head-tracking.svg` is the one diagram that moves, because the distinction it
draws only exists while the head turns — two objects standing still are
identical whatever frame they are in. It is also the reason the reference-frame
pictures measure a **bearing** rather than only a position: scene relative keeps
its place and gives up its bearing, head locked keeps its bearing and gives up
its place, and the bearing is the part you hear. A diagram that only asked
"which one moved" would teach the inversion backwards.

Its poses are stepped: everything but the head is a pure translation and rides a
transform animation, while a turning cube changes shape and needs its own
geometry per pose. Three things there are easy to get wrong and impossible to
see in a single frame, so each is pinned by a check rather than by eye:

- A pose's negative delay is a whole loop **minus** its own slice. Delaying by
  the slice alone plays the poses backwards, desynchronising them from the
  transform animation with no other symptom.
- A pose stays up for `TRACK_OVERLAP` slices rather than exactly one. At exactly
  one the outgoing pose can reach zero a frame before the incoming one reaches
  one, and the gap shows as the background flashing through.
- The yaw has to carry the facing direction to the same place a bearing points,
  or the head turns against the object that is locked to it. The test is that
  the nose and the head-locked object move the same way on screen.

Its cubes are drawn at the scene image's own size rather than smaller. Three
faithful tones separated by twelve per cent of the way to the ink read as one
tone on a cube two thirds that size, and the answer to that is room, not darker
paint.

`TRACK_POSES` is 72 over six seconds: twelve updates a second, 3.9 degrees a
step. Fewer reads as stepping; more is mostly file size. A group's own transform
already carries the floor position, so shapes inside it are drawn centred on the
origin — adding the floor's screen offset again drops them a room's height
below where they belong.

The outward and return turns share 37 distinct pose drawings through local
`<use>` references. Only byte-identical geometry is reused: all 72 timed
instances, their overlapping visibility, animation keyframes and reduced-motion
fallback remain intact. Coordinates and colours keep their original precision.

Only what actually changes shape is redrawn per pose. A horizontal plane maps to
the screen through an invertible linear map, so a yaw inside that plane is an
affine transform of the projection: `floor_rotation` returns it, and the
head-locked arc is drawn once at rest and turned by it. That matters for more
than size. A stepped arc beside a smoothly moving object visibly lags it, and
these arcs are meant to end **on** the objects they point at — each spans its own
object's radius, so one end meets the facing ray and the other meets the drop
line. The two ends stay within 1.3 px of each other across the loop; the
remainder is CSS interpolating a matrix by decomposition while the translate
beside it interpolates linearly. The scene-relative arc is the one thing that
genuinely cannot be a transform, because its sweep changes rather than rotating
— but its far end is on an object that never moves, so nothing lags there
either.

`generate-readme-diagrams.py` imports the palette, the camera and the voxel
geometry from `generate-readme-scene.py` rather than restating them, and
transcribes the constants it annotates — the silence floor, the footprint
scales, the nameplate dim, the silence hold, the peak hold — from
`src/scene3d/params.rs` and `src/app.rs` into one block at the top, and the
Magnetic page's grid — its shape, the reading its fill stops at, its marker and
its line colours — from `src/posebridge/coverage.rs` and
`src/posebridge/ui/calibration.rs` into another beside it. Change one of those
in the player and its block has to follow; it exists so that the
drift is visible rather than buried in coordinates. Leader lines aim at
geometry the generator projects, never at coordinates measured by hand.

## Regenerating

From the repository root, with Python and Node.js/npm:

```sh
python3 scripts/generate-readme-scene.py
python3 scripts/generate-readme-diagrams.py
```

```sh
python3 scripts/check-readme-text.py
```

The text check needs Chrome, Chromium or Microsoft Edge. It searches `PATH` and
the usual macOS and Windows installation folders. For a different installation,
pass its executable path (quote paths containing spaces):

```sh
python3 scripts/check-readme-text.py --browser "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
```

Run that after the diagrams. SVG does not wrap, so every line break in them is
placed by hand, and a line that outgrows its panel is invisible until someone
looks at the right picture on the right machine. Worse, fitting on the machine
that drew it proves nothing: the pictures ask for `-apple-system`, then
`Segoe UI`, then whatever sans-serif the reader has, and those differ in width
by more than a tenth. Three separate overflows reached a reader before this
check existed, each of them measuring as comfortably inside its panel here. So
the check measures every run with the real renderer and requires it to clear its
panel by a sixth of its own width, on whichever side a wider font would grow it
— the side away from its anchor. Readouts are skipped: their cells are fixed
by design. A line belongs to the panel containing its fixed text anchor, so a
centred or right-aligned line still gets checked against that panel when it
grows past its left edge.

Regression tests for panel ownership and browser discovery run without a browser
(Node.js is needed for the DOM probe fixtures):

```sh
python3 -m unittest discover -s scripts -p 'test_readme_text.py'
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
