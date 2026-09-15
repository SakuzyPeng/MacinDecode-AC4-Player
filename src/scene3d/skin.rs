//! Minecraft's 64×64 skin atlas, including slim arms and the outer clothing layer.
//!
//! Texel runs become cached flat quads. At this fixed resolution this keeps pixel
//! edges sharp without texture filtering or another GPU texture lifetime. Only
//! their pose and face shading change each frame.

use std::io::Cursor;

use image::{ImageDecoder, RgbaImage};
use serde::{Deserialize, Serialize};

use super::{figure::Figure, mesh::MeshBuilder, mesh::ViewContext, params};

pub const MAX_PNG_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BodyType {
    #[default]
    Steve,
    Alex,
}

impl BodyType {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Steve => "Steve (classic, 4 px arms)",
            Self::Alex => "Alex (slim, 3 px arms)",
        }
    }

    const fn arm_width(self) -> u8 {
        match self {
            Self::Steve => 4,
            Self::Alex => 3,
        }
    }
}

#[derive(Debug)]
pub struct Skin {
    image: RgbaImage,
    pub detected: BodyType,
    pub model: BodyType,
    patches: Vec<Patch>,
}

#[derive(Debug)]
struct Patch {
    corners: [[f32; 3]; 4],
    normal: [f32; 3],
    colour: [u8; 4],
    head: bool,
    overlay: bool,
}

impl Skin {
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() as u64 > MAX_PNG_BYTES {
            return Err("Skin PNG must be smaller than 1 MiB".into());
        }
        let mut decoder = image::codecs::png::PngDecoder::new(Cursor::new(bytes))
            .map_err(|error| format!("Cannot read skin PNG: {error}"))?;
        if !matches!(decoder.dimensions(), (64, 64 | 32)) {
            let (width, height) = decoder.dimensions();
            return Err(format!(
                "Skin must be a 64×64 PNG (or legacy 64×32); received {width}×{height}"
            ));
        }
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(64);
        limits.max_image_height = Some(64);
        limits.max_alloc = Some(MAX_PNG_BYTES);
        decoder
            .set_limits(limits)
            .map_err(|error| error.to_string())?;
        let image = image::DynamicImage::from_decoder(decoder)
            .map_err(|error| format!("Cannot decode skin PNG: {error}"))?
            .into_rgba8();
        let detected = detect_body_type(&image);
        let mut skin = Self {
            image,
            detected,
            model: detected,
            patches: Vec::new(),
        };
        skin.rebuild();
        Ok(skin)
    }

    pub fn legacy(&self) -> bool {
        self.image.height() == 32
    }

    pub fn set_model(&mut self, model: Option<BodyType>) {
        // The old atlas has no slim layout or separate left limbs.
        let model = if self.legacy() {
            BodyType::Steve
        } else {
            model.unwrap_or(self.detected)
        };
        if self.model != model {
            self.model = model;
            self.rebuild();
        }
    }

    fn rebuild(&mut self) {
        self.patches.clear();
        for part in model_parts(self.model, self.legacy()) {
            for face in faces(part.size, part.uv) {
                self.add_face(part, face);
            }
        }
    }

    fn add_face(&mut self, part: ModelPart, face: Face) {
        // Old RGB atlases often fill the unused hat region solid black. Vanilla
        // treats a completely opaque legacy hat rectangle as unused.
        if part.overlay && self.legacy() && legacy_hat_is_unused(&self.image) {
            return;
        }
        let [width, height] = face.extent;
        for row in 0..height {
            let mut column = 0;
            while column < width {
                let colour_at = |x| {
                    let mut colour = self
                        .image
                        .get_pixel(u32::from(face.uv[0] + x), u32::from(face.uv[1] + row))
                        .0;
                    if !part.overlay {
                        colour[3] = 255;
                    }
                    colour
                };
                let colour = colour_at(column);
                let start = column;
                column += 1;
                while column < width && colour_at(column) == colour {
                    column += 1;
                }
                if colour[3] == 0 {
                    continue;
                }
                let point = |x, y| face.point(part, x, y);
                let mut corners = [
                    point(start, row),
                    point(column, row),
                    point(column, row + 1),
                    point(start, row + 1),
                ];
                let mut normal = face.normal;
                // All atlas faces except the bottom have an inward-wound UV
                // rectangle in this Y-up, -Z-front coordinate system.
                if face.normal[1] >= 0.0 {
                    corners.reverse();
                }
                if part.mirror {
                    for corner in &mut corners {
                        corner[0] = 2.0 * part.centre[0] - corner[0];
                    }
                    normal[0] = -normal[0];
                    corners.reverse();
                }
                self.patches.push(Patch {
                    corners,
                    normal,
                    colour,
                    head: part.head,
                    overlay: part.overlay,
                });
            }
        }
    }

    pub fn draw(&self, mesh: &mut MeshBuilder, figure: Figure, floor: f32, view: &ViewContext) {
        let unit = params::FIGURE_SHOULDER_WIDTH / 16.0;
        let neck = floor + 24.0 * unit;
        let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let head = figure.head_axes();
        for patch in &self.patches {
            let axes = if patch.head { head } else { identity };
            let corners = patch
                .corners
                .map(|point| Figure::about_neck(point, unit, neck, axes));
            let normal = Figure::about_neck(patch.normal, 1.0, 0.0, axes);
            mesh.add_skin_quad(corners, normal, patch.colour, patch.overlay, view);
        }
    }
}

