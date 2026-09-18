#!/usr/bin/env python3
"""Generate the built-in demo score from the Mutopia LilyPond sources.

The demo track is Pachelbel's Canon in D, synthesised in real time. The
composition is public domain; this typesetting is CC BY 4.0 and is credited in
the About list, the README pair and the manual pair.

Transcribing mechanically rather than by hand is the whole point: a familiar
piece is exactly the one where a wrong note is glaring. The parser below covers
only the LilyPond subset this score uses and refuses anything else rather than
guessing, and every bar is checked to hold its four beats.

`src/decoder/demo/score.rs` is committed, so neither the build nor the tests
need the network. Re-run after changing the tempo, the phase map or the sources:

    python3 scripts/generate-demo-score.py                 # fetch and verify
    python3 scripts/generate-demo-score.py --source-dir DIR
    python3 scripts/generate-demo-score.py --print-checksums
"""

from __future__ import annotations

import argparse
import hashlib
import math
import re
import subprocess
import sys
import tempfile
from pathlib import Path

BASE_URL = (
    "https://www.mutopiaproject.org/ftp/PachelbelJ/"
    "Canon_per_3_Violini_e_Basso/Canon_per_3_Violini_e_Basso-lys/"
)
# Mutopia-2015/09/02-2047, maintained by Michael Fischer v. Mollard, CC BY 4.0.
SOURCES = {
    "violin_common.ily": "42442889c4a5830fb90bab344253bd9c2c4763454f31589f99babd981e44e5a4",
    "violin_one.ily": "38b81adcc25643bab07e2bf99d22031c67345a50aca71d0b95210d7c9970617e",
    "violin_two.ily": "7fedbac0b998a5da396c2c3646a867a2a9884dde2c2ee3fb42e731a8642053e9",
    "violin_three.ily": "1afa8a6667441d986aa22587586cc8ad369c432fcd21be6f70d7c75ceeb18321",
    "violoncello.ily": "33234ebdcce2adf5724084d1149dcfd9d3d55712c3f86b39ffd85fd553270003",
}

# 48 ticks per quarter divides every value this score uses: a 32nd is 6 ticks
# and a dotted eighth is 36, so no onset is ever rounded.
TICKS_PER_BEAT = 48
BEATS_PER_BAR = 4
TICKS_PER_BAR = TICKS_PER_BEAT * BEATS_PER_BAR
GROUND_REPEATS = 28
BARS = 57
TEMPO_BPM = 72

# Bars of rest before each violin picks up the shared line.
VIOLIN_ENTRY_BARS = (2, 4, 6)

# LilyPond `c` with no octave mark is C3 = MIDI 48, so its diatonic index is 21.
STEP_SEMITONES = (0, 2, 4, 5, 7, 9, 11)
STEP_NAMES = "cdefgab"
ACCIDENTALS = {"isis": 2, "is": 1, "eses": -2, "es": -1, "": 0}
VIOLIN_REFERENCE = 35  # \relative c''
CELLO_REFERENCE = 21  # \relative c

# One object per ringing note needs the previous note to have fallen far enough
# first. Amplitude e^(-t/tau) reaches -20 dB at t = ln(10) * tau, which is where
# truncating a tail stops being audible in this texture.
STEAL_GUARD_TAUS = math.log(10)
# The keyboard phase is the only one that spends an object per note, so it is
# the only one whose timbre is pinned by the object budget rather than by taste.
KEYBOARD_TAU_SECONDS = 0.25

# Statement ranges are inclusive. `pool` is how many of the 16 slots a phase
# spends beyond the six-slot musical spine; the keyboard phase also reclaims the
# three canon-voice slots, whose voices have handed their notes to the pool.
MAX_DYNAMIC_OBJECTS = 16
SPINE_SLOTS = 6
CANON_VOICE_SLOTS = 3
PHASES = (
    # name, first statement, last statement, pool slots, spends an object per note
    ("entry", 1, 3, 0, False),
    ("choreography", 4, 9, 0, False),
    ("climax", 10, 13, 0, False),
    ("reference", 14, 19, 2, False),
    ("keyboard", 20, 23, 13, True),
    ("gain-ring", 24, 26, 4, False),
    ("close", 27, 28, 0, False),
)

