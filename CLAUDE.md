# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Native desktop player for AC-4 spatial audio (`.m4a`, `.mp4`, `.ac4`), built on egui/eframe.
Decode runs on every platform — Core's crates carry no `target_os` of their own. What Windows adds
is *playback*, through Windows Spatial Audio. `--no-default-features` drops the `decode` feature for
an inspection-only shell, which is also the only configuration that builds without the spec tables. All decoding comes from `MacinDecode-AC4-Core` — the app never calls a
system media decoder.

Deeper design docs (in Chinese): `docs/ARCHITECTURE.md`, `docs/WINDOWS_DECODE.md`,
`docs/WINDOWS_SPATIAL_AUDIO.md`. They carry the authoritative contracts; keep them in sync when
changing the decode or output boundary.

## Commands

```bash
cargo run
cargo test
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

`rust-toolchain.toml` pins 1.98.0 with `profile = "minimal"`, so a fresh rustup has **no rustfmt or
clippy**. Run `rustup component add rustfmt clippy` first, or both lint commands fail with a rustup
recursion backtrace rather than a useful error.

Single test / workspace member:

```bash
cargo test decoder::tests::scene_queue_is_bounded_to_two_seconds_and_pop_releases_space
cargo test -p macindecode-windows-spatial-audio
```

### The `decode` feature needs the ETSI spec tables

`macindecode-ac4-scene`'s `audio-decode` feature pulls in a Core build script that reads three
locally generated tables. The requirement is keyed to the feature, not to a platform: with `decode`
on (the default) *every* build needs them, and with `--no-default-features` none does. Generate them
in a `MacinDecode-AC4-Core` checkout at the `rev` pinned in `Cargo.toml`, then point at it:

```bat
set "MACINDECODE_AC4_SPEC_DIR=<MacinDecode-AC4-Core>\spec"
```

The directory must contain `generated/ts103190_pdf_tables.rs`, `ts_103190_tables.c`, and
`ts_103190_tables_part2.c`. See the README for the `scripts/` invocations that produce them.

### Hardware/media regressions

Ignored tests need `MACINDECODE_AC4_TEST_MEDIA` and, for the output ones, a Spatial Audio-capable
endpoint. Real media goes only in the gitignored `.local-test-media/`, never in Git.

```bat
cargo test decoder::worker::tests::decodes_local_media_into_a_bounded_scene_buffer -- --ignored
cargo test decoder::worker::tests::seeks_real_media_across_epochs_without_rereading_the_file -- --ignored
cargo test backend::windows::tests::submits_decoded_scene_to_windows_spatial_audio -- --ignored
cargo test -p macindecode-windows-spatial-audio ended_renderer_releases_objects_without_entering_failed_state -- --ignored
```

Plain `cargo test` needs no media and no audio device.

## Architecture

### Coordinator and controllers

`app::PlayerApp` is the playback coordinator; everything else is a controller it drives. Its `ui()`
runs `sync_inspection` → `sync_decoder` → `sync_output` before drawing, on every frame, and the
order matters — `sync_output` reads the decoder's phase and current key.

All three controllers share one shape: `ensure_*()` declares desired state, `poll()` drains worker
events, `snapshot()` hands the UI an immutable view, and `revision()` bumps only on real change so
the status line isn't recomputed each frame.

- `inspection::InspectionController` — thread per request into `macindecode-ac4-inspect`;
  cross-platform, read-only. `bitstream_ui.rs` renders its report.
- `decoder::DecoderController` — command/event channels to one long-lived worker
  (`decoder/worker.rs`) plus the shared Scene FIFO.
- `backend::SpatialOutputController` — owns the native `Renderer` and a device-catalog worker
  (`backend/windows.rs`) that re-enumerates endpoints roughly every 2 s.

### PlaybackKey — the concurrency invariant

`PlaybackKey { request_id, playback_epoch }` (`decoder.rs`) gates every async path. `request_id`
increments when a source is opened or closed; `playback_epoch` increments on seek, end-of-stream
replay, device recovery, and Scene-topology rebuild. The FIFO, decode events, and the Scene reader
all reject a non-current key, so PCM from a superseded worker run can never reach the live stream.
Any new asynchronous path must carry and check a key.

### Bounded Scene FIFO

`SharedSceneQueue` holds at most `MAX_BUFFER_SECONDS` (2 s) of decoded per-channel frames;
`PREBUFFER_MILLISECONDS` (300 ms) is enough to reach Ready. Compressed bytes are read once and held
by the request — seek, replay, device recovery, and topology rebuild never touch the disk again, so
external edits to the active file are invisible until it is reselected.

### Core type isolation

`Ac4DecoderSession::decode_access_unit` returns borrowed views valid only until the Session's next
mutable call. `decoder/worker.rs::own_scene_frame` copies the minimal semantics — stable element
IDs, per-object mono planar normalized `f32`, one optional native LFE, integer sample times, OAMD
active/position/gain/ramp — into player-owned types *before* the worker lets the Session advance.
Core types must never cross into `backend`; the native crate must never see a bitstream or a Core
Session.

### unsafe boundary

The main crate is `unsafe_code = "forbid"` (both `Cargo.toml` lints and `main.rs`). Every COM call,
raw pointer, and Windows buffer lives in `crates/windows-spatial-audio`, which exposes a safe
surface (`Renderer`, `SpatialSource`, `RenderQuantum`, `enumerate_output_devices`).
`backend/windows.rs` adapts player semantics onto it; don't leak `windows` crate types past that
file.

### Render quantum adapter

`backend/source.rs` implements `SpatialSource` over the FIFO. One Windows quantum can span several
Scene blocks, so it concatenates blocks, trims pre-zero MP4 timeline and overlaps, zero-fills
forward gaps, interpolates OAMD ramps at the quantum start (Windows accepts one position/gain per
object per quantum, so in-quantum updates quantize to the next boundary), and converts Core/ADM
`[x, y, z]` to Windows listener coordinates `[x, z, -y]` clamped to `[-1, 1]`.

### Stream reuse vs rebuild

`SceneSignature` (`decoder.rs`) locks sample rate, configuration generation, presentation, dynamic
object element IDs, and LFE element ID from the first block. `OutputStreamConfig::stream_compatible`
(`backend.rs`) then decides whether a seek can `ReplaceSource` on the live stream or needs a full
renderer rebuild. When the signature changes mid-stream the adapter reports a recoverable error that
`app::is_reconfigurable_scene_error` matches by message prefix — so those error strings are a
contract between `backend/source.rs` and `app.rs`; changing one requires changing both.
`automatic_reconfigure_guard` keeps a failing rebuild from spinning.

### Seek

MP4/M4A requires *both* a container sync sample and Core `RandomAccess::Full`; raw `.ac4` requires
sync-frame ranges plus `RandomAccess::Full`. A background `ac4-seek-index` worker builds the index
in parallel with initial decode, so first playback never waits — the timeline stays disabled and the
status shows indexing until it lands. A candidate rejected by Core's audio layer falls back to the
previous Full candidate within the same epoch, but only before target PCM has been produced.

### Platform gating

Two inputs and one derived gate. Conflating the inputs is the mistake this section exists to
prevent; writing the conjunction by hand is the other one.

**`#[cfg(feature = "decode")]`** — there is a decoder. Covers `decoder/worker.rs`, the parts of
`decoder.rs` that drive it, and `backend/preview.rs`. Nothing here is platform-specific:
`decoder/worker.rs` imports only `std` and the Core crates and calls no OS API at all.