fn detect_body_type(image: &RgbaImage) -> BodyType {
    // A PNG has no mandatory model metadata. Check *both* arms' unused side
    // strips and bottom-face columns before making the slim inference. Do this
    // before making the base layer opaque, and ignore the optional jacket.
    let slim = image.height() == 64
        && [
            (54, 20, 2, 12),
            (50, 16, 2, 4),
            (46, 52, 2, 12),
            (42, 48, 2, 4),
        ]
        .into_iter()
        .all(|(x, y, width, height)| {
            (y..y + height).all(|y| (x..x + width).all(|x| image.get_pixel(x, y)[3] == 0))
        });
    if slim {
        BodyType::Alex
    } else {
        BodyType::Steve
    }
}

fn legacy_hat_is_unused(image: &RgbaImage) -> bool {
    (0..16).all(|y| (32..64).all(|x| image.get_pixel(x, y)[3] == 255))
}

#[derive(Clone, Copy)]
struct ModelPart {
    centre: [f32; 3],
    size: [u8; 3],
    uv: [u8; 2],
    head: bool,
    overlay: bool,
    mirror: bool,
}

fn model_parts(model: BodyType, legacy: bool) -> Vec<ModelPart> {
    let arm = model.arm_width();
    let shoulder = 4.0 + f32::from(arm) / 2.0;
    let base = [
        ([0.0, 4.0, 0.0], [8, 8, 8], [0, 0], [32, 0]),
        ([0.0, -6.0, 0.0], [8, 12, 4], [16, 16], [16, 32]),
        ([shoulder, -6.0, 0.0], [arm, 12, 4], [40, 16], [40, 32]),
        ([-shoulder, -6.0, 0.0], [arm, 12, 4], [32, 48], [48, 48]),
        ([2.0, -18.0, 0.0], [4, 12, 4], [0, 16], [0, 32]),
        ([-2.0, -18.0, 0.0], [4, 12, 4], [16, 48], [0, 48]),
    ];
    let mut parts = Vec::with_capacity(12);
    for (index, (centre, size, mut uv, overlay_uv)) in base.into_iter().enumerate() {
        let mirror = legacy && matches!(index, 3 | 5);
        if mirror {
            uv = if index == 3 { [40, 16] } else { [0, 16] };
        }
        let part = ModelPart {
            centre,
            size,
            uv,
            head: index == 0,
            overlay: false,
            mirror,
        };
        parts.push(part);
        if !legacy || part.head {
            parts.push(ModelPart {
                uv: overlay_uv,
                overlay: true,
                ..part
            });
        }
    }
    parts
}

#[derive(Clone, Copy)]
struct Face {
    uv: [u8; 2],
    extent: [u8; 2],
    normal: [f32; 3],
    right: [f32; 3],
    down: [f32; 3],
}

impl Face {
    fn point(&self, part: ModelPart, x: u8, y: u8) -> [f32; 3] {
        let inflate = if !part.overlay {
            0.0
        } else if part.head {
            1.0
        } else {
            0.5
        };
        let u = f32::from(x) / f32::from(self.extent[0]) - 0.5;
        let v = f32::from(y) / f32::from(self.extent[1]) - 0.5;
        std::array::from_fn(|axis| {
            let size = f32::from(part.size[axis]) + inflate;
            part.centre[axis]
                + size * (self.normal[axis] * 0.5 + self.right[axis] * u + self.down[axis] * v)
        })
    }
}

fn faces([width, height, depth]: [u8; 3], [origin_x, origin_y]: [u8; 2]) -> [Face; 6] {
    [
        Face {
            uv: [origin_x + depth, origin_y],
            extent: [width, depth],
            normal: [0.0, 1.0, 0.0],
            right: [-1.0, 0.0, 0.0],
            down: [0.0, 0.0, -1.0],
        },
        Face {
            uv: [origin_x + depth + width, origin_y],
            extent: [width, depth],
            normal: [0.0, -1.0, 0.0],
            right: [-1.0, 0.0, 0.0],
            down: [0.0, 0.0, -1.0],
        },
        Face {
            uv: [origin_x, origin_y + depth],
            extent: [depth, height],
            normal: [1.0, 0.0, 0.0],
            right: [0.0, 0.0, -1.0],
            down: [0.0, -1.0, 0.0],
        },
        Face {
            uv: [origin_x + depth, origin_y + depth],
            extent: [width, height],
            normal: [0.0, 0.0, -1.0],
            right: [-1.0, 0.0, 0.0],
            down: [0.0, -1.0, 0.0],
        },
        Face {
            uv: [origin_x + depth + width, origin_y + depth],
            extent: [depth, height],
            normal: [-1.0, 0.0, 0.0],
            right: [0.0, 0.0, 1.0],
            down: [0.0, -1.0, 0.0],
        },
        Face {
            uv: [origin_x + 2 * depth + width, origin_y + depth],
            extent: [width, height],
            normal: [0.0, 0.0, 1.0],
            right: [1.0, 0.0, 0.0],
            down: [0.0, -1.0, 0.0],
        },
    ]
}

#[cfg(test)]
pub(crate) mod tests;