# Constants only the tests read: the goldens exist so a change has to be
# deliberate, and the arrangement reads the note tables directly.
TEST_ONLY = (
    "#[cfg_attr(",
    "    not(test),",
    "    allow(",
    "        dead_code,",
    '        reason = "these express the contract the score is generated against -- the \\',
    "                  phase budget, the note geometry, the golden digest -- and the \\",
    '                  tests are what check it; the arrangement reads the tables directly"',
    "    )",
    ")]",
)

REPOSITORY = Path(__file__).resolve().parent.parent
OUTPUT = REPOSITORY / "src" / "decoder" / "demo" / "score.rs"


class ScoreError(RuntimeError):
    """The sources said something this generator refuses to guess about."""


# ---------------------------------------------------------------------------
# LilyPond subset parser
# ---------------------------------------------------------------------------

TOKEN = re.compile(
    r"""
      (?P<bar>\|)
    | (?P<rest>[rR])(?P<rest_dur>\d+)?(?P<rest_dots>\.*)(?:\s*\*\s*(?P<rest_mul>\d+))?
    | (?P<step>[a-g])(?P<alter>isis|eses|is|es)?(?P<octave>[',]*)
      (?P<dur>\d+)?(?P<dots>\.*)(?P<art>(?:[-^_][.\->^_+])*)(?P<tie>~)?
    """,
    re.VERBOSE,
)


def preprocess(text: str) -> str:
    """Strip what the tokenizer must not see and expand `\\repeat unfold`."""
    text = re.sub(r"%[^\n]*", " ", text)
    text = re.sub(r'"[^"]*"', " ", text)
    # This score's repeats hold no nesting; a nested one would be mis-expanded.
    if re.search(r"\\repeat\s+unfold[^{]*\{[^{}]*\{", text):
        raise ScoreError("nested \\repeat unfold is not supported")
    text = re.sub(
        r"\\repeat\s+unfold\s+(\d+)\s*\{([^{}]*)\}",
        lambda m: (" " + m.group(2) + " ") * int(m.group(1)),
        text,
    )
    # Take these two before the blanket command strip: `\key d \major` would
    # otherwise leave a bare `d` behind, and it would parse as a note.
    text = re.sub(r"\\key\s+[a-g](?:is|es)?\s*\\[a-zA-Z]+", " ", text)
    text = re.sub(r"\\time\s+\d+\s*/\s*\d+", " ", text)
    text = re.sub(r"\\[a-zA-Z]+", " ", text)
    text = re.sub(r"#\S+", " ", text)
    text = text.replace("{", " ").replace("}", " ")
    # Both `fis 4` and `a16fis32` occur in the sources. Normalise the spacing so
    # the tokenizer sees one shape; the first spelling silently produced eighths
    # instead of quarters when this was left to the tokenizer.
    text = re.sub(r"\b([a-g](?:isis|eses|is|es)?[',]*|[rR])\s+(?=\d)", r"\1", text)
    text = re.sub(r"(\d\.?)(?=[a-gr])", r"\1 ", text)
    return text


def dotted(duration: int, dots: str) -> int:
    length = duration
    added = duration
    for _ in dots:
        added //= 2
        length += added
    return length


def resolve_relative(previous: int, step: int, marks: str) -> int:
    """LilyPond relative octaves: the octave within a diatonic fourth, then marks.

    Works in diatonic indices throughout, the way LilyPond does. Deriving the
    index back from a MIDI number cannot work: a black key is two spellings.
    """
    base = previous - (previous % 7) + step
    for candidate in (base - 7, base, base + 7):
        if abs(candidate - previous) <= 3:
            base = candidate
            break
    else:  # pragma: no cover - unreachable for any step in 0..7
        raise ScoreError("no octave lies within a fourth")
    return base + 7 * (marks.count("'") - marks.count(","))


