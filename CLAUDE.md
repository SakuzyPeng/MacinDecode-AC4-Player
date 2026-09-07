# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Native desktop player and inspector for AC-4 spatial audio (`.m4a`, `.mp4`, `.ac4`), built on
egui/eframe. All decoding comes from `MacinDecode-AC4-Core` — the app never calls a system media
decoder. Decode runs on every platform: Core's crates carry no `target_os` of their own.

What a platform adds is *playback*, and there are two independent output paths:

- **Windows object passthrough** — `crates/windows-spatial-audio` (COM/WASAPI). Dynamic objects plus
  one static LFE, submitted per Windows render quantum.
- **MacinRender** — `crates/macinrender` over the MacinRender C ABI. Either a SAF VBAP bed on fixed
  Apple geometry handed to the system spatializer ("system spatial audio"), or SAF HRTF binaural to
  any stereo device. Available on **macOS *and* Windows**; the crate is an optional dependency
  target-gated to those two, so a Linux build with the feature on simply doesn't compile it.

Linux gets inspection, decode and the silent scene preview — everything except audio out.

Two default features: `decode` (the Core decoder) and `macinrender` (the native renderer, which
implies `decode`). `--no-default-features` is the inspection-only shell, and the only configuration
that builds without the ETSI spec tables.

Design docs (Chinese) carry the authoritative contracts; keep them in sync when changing the boundary
they describe: `docs/ARCHITECTURE.md`, `docs/MACINRENDER.md` (output, Atmos label assist, head
control, native build), `docs/WINDOWS_DECODE.md`, `docs/WINDOWS_SPATIAL_AUDIO.md`,
`docs/PLAYLISTS.md` (persistence), `docs/STORAGE.md` (data directory, SOFA), `docs/PACKAGING.md`
(installers, CI). `README.md` (Chinese) and `README.en.md` (English) are a pair — a user-visible
change lands in both.

## Commands

