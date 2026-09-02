//! The 3D object scene view.
//!
//! An isometric technical drawing that happens to be made of voxels: the theme
//! is already zero-radius, hairline-bordered and flat-filled, which is the same
//! geometric language as an axis-aligned voxel. The point is not to embed a game
//! viewport in a paper UI.
//!
//! Layering, and the reason for it: everything except [`gpu`] works in plain
//! arrays and is unit tested, because `cargo test` cannot bring up wgpu in a
//! headless environment. [`gpu`] only uploads what [`mesh`] produced. This
//! mirrors the discipline the rest of the crate already keeps — `unsafe` shut
//! inside the native crate, Core types kept out of `backend`.
//!
//! Rendering goes through wgpu rather than `egui::Painter` so there is a real
//! depth buffer. With a free camera the user can reach cyclic occlusion and
//! exact depth ties by dragging, which would make a painter's-algorithm sort
//! genuinely fragile rather than only theoretically so.

pub mod camera;
pub mod figure;
pub mod gpu;
pub mod mesh;
pub mod params;
pub mod scene;