def midi_of(diatonic: int, alter: int) -> int:
    return 12 * (diatonic // 7 + 1) + STEP_SEMITONES[diatonic % 7] + alter


def parse_relative(
    text: str, reference: int, *, name: str
) -> tuple[list[dict], int, int]:
    """Parse a `\\relative` body into notes, its bar count and its final step.

    The final diatonic index matters because each violin part continues
    relatively from the last note of the shared line, so a part's tail cannot
    be parsed on its own.
    """
    text = preprocess(text)
    notes: list[dict] = []
    tick = 0
    bar_start = 0
    bars = 0
    duration = TICKS_PER_BEAT
    previous = reference
    tied: dict | None = None
    cursor = 0
    for match in TOKEN.finditer(text):
        gap = text[cursor : match.start()]
        if gap.strip():
            raise ScoreError(f"{name}: unparsed text {gap.strip()!r}")
        cursor = match.end()
        if match.group("bar"):
            held = tick - bar_start
            if held != TICKS_PER_BAR:
                raise ScoreError(
                    f"{name}: bar {bars + 1} holds {held / TICKS_PER_BEAT} beats,"
                    f" expected {BEATS_PER_BAR}"
                )
            bar_start = tick
            bars += 1
            continue
        if match.group("rest") is not None:
            if match.group("rest") == "R":
                length = TICKS_PER_BAR
            else:
                if match.group("rest_dur"):
                    duration = TICKS_PER_BEAT * 4 // int(match.group("rest_dur"))
                length = dotted(duration, match.group("rest_dots"))
            tick += length * int(match.group("rest_mul") or 1)
            tied = None
            continue
        step = STEP_NAMES.index(match.group("step"))
        previous = resolve_relative(previous, step, match.group("octave"))
        pitch = midi_of(previous, ACCIDENTALS[match.group("alter") or ""])
        if match.group("dur"):
            duration = TICKS_PER_BEAT * 4 // int(match.group("dur"))
        length = dotted(duration, match.group("dots"))
        if tied is not None:
            if tied["pitch"] != pitch:
                raise ScoreError(f"{name}: a tie joins MIDI {tied['pitch']} to {pitch}")
            tied["duration"] += length
        else:
            notes.append({"onset": tick, "duration": length, "pitch": pitch})
        tick += length
        tied = notes[-1] if match.group("tie") else None
    if text[cursor:].strip():
        raise ScoreError(f"{name}: unparsed tail {text[cursor:].strip()!r}")
    if tick != bar_start:
        bars += 1
    return notes, bars, previous


def body_of(text: str, variable: str) -> str:
    """The `\\relative` body assigned to `variable`, brace-matched."""
    match = re.search(rf"{variable}\s*=\s*\\relative\s+\S+\s*\{{", text)
    if match is None:
        raise ScoreError(f"no \\relative assignment for {variable}")
    depth = 1
    index = match.end()
    while depth and index < len(text):
        depth += {"{": 1, "}": -1}.get(text[index], 0)
        index += 1
    if depth:
        raise ScoreError(f"unbalanced braces in {variable}")
    return text[match.end() : index - 1]


# ---------------------------------------------------------------------------
# Sources
# ---------------------------------------------------------------------------


def load(directory: Path | None) -> tuple[dict[str, str], dict[str, str]]:
    """Read the five LilyPond files, returning their text and their SHA-256s.

    A pinned digest that no longer matches is refused rather than regenerated
    from: the score is committed, so a silent upstream change would land as an
    unexplained diff in a file nobody reads note by note.
    """
    sources: dict[str, str] = {}
    digests: dict[str, str] = {}
    for name, expected in SOURCES.items():
        if directory is not None:
            data = (directory / name).read_bytes()
        else:
            result = subprocess.run(
                ["curl", "--fail", "--silent", "--show-error", "--location", BASE_URL + name],
                capture_output=True,
                check=False,
            )
            if result.returncode != 0:
                raise ScoreError(
                    f"could not download {name}: "
                    f"{result.stderr.decode(errors='replace').strip()}"
                    "; pass --source-dir for an offline run"
                )
            data = result.stdout
        digests[name] = hashlib.sha256(data).hexdigest()
        if expected and digests[name] != expected:
            raise ScoreError(f"{name} has SHA-256 {digests[name]}, expected {expected}")
        sources[name] = data.decode("utf-8")
    return sources, digests


# ---------------------------------------------------------------------------
# Assembly
# ---------------------------------------------------------------------------


def assemble(files: dict[str, str]) -> dict:
    line, line_bars, line_final = parse_relative(
        body_of(files["violin_common.ily"], "violinCommon"),
        VIOLIN_REFERENCE,
        name="violin_common",
    )

    tails = []
    for index, (source, variable) in enumerate(
        (("violin_one.ily", "violinone"), ("violin_two.ily", "violintwo"), ("violin_three.ily", "violinthree"))
    ):
        body = body_of(files[source], variable)
        if "\\violinCommon" not in body:
            raise ScoreError(f"{variable} does not use the shared line")
        prefix, suffix = body.split("\\violinCommon", 1)
        rests = re.findall(r"R1\s*\*\s*(\d+)", prefix)
        if len(rests) != 1:
            raise ScoreError(f"{variable}: expected one R1*n entry rest, found {len(rests)}")
        entry_bars = int(rests[0])
        if entry_bars != VIOLIN_ENTRY_BARS[index]:
            raise ScoreError(
                f"{variable} enters after {entry_bars} bars, expected {VIOLIN_ENTRY_BARS[index]}"
            )
        # A part's tail continues relatively from the last note of the shared
        # line, so it is parsed with that step as its reference.
        tail, tail_bars, _ = parse_relative(suffix, line_final, name=variable)
        if entry_bars + line_bars + tail_bars != BARS:
            raise ScoreError(
                f"{variable} spans {entry_bars + line_bars + tail_bars} bars, expected {BARS}"
            )
        tails.append(tail)

    cello, cello_bars, _ = parse_relative(
        body_of(files["violoncello.ily"], "violoncello"), CELLO_REFERENCE, name="violoncello"
    )
    if cello_bars != BARS:
        raise ScoreError(f"the ground spans {cello_bars} bars, expected {BARS}")
    ground, coda = split_ground(cello)

    return {
        "line": line,
        "line_bars": line_bars,
        "tails": tails,
        "ground": ground,
        "coda": coda,
    }


def split_ground(cello: list[dict]) -> tuple[list[dict], list[dict]]:
    """Separate the two-bar ostinato from the final bar, checking it really repeats.

    This is the structural claim the whole demo rests on: 28 identical
    statements carrying the harmony, so any subset of canon voices above it
    still sounds correct. Verify it here rather than trusting the typesetting.
    """
    statement = 2 * TICKS_PER_BAR
    ground = [note for note in cello if note["onset"] < statement]
    for repeat in range(1, GROUND_REPEATS):
        shift = repeat * statement
        actual = [note for note in cello if shift <= note["onset"] < shift + statement]
        expected = [
            {"onset": note["onset"] + shift, "duration": note["duration"], "pitch": note["pitch"]}
            for note in ground
        ]
        if actual != expected:
            raise ScoreError(f"ground statement {repeat + 1} departs from the ostinato")
    closing = GROUND_REPEATS * statement
    coda = [
        {"onset": note["onset"] - closing, "duration": note["duration"], "pitch": note["pitch"]}
        for note in cello
        if note["onset"] >= closing
    ]
    return ground, coda


def score_digest(score: dict) -> int:
    """FNV-1a over every note, in table order.

    The pitch-class and range goldens catch an accidental that was lost or
    invented, but not a wrong note inside the set the score already uses -- and
    a wrong note in a piece this familiar is exactly what must not survive.
    This covers every field of every note; the Rust side recomputes it.
    """
    digest = 0xCBF29CE484222325
    tables = [score["line"], *score["tails"], score["ground"], score["coda"]]
    for table in tables:
        for entry in table:
            payload = (
                entry["onset"].to_bytes(4, "little")
                + entry["duration"].to_bytes(4, "little")
                + bytes([entry["pitch"]])
            )
            for byte in payload:
                digest = ((digest ^ byte) * 0x100000001B3) & 0xFFFF_FFFF_FFFF_FFFF
    return digest

# ---------------------------------------------------------------------------
# Object budget
# ---------------------------------------------------------------------------


def violin_onsets(score: dict) -> list[float]:
    """Every violin onset in ticks from bar 1, across all three voices."""
    onsets = []
    for index, tail in enumerate(score["tails"]):
        entry = VIOLIN_ENTRY_BARS[index] * TICKS_PER_BAR
        onsets += [entry + note["onset"] for note in score["line"]]
        after = entry + score["line_bars"] * TICKS_PER_BAR
        onsets += [after + note["onset"] for note in tail]
    return sorted(float(onset) for onset in onsets)


def guard_ticks(tau_seconds: float) -> float:
    return STEAL_GUARD_TAUS * tau_seconds * TICKS_PER_BEAT * TEMPO_BPM / 60.0


def peak_ringing(onsets: list[float], first: float, last: float, guard: float) -> int:
    """Most notes still inside the guard window at any onset in [first, last)."""
    peak = 0
    start = 0
    for index, onset in enumerate(onsets):
        if not first <= onset < last:
            continue
        while onsets[start] <= onset - guard:
            start += 1
        peak = max(peak, index - start + 1)
    return peak


def budget(score: dict) -> list[dict]:
    """Per-phase object usage, refusing any phase that would exceed the cap."""
    onsets = violin_onsets(score)
    guard = guard_ticks(KEYBOARD_TAU_SECONDS)
    statement = 2 * TICKS_PER_BAR
    report = []
    for name, first, last, pool, per_note in PHASES:
        span = (float((first - 1) * statement), float(last * statement))
        peak = peak_ringing(onsets, *span, guard)
        spine = SPINE_SLOTS - CANON_VOICE_SLOTS if per_note else SPINE_SLOTS
        total = spine + pool
        if total > MAX_DYNAMIC_OBJECTS:
            raise ScoreError(
                f"phase {name} would hold {total} objects, over the {MAX_DYNAMIC_OBJECTS} cap"
            )
        if per_note and peak > pool:
            raise ScoreError(
                f"phase {name} needs {peak} pool slots for its notes but has {pool};"
                f" move the phase, shorten the decay or widen the pool"
            )
        report.append(
            {
                "name": name,
                "first": first,
                "last": last,
                "pool": pool,
                "per_note": per_note,
                "peak": peak,
                "total": total,
            }
        )
    return report


# ---------------------------------------------------------------------------
# Emit
# ---------------------------------------------------------------------------


def render_notes(notes: list[dict]) -> str:
    return ", ".join(f"note({n['onset']}, {n['duration']}, {n['pitch']})" for n in notes)


def emit(score: dict, report: list[dict], digests: dict[str, str]) -> str:
    line = score["line"]
    pitches = sorted({n["pitch"] for group in [line, score["ground"], score["coda"], *score["tails"]] for n in group})
    classes = sorted({pitch % 12 for pitch in pitches})
    statement = 2 * TICKS_PER_BAR
    entries = [bars * TICKS_PER_BAR for bars in VIOLIN_ENTRY_BARS]

    out = [
        "//! Pachelbel's Canon in D, transcribed for the built-in demo track.",
        "//!",
        "//! Generated by `scripts/generate-demo-score.py`. Do not edit by hand:",
        "//! every value here is checked against the sources at generation time,",
        "//! and a hand edit would pass those checks without ever running them.",
        "//!",
        "//! The composition (Pachelbel, 1694) is public domain. The typesetting the",
        "//! notes were transcribed from is Mutopia-2015/09/02-2047, maintained by",
        "//! Michael Fischer v. Mollard, under CC BY 4.0.",
        "//!",
        "//! It is a canon, so the melodic line is stored once. The three violins are",
        "//! that one line at three entry offsets, which is why a voice can never",
        "//! drift from its companions: there is nothing to drift from.",
        "",
        "use super::{Note, Phase, note, phase};",
        "",
        "/// Ticks per quarter note. Divides every value the score uses, so no onset",
        "/// is ever rounded: a 32nd is 6 ticks and a dotted eighth is 36.",
        f"pub(crate) const TICKS_PER_BEAT: u32 = {TICKS_PER_BEAT};",
        f"pub(crate) const BEATS_PER_BAR: u32 = {BEATS_PER_BAR};",
        f"pub(crate) const TEMPO_BPM: u32 = {TEMPO_BPM};",
        f"pub(crate) const BARS: u32 = {BARS};",
        "/// One ground statement is two bars, and it is also the demo's phase unit:",
        "/// a phase boundary lands where the bass returns to its first note.",
        f"pub(crate) const TICKS_PER_STATEMENT: u32 = {statement};",
        f"pub(crate) const GROUND_REPEATS: u32 = {GROUND_REPEATS};",
        "",
        "/// How far the shared line runs, so a violin's tail knows where it starts.",
        f"pub(crate) const CANON_LINE_TICKS: u32 = {score['line_bars'] * TICKS_PER_BAR};",
        "",
        "/// The shared canon line, in ticks from its own first bar.",
        f"pub(crate) const CANON_LINE: &[Note] = &[{render_notes(line)}];",
        "",
        "/// Where each violin picks the line up, in ticks from the first bar.",
        f"pub(crate) const CANON_ENTRIES: [u32; 3] = [{', '.join(str(e) for e in entries)}];",
        "",
        "/// What each violin plays after the shared line runs out, in ticks from",
        "/// the end of its own statement of that line.",
        "pub(crate) const CANON_TAILS: [&[Note]; 3] = [",
    ]
    for tail in score["tails"]:
        out.append(f"    &[{render_notes(tail)}],")
    out += [
        "];",
        "",
        "/// The ground bass: one two-bar statement, repeated `GROUND_REPEATS` times.",
        f"pub(crate) const GROUND: &[Note] = &[{render_notes(score['ground'])}];",
        "",
        "/// The closing bar, in ticks from the end of the last ground statement.",
        f"pub(crate) const GROUND_CODA: &[Note] = &[{render_notes(score['coda'])}];",
        "",
        "/// Every note of every table, hashed FNV-1a in table order.",
        "///",
        "/// The goldens below catch an accidental that went missing or appeared,",
        "/// but not a wrong note inside the set the score already uses. This",
        "/// catches any of them, which is the point of transcribing a piece",
        "/// everyone knows: the error has to be impossible to smuggle in.",
        *TEST_ONLY,
        f"pub(crate) const SCORE_DIGEST: u64 = {score_digest(score):_};",
        "",
        "/// Every pitch class the score uses, as a golden value. D major plus the",
        "/// C natural the line borrows on its way to G; a transcription that lost",
        "/// or invented an accidental would change this list.",
        *TEST_ONLY,
        f"pub(crate) const PITCH_CLASSES: &[u8] = &[{', '.join(str(c) for c in classes)}];",
        f"pub(crate) const PITCH_RANGE: (u8, u8) = ({pitches[0]}, {pitches[-1]});",
        "",
        "/// The most dynamic objects the demo may hold at once.",
        "///",
        "/// Under this cap the scene view never truncates, and the stream still",
        "/// configures on an endpoint that offers no more than sixteen.",
        f"pub(crate) const MAX_DYNAMIC_OBJECTS: u32 = {MAX_DYNAMIC_OBJECTS};",
        "/// Slots held by the music itself: three violins, the ground, two continuo.",
        f"pub(crate) const SPINE_SLOTS: u32 = {SPINE_SLOTS};",
        f"pub(crate) const CANON_VOICE_SLOTS: u32 = {CANON_VOICE_SLOTS};",
        "/// Seconds of decay a pool slot must be given before it is handed on.",
        "///",
        "/// `ln(10) * tau` is where the tail has fallen 20 dB, which is where",
        "/// truncating it stops being audible against what follows.",
        f"pub(crate) const KEYBOARD_TAU_SECONDS: f32 = {KEYBOARD_TAU_SECONDS};",
        "pub(crate) const STEAL_GUARD_TAUS: f32 = std::f32::consts::LN_10;",
        "",
        "/// What each phase demonstrates, and what it spends to do it.",
        "///",
        "/// `peak_ringing` is what the generator measured from the tables above. The",
        "/// Rust side recomputes it and asserts agreement, so the two derivations",
        "/// have to keep agreeing about how many objects this score needs.",
        "pub(crate) const PHASES: &[Phase] = &[",
    ]
    for entry in report:
        out.append(
            f'    phase("{entry["name"]}", {entry["first"]}, {entry["last"]},'
            f' {entry["pool"]}, {str(entry["per_note"]).lower()}, {entry["peak"]}),'
        )
    out += ["];", ""]
    out.append("// Source digests, SHA-256:")
    for name, digest in digests.items():
        out.append(f"//   {name}  {digest}")
    out.append("")
    return "\n".join(out)


def format_rust(path: Path) -> None:
    """Run rustfmt so the committed file matches what `cargo fmt` would leave."""
    result = subprocess.run(
        ["rustfmt", "--edition", "2024", str(path)], capture_output=True, check=False
    )
    if result.returncode != 0:
        print(
            f"generate-demo-score: rustfmt failed: {result.stderr.decode(errors='replace').strip()}",
            file=sys.stderr,
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-dir", type=Path, help="read the .ily sources from here")
    parser.add_argument(
        "--print-checksums", action="store_true", help="print SHA-256 digests and exit"
    )
    parser.add_argument("--check", action="store_true", help="fail if the output would change")
    arguments = parser.parse_args()

    try:
        files, digests = load(arguments.source_dir)
        if arguments.print_checksums:
            for name, digest in digests.items():
                print(f'    "{name}": "{digest}",')
            return 0
        score = assemble(files)
        report = budget(score)
        rendered = emit(score, report, digests)
    except ScoreError as error:
        print(f"generate-demo-score: {error}", file=sys.stderr)
        return 1

    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    if arguments.check:
        # Format the candidate the same way before comparing: the committed file
        # has been through rustfmt, so comparing the raw render always differs.
        with tempfile.TemporaryDirectory() as directory:
            candidate = Path(directory) / OUTPUT.name
            candidate.write_text(rendered)
            format_rust(candidate)
            if not OUTPUT.exists() or OUTPUT.read_text() != candidate.read_text():
                print(
                    f"generate-demo-score: {OUTPUT} is stale; re-run this script",
                    file=sys.stderr,
                )
                return 1
    else:
        OUTPUT.write_text(rendered)
        format_rust(OUTPUT)

    notes = len(score["line"])
    print(f"canon line: {notes} notes over {score['line_bars']} bars")
    print(f"ground: {len(score['ground'])} notes x {GROUND_REPEATS}, coda {len(score['coda'])}")
    print(f"tails: {', '.join(str(len(tail)) for tail in score['tails'])} notes")
    print()
    print(f"{'phase':<14} {'statements':<12} {'peak':>5} {'pool':>5} {'objects':>8}")
    for entry in report:
        span = f"S{entry['first']}-S{entry['last']}"
        note_mark = " *" if entry["per_note"] else ""
        print(
            f"{entry['name']:<14} {span:<12} {entry['peak']:>5} {entry['pool']:>5}"
            f" {entry['total']:>8}{note_mark}"
        )
    print(f"\n* spends one object per ringing note; cap is {MAX_DYNAMIC_OBJECTS}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