```bash
cargo run
cargo test
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

`rust-toolchain.toml` pins 1.98.0 with `profile = "minimal"` and explicitly installs rustfmt and clippy.

Single test / workspace member:

```bash
cargo test decoder::tests::scene_queue_is_bounded_to_two_seconds_and_pop_releases_space
cargo test -p macindecode-windows-spatial-audio
```

`default-members` is `[".", "crates/windows-spatial-audio"]`, so a bare `cargo test` does not run
`macindecode-macinrender`'s own tests; ask for `-p macindecode-macinrender` (which needs the native
library built, see below).

### Build inputs the repository does not carry

Two things are fetched or generated rather than committed, and which of them you need depends on the
features, not on the platform:

| Feature | Input | Env |
| --- | --- | --- |
| `decode` | three ETSI TS 103 190 tables generated from the official spec | `MACINDECODE_AC4_SPEC_DIR` |
| `macinrender` | MacinRender sources built through CMake/Ninja with a C++20 toolchain (macOS/Windows only), plus Boost, and OpenBLAS/LAPACKE on Windows | `MACINRENDER_SOURCE_DIR`, `MACINRENDER_FETCHCONTENT_DIR`, `BOOST_ROOT`, `OPENBLAS_*` / `LAPACKE_*` / `CMAKE_TOOLCHAIN_FILE` |

`python scripts/prepare_inputs.py` produces all of it under the gitignored `.ci-inputs/`: it checks
out Core at the `rev` pinned in `Cargo.toml` and MacinRender at the `GIT_TAG` pinned in
`crates/macinrender/native/CMakeLists.txt`, runs Core's `fetch_specs.py` / `generate_spec_tables.py`,
and prepares Boost and (on Windows) OpenBLAS. It only exports into the calling process and
`GITHUB_ENV`, so for an interactive build either point the variables at `.ci-inputs/` yourself or run
`scripts/package.py`, which prepares and builds in one process.

`MACINDECODE_AC4_SPEC_DIR` must contain `generated/ts103190_pdf_tables.rs`, `ts_103190_tables.c` and
`ts_103190_tables_part2.c`.

### Offline or sandboxed builds

`build.rs` downloads a **checksum-pinned Noto Sans CJK SC** with `curl` and `include_bytes!`s it into
`theme.rs`; the app no longer relies on a system CJK font. With no network the build *fails* rather
than falling back — set `MACINDECODE_UI_FONT_PATH` to a local copy of that exact file (the SHA-256 in
`build.rs` is checked either way). `build.rs` also embeds the Windows icon/manifest and the
cargo-about license JSON named by `MACINDECODE_LICENSES_JSON` (absent ⇒ an empty About list).

### Hardware/media regressions

Ignored tests need `MACINDECODE_AC4_TEST_MEDIA`, and the output ones a real endpoint (a Spatial
Audio-capable one on Windows). `MACINDECODE_AC4_TEST_SOFA` gates the concurrent HRTF test in the
MacinRender crate. The MacinRender media tests render to a silent timed device unless
`MACINDECODE_AC4_TEST_SYSTEM_OUTPUT` opts into a real one, and `MACINDECODE_AC4_TEST_SECONDS`
bounds how long they run. Real media goes only in the gitignored `.local-test-media/`, never in
Git.

```bash
cargo test decoder::worker::tests::decodes_local_media_into_a_bounded_scene_buffer -- --ignored
cargo test decoder::worker::tests::seeks_real_media_across_epochs_on_the_open_file -- --ignored
cargo test backend::macinrender::tests::decodes_real_media_through_all_render_modes -- --ignored
cargo test backend::macinrender::tests::real_media_ends_at_the_container_boundary_and_can_seek_there -- --ignored
cargo test backend::windows::tests::submits_decoded_scene_to_windows_spatial_audio -- --ignored
cargo test -p macindecode-windows-spatial-audio ended_renderer_releases_objects_without_entering_failed_state -- --ignored
bash scripts/test-atmos-assist.sh 610   # macOS Atmos label assist against the real native library
```

Plain `cargo test` needs no media, no audio device and no GPU. Capacity benchmarks
(`capacity_one_hundred_thousand_entries`, `large_library_ui_render_capacity`) and the offscreen
`scene3d::gpu` readback are ignored for the same reason.

### Installers and install checks

```bash
python scripts/package.py --target x86_64-pc-windows-msvc
python3 scripts/package.py --target aarch64-apple-darwin
```

The script prepares the pinned inputs, builds release, then inspects the real payload, its system
dependencies, native rendering and a graphics window before writing `dist/`. The binary's own check
modes are `--check-install` and `--smoke-test`; both require an explicit `--data-dir` so they never
touch real user data. `--data-dir` (or `MACINDECODE_PLAYER_DATA_DIR`) also isolates the eframe
storage file, not just the database.

## Architecture

### Frame model: `logic` runs when nothing is drawn

eframe runs **no egui pass at all** while the window is minimized or hidden, so `PlayerApp` splits
`eframe::App`: `logic()` → `app/library_integration.rs::tick()` does all synchronisation and issues
the repaint requests, and `ui()` only draws. `tick()` order is `poll_library` → `poll_sofa_picker` →
`sync_inspection` → `sync_decoder` → (`restore_session_seek` or `sync_output`) → `persist_state`, and
the order matters: `sync_output` reads the decoder's phase and current key. Moving synchronisation
into `ui()` is the regression this split exists to prevent — a backgrounded player reached
end-of-item and stopped, because nothing polled the output and nothing asked for the pass that would
have noticed. `stage_drawn` carries "was anything drawn since the last call" into `logic`; a hidden
window keeps a slower cadence.

### Coordinator and controllers

`app::PlayerApp` is the coordinator; everything else is a controller it drives. Controllers share one
shape: `ensure_*()` declares desired state, `poll()` drains worker events, `snapshot()` hands the UI
an immutable view, and `revision()` bumps only on real change so the status line isn't recomputed
each frame.

- `library::LibraryController` — the single SQLite owner; UI messages carry identities and immutable
  snapshots, never a connection or a file handle. Also boots preferences and the session checkpoint.
- `inspection::InspectionController` — thread per request into `macindecode-ac4-inspect`;
  cross-platform, read-only. `bitstream_ui.rs` renders its report.
- `decoder::DecoderController` — command/event channels to one long-lived worker
  (`decoder/worker.rs`) plus the shared Scene FIFO.
- `backend::SpatialOutputController` (`backend/controller.rs`) — mode/device policy, settings hot
  swap, head control, and whichever output exists: the Windows `NativeOutputController`
  (`backend.rs`, `backend/windows.rs`) and/or the MacinRender producer (`backend/macinrender.rs`).
- `sofa_catalog` — background scan and atomic import of the managed `sofa/` folder.
- `head_tracking::HeadTracker` — its own clock, independent of egui repainting.

### Worker threads

Every one of these is named; keep new ones named too.

| Thread | Owner |
| --- | --- |
| `ac4-core-decode`, `ac4-seek-index` | `decoder/worker.rs` |
| `ac4-inspection` | `inspection.rs` |
| `player-library` | `library.rs` |
| `sofa-catalog` | `sofa_catalog.rs` |
| `listener-orientation` | `head_tracking.rs` |
| `macinrender-scene-producer`, `hrtf-preparation`, `pcm-device-catalog`, `discard-prepared-output` | `backend/macinrender.rs` |
| `prepare-audio-output` | `backend/controller.rs` |
| `windows-audio-device-catalog` | `backend/windows.rs` |
| `windows-spatial-audio` | `crates/windows-spatial-audio` |

Output changes prepare a *paused* new output on `prepare-audio-output` while the old one keeps
playing, and only hand over from the current presentation position once preparation succeeded.
Destruction never joins native callbacks on the UI thread.

### PlaybackKey — the concurrency invariant

`PlaybackKey { request_id, playback_epoch }` (`decoder.rs`) gates every async path. `request_id`
increments when a source is opened or closed; `playback_epoch` increments on seek, end-of-stream
replay, device recovery, and Scene-topology rebuild. The FIFO, decode events, the Scene reader and
the scene-view mirror all reject a non-current key, so PCM or positions from a superseded worker run
can never reach the live stream. Any new asynchronous path must carry and check a key.

### Bounded Scene FIFO

`SharedSceneQueue` holds at most `MAX_BUFFER_SECONDS` (2 s) of decoded per-channel frames;
`PREBUFFER_MILLISECONDS` (300 ms) is enough to reach Ready. Compressed packets are read on demand
through a retained file handle with per-worker 256 KiB buffers. MP4 metadata is shared and capped at
64 MiB; the sparse seek index holds at most `MAX_SEEK_POINTS` (8192) safe points. Seek and replay
reuse the handle without reopening the path. Detectable in-place file changes stop playback; the file
must then be removed and added again.

### Core type isolation

`Ac4DecoderSession::decode_access_unit` returns borrowed views valid only until the Session's next
mutable call. `decoder/worker.rs::own_scene_frame` copies the minimal semantics — stable element
IDs, per-object mono planar normalized `f32`, one optional native LFE, integer sample times, OAMD
active/position/gain/ramp — into player-owned types *before* the worker lets the Session advance.
Core types must never cross into `backend`; a native crate must never see a bitstream or a Core
Session.

### unsafe boundary

The main crate is `unsafe_code = "forbid"` (both `Cargo.toml` lints and `main.rs`). Two crates hold
all of it, each `unsafe_op_in_unsafe_fn = "deny"` and each exposing a safe surface:

- `crates/windows-spatial-audio` — COM, WASAPI, raw buffers (`Renderer`, `SpatialSource`,
  `RenderQuantum`, `enumerate_output_devices`). `backend/windows.rs` adapts player semantics onto it;
  don't leak `windows` crate types past that file.
- `crates/macinrender` — the MacinRender C ABI (`Session`, cloned controls, `Config`), plus the
  Objective-C++ Atmos label assist and CoreMotion bridges. `backend/macinrender.rs` adapts onto it;
  don't leak `raw`/ABI types past that file.

### The three Scene consumers

Exactly one consumer ever pops a given FIFO; all three resolve OAMD state through the same
`backend/state.rs` helpers (`validate_block`, `element_state_at`, `listener_render_state`), so
validation, timeline trimming and ramp resolution can't drift between them.

- `backend/source.rs` (`windows_spatial_output`) implements `SpatialSource` on the WASAPI callback.
  One quantum can span several Scene blocks, so it concatenates blocks, trims pre-zero MP4 timeline
  and overlaps, zero-fills forward gaps, and interpolates ramps at the quantum start (Windows takes
  one position/gain per object per quantum, so in-quantum updates quantize to the next boundary).
- `backend/macinrender.rs` (`macinrender_output`) submits from the `macinrender-scene-producer`
  thread and keeps a bounded **metadata-only** history (16 MiB) so the picture can be resolved at the
  device's presentation position rather than at what has been pushed. Gaps submit an explicitly
  silent Scene. The ABI wants a nonempty changed-field mask, and Core's bitmask is translated field
  by field — the bit numbers differ.
- `backend/preview.rs` (`decode`, constructed only where no renderer owns the FIFO) walks the queue
  on `Instant` — not egui input time, which freezes while hidden — and publishes positions without
  touching PCM.

Coordinates are converted per consumer: `listener_render_state` maps Core/ADM `[x, y, z]` to
`[x, z, -y]` clamped to `[-1, 1]` for the Windows API *and* for the scene view, while the MacinRender
producer submits ADM coordinates unchanged because the renderer is ADM-native.

### Scene view mirror

`scene_view::SceneViewMirror` is the one-way channel from whichever consumer is live to the frame
that draws it: `try_lock` writes that drop rather than block, a fixed `MAX_VIEW_OBJECTS` (20) array
so neither side allocates, and reader-copies-and-leaves. A scene past the budget is truncated and
reported on screen, never grown. `scene3d` draws it through wgpu (real depth buffer, MSAA) with
everything except `scene3d::gpu` unit-tested without an adapter.

### Stream reuse vs rebuild

`SceneSignature` (`decoder.rs`) locks sample rate, configuration generation, presentation, dynamic
object element IDs, and LFE element ID from the first block. `OutputStreamConfig::stream_compatible`
(`backend.rs`) then decides whether a seek can replace the source on the live stream or needs a full
rebuild. When the signature changes mid-stream the consumer reports a recoverable error that
`app::is_reconfigurable_scene_error` matches **by message prefix** — those strings are a contract
between `backend/state.rs` and `app.rs`; changing one requires changing both.
`automatic_reconfigure_guard` keeps a failing rebuild from spinning.

### Seek

MP4/M4A requires *both* a container sync sample and Core `RandomAccess::Full`; raw `.ac4` requires
sync-frame ranges plus `RandomAccess::Full`. A background `ac4-seek-index` worker builds the index in
parallel with initial decode, so first playback never waits — the timeline stays disabled and the
status shows indexing until it lands. A candidate rejected by Core's audio layer falls back to the
previous Full candidate within the same epoch, but only before target PCM has been produced. Replay
and seeks to time zero decode from the first access unit to keep MP4 priming, rather than jumping to
a later sync sample.

### Persistence

`library/store.rs` owns bundled SQLite (foreign keys, WAL, `synchronous=FULL`) on the
`player-library` thread; `preferences.rs` owns versioned JSON written through a same-directory temp
file, fsync and atomic replace with a `.bak`, plus the data-directory lock that keeps one writer per
directory. Browse focus, playback source and list membership are modelled separately — browsing list
B must not disturb playback from list A. Preference and browse changes coalesce over 500 ms; the
playback checkpoint saves every 5 s plus on pause, track change and exit. Startup restore waits for
the seek index and must not overwrite a stored checkpoint with the zero position of a fresh start.
Never migrate a database version without going through the SQLite backup API first.

### Platform gating

Four inputs, all derived in `build.rs` (each with a matching `rustc-check-cfg`, so `unexpected_cfgs`
stays quiet). Nothing in `src/` writes the conjunctions by hand.

- **`feature = "decode"`** — there is a decoder. `decoder/worker.rs`, the parts of `decoder.rs` that
  drive it, `backend/preview.rs`. Nothing here is platform-specific: `decoder/worker.rs` imports only
  `std` and the Core crates and calls no OS API at all.
- **`windows_spatial_output`** = Windows + `decode`. `backend/windows.rs`, `backend/source.rs`, the
  `NativeOutputController` branches of `backend.rs`.
- **`macinrender_output`** = (macOS or Windows) + `decode` + `macinrender`. The largest gate by far:
  `backend/macinrender.rs`, most of `backend/controller.rs`, the SOFA and layout settings.
  Pair it with `target_os = "macos"` for the Atmos label assist and AirPods motion.
- **`spatial_output`** = the union, i.e. *some* real output exists. Deliberately rare now (a repaint
  cadence and one test) — prefer the specific gate that matches the code you are writing.

Bare `#[cfg(target_os = "windows")]` survives only where the OS itself is the subject: Windows
share-mode file opening in `media.rs`, the wording of the "no output" arms, and dynamic-object budget
policy in `backend/controller.rs`.

Cross-platform types that only one side consumes carry `#[cfg_attr(not(<gate>), allow(dead_code,
reason = "..."))]` — keep that idiom on new fields or the other build warns, and `-D warnings` turns
that into a failure. Pick the gate by counting consumers: the Scene FIFO's read side and
`SceneViewMirror::write` have three (both render paths and the preview), so they key on `decode`;
`backend::state::lfe_render_state` has one, so it keys on `windows_spatial_output`. A site that got
only half a conjunction is what broke `cargo build --no-default-features` on Windows once already, so
after touching gating, build both `--no-default-features` and the default set on the platform you are
on.

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
  regenerate the spec tables from a matching Core checkout. The MacinRender commit is pinned
  separately in `crates/macinrender/native/CMakeLists.txt`.
- `.cargo/config.toml` sets `+crt-static` and `/STACK:8000000` on both MSVC targets and pins
  `MACOSX_DEPLOYMENT_TARGET=14.0`; the decoder depends on the stack size and packaging on the rest.
- UI strings are English. `docs/` is Chinese; `README.md` is Chinese with `README.en.md` alongside it.
- Commits follow Conventional Commits (`feat(gui):`, `fix:`, `docs:`).
