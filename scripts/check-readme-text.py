#!/usr/bin/env python3
"""Report text in the generated diagrams that runs outside the box it belongs to.

SVG does not wrap, so every line break in these pictures is placed by hand and a
line that grows past its panel is invisible until someone looks. This measures
each <text> with the real renderer and checks it against the panel it sits in,
so the constraint is enforced rather than remembered.

Fitting exactly is not enough. The pictures ask for -apple-system, Segoe UI and
then whatever sans-serif the reader has, and those differ in width by more than
a tenth: a line measured here against Liberation Sans can still run past its
panel in San Francisco. So a run has to clear its panel by HEADROOM of its own
width, on whichever side a wider font would grow it — the side away from its
anchor.

Needs Chromium; run it after scripts/generate-readme-diagrams.py.
"""

from pathlib import Path
import argparse
import json
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
CHROME_CANDIDATES = ("/opt/pw-browsers/chromium-1194/chrome-linux/chrome",
                     "chromium", "chromium-browser", "google-chrome")
MARGIN, HEADROOM = 4.0, 0.16

PROBE = """
const out = [];
const svg = document.querySelector('svg');
const panels = [...svg.querySelectorAll('rect')]
  .map(r => ({x: r.x.baseVal.value, y: r.y.baseVal.value,
              w: r.width.baseVal.value, h: r.height.baseVal.value}))
  .filter(p => p.w > 120 && p.h > 40);
for (const t of svg.querySelectorAll('text')) {
  const b = t.getBBox();
  const inside = panels
    .filter(p => b.x >= p.x - 1 && b.x <= p.x + p.w + 1
              && b.y >= p.y - 1 && b.y <= p.y + p.h + 1)
    .sort((a, c) => a.w - c.w)[0];
  // A readout is laid out in fixed cells on purpose — the sign owns one and the
  // digits are right-aligned against the last — so it is not free-running prose
  // and a wider font does not make it spill.
  if (getComputedStyle(t).fontFamily.includes('mono')) continue;
  const box = inside || {x: 0, w: svg.width.baseVal.value};
  out.push([t.textContent.slice(0, 60), +b.x.toFixed(1), +b.width.toFixed(1),
            +box.x.toFixed(1), +(box.x + box.w).toFixed(1),
            getComputedStyle(t).textAnchor, inside ? 'panel' : 'card']);
}
document.getElementById('o').textContent = JSON.stringify(out);
"""


def chrome():
    for path in CHROME_CANDIDATES:
        if Path(path).exists() or subprocess.run(["which", path], capture_output=True).returncode == 0:
            return path
    sys.exit("Chromium not found; this check needs a browser to measure text.")


def measure(svg_path, scratch):
    page = scratch / f"{svg_path.stem}-text.html"
    page.write_text('<!doctype html><meta charset="utf-8">'
                    + svg_path.read_text(encoding="utf-8")
                    + f'<pre id="o"></pre><script>{PROBE}</script>', encoding="utf-8")
    dom = subprocess.run([chrome(), "--headless", "--disable-gpu", "--no-sandbox",
                          "--virtual-time-budget=8000", "--dump-dom", f"file://{page}"],
                         capture_output=True, text=True, timeout=180).stdout
    raw = re.search(r'<pre id="o">(.*?)</pre>', dom, re.S).group(1)
    for a, b in (("&quot;", '"'), ("&amp;", "&"), ("&lt;", "<"), ("&gt;", ">")):
        raw = raw.replace(a, b)
    return json.loads(raw)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dir", type=Path, default=ROOT / "assets/readme")
    args = parser.parse_args()
    scratch = ROOT / "target/readme-text-check"
    scratch.mkdir(parents=True, exist_ok=True)
    failures = 0
    for svg in sorted(args.dir.glob("*.svg")):
        for text, left, width, box_left, box_right, anchor, kind in measure(svg, scratch):
            # A wider font grows a run away from its anchor, so that is the side
            # that has to have room. A centred run grows both ways by half.
            grow_right = width * HEADROOM * (0.5 if anchor == "middle" else
                                             0.0 if anchor == "end" else 1.0)
            grow_left = width * HEADROOM * (0.5 if anchor == "middle" else
                                            1.0 if anchor == "end" else 0.0)
            over_right = (left + width + grow_right) - (box_right - MARGIN)
            over_left = (box_left + MARGIN) - (left - grow_left)
            if max(over_right, over_left) > 0:
                failures += 1
                side = "right" if over_right >= over_left else "left"
                print(f"{svg.name}: {max(over_right, over_left):.0f} px short of "
                      f"clearing its {kind} on the {side} once a wider font is "
                      f"allowed for\n    {text!r}")
    print(f"\n{failures} overflowing text run(s)" if failures
          else "\nevery text run fits the panel it starts in")
    sys.exit(1 if failures else 0)