**`#[cfg(target_os = "windows")]`** — there is COM and WASAPI. On its own it now appears in only two
places: the native crate's own gating, and the two arms that word the "no output" message.

**`#[cfg(spatial_output)]`** — playback exists, i.e. *both* of the above. `build.rs` emits it (with
a matching `rustc-check-cfg`, so `unexpected_cfgs` stays quiet); nothing in `src/` writes
`all(target_os = "windows", feature = "decode")` by hand. It covers `backend/windows.rs`,
`backend/source.rs`, and every renderer-touching branch of `backend.rs`. Where it is off — any
non-Windows build, and a Windows build with `--no-default-features` — the output controller
constructs "unavailable". A site that got only half the conjunction is exactly what broke
`cargo build --no-default-features` on Windows once already.

Cross-platform types that only one side consumes carry `#[cfg_attr(not(<gate>), allow(dead_code,
reason = "..."))]` — keep that idiom on new fields or the other build warns (and `-D warnings` turns
that into a failure). Pick the gate by counting consumers: the Scene FIFO's read side and
`SceneViewMirror::write` have two (the render callback and the preview), so they key on `decode`;
`backend::state::lfe_render_state` has one, so it keys on `spatial_output`.

`cargo test` runs 122 tests with `--no-default-features` and 130 with `decode` on; the extra eight
cover `backend/preview.rs`, which is the scene view's clock on a build with a decoder but no
renderer — it walks the Scene FIFO at wall-clock rate through the same `backend::state` helpers the
render callback uses, so the picture cannot drift from what playback would submit. With `decode` on
it also compiles three media-gated `decoder::worker` tests (ignored without
`MACINDECODE_AC4_TEST_MEDIA`).
The decode worker asks for a 16 MiB stack explicitly rather than inheriting the Windows linker's
`/STACK`. Measured on one 20-object L4 A-JOC stream: release overflows at 512 KiB, debug at 1 MiB,
and both carry the stream at 2 MiB — so std's default is enough for *that* file, with under 2×
headroom in a debug build. Core reserves 16 MiB for the same reconstruction in its own tests, and a
thread stack is reserved address space rather than committed memory, so the reservation is the cheap
side of the trade.

## Conventions

- Rust 2024 edition, resolver 3. `clippy::pedantic` is `warn` in `Cargo.toml` but the project lint
  command uses `-D warnings`, so pedantic findings are hard errors. Silence them narrowly with
  `#[allow(..., reason = "...")]` — the existing code always supplies a `reason`.
- The four `MacinDecode-AC4-Core` crates are pinned to one git `rev`; bump all of them together and
  regenerate the spec tables from a matching Core checkout.
- `.cargo/config.toml` raises the Windows linker stack to 8 MB on both MSVC targets; the decoder
  depends on it.
- UI strings are English; README and `docs/` are Chinese. `theme.rs` installs a per-OS system CJK
  fallback font so CJK file names render.
- Commits follow Conventional Commits (`feat(gui):`, `fix:`, `docs:`).
