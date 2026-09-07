# README scene

`spatial-orbit.svg` is the shared animation in the Chinese and English READMEs.
It follows the palette and geometry in `src/theme.rs` and `src/scene3d/`, with
six illustrative objects rather than a recording of decoded audio. Two orbit
among four stationary objects, so individual trails remain easy to distinguish.

Regenerate from the repository root with Python and Node.js/npm:

```sh
python3 scripts/generate-readme-scene.py
```

The generator invokes SVGO **4.0.0** through `npm exec`, using
[`scripts/readme-svgo.config.mjs`](../../scripts/readme-svgo.config.mjs).
The first invocation may download that tool. This is only needed to regenerate
the artwork; viewing the README and building the player need no Node dependencies.
To inspect the intermediate SVG using Python alone:

```sh
python3 scripts/generate-readme-scene.py --unoptimized --output target/readme-scene.svg
```

The cube faces and floor marks share geometry, with CSS palettes for the forty
sample ages. Phase classes share animation delays; each airborne instance runs
its position and front/back opacity animations on the same element. Number
strokes share styling while keeping their original draw order and compositing.
The optimizer preserves initially transparent layers, animation transforms,
custom properties and separate strokes. Do not substitute SVGO's default
configuration: it can delete hidden animation layers and change antialiased
intersections when merging paths.

The image is self-contained: SVG geometry, local fragment references, and CSS
keyframes only. Embed it as a Markdown image; GitHub does not accept raw inline
SVG markup in a README. No JavaScript, external fonts, or network assets are used.
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
