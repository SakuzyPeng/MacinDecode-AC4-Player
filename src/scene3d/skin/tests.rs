#![allow(
    clippy::float_cmp,
    reason = "unposed atlas coordinates are exact integer or half-integer values"
)]

use super::*;
use crate::scene3d::{
    camera::Camera,
    scene::{self, SceneInput},
};

pub(crate) fn sample_image(model: BodyType, legacy: bool) -> RgbaImage {
    let mut image = RgbaImage::new(64, if legacy { 32 } else { 64 });
    for part in model_parts(model, legacy)
        .into_iter()
        .filter(|part| !part.overlay)
    {
        for face in faces(part.size, part.uv) {
            for y in face.uv[1]..face.uv[1] + face.extent[1] {
                for x in face.uv[0]..face.uv[0] + face.extent[0] {
                    image.put_pixel(
                        u32::from(x),
                        u32::from(y),
                        image::Rgba([x + 64, y + 64, 120, 255]),
                    );
                }
            }
        }
    }
    image
}

pub(crate) fn png(image: &RgbaImage) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

fn centre(corners: [[f32; 3]; 4]) -> [f32; 3] {
    std::array::from_fn(|axis| corners.iter().map(|point| point[axis]).sum::<f32>() / 4.0)
}

#[test]
fn both_arm_margins_determine_the_model_before_base_alpha_is_normalized() {
    for model in [BodyType::Steve, BodyType::Alex] {
        let mut image = sample_image(model, false);
        // Decorations in the unused head/jacket regions do not decide arm width.
        image.put_pixel(63, 0, image::Rgba([255; 4]));
        let skin = Skin::decode(&png(&image)).unwrap();
        assert_eq!(skin.detected, model);
        assert_eq!(skin.model, model);
    }
    let mut image = sample_image(BodyType::Alex, false);
    image.put_pixel(47, 63, image::Rgba([255; 4]));
    assert_eq!(
        Skin::decode(&png(&image)).unwrap().detected,
        BodyType::Steve
    );
}

#[test]
fn slim_arms_touch_the_torso_and_keep_head_height_and_leg_dimensions() {
    let steve = model_parts(BodyType::Steve, false);
    let alex = model_parts(BodyType::Alex, false);
    for (a, s) in alex.iter().zip(&steve) {
        if a.size[0] == 3 {
            assert_eq!(s.size[0], 4);
            assert_eq!(a.centre[0].abs() - f32::from(a.size[0]) / 2.0, 4.0);
            assert_eq!(a.centre[0].abs() + f32::from(a.size[0]) / 2.0, 7.0);
        } else {
            assert_eq!(a.centre, s.centre);
            assert_eq!(a.size, s.size);
        }
    }
    let mut skin = Skin::decode(&png(&sample_image(BodyType::Alex, false))).unwrap();
    skin.set_model(Some(BodyType::Steve));
    assert_eq!(skin.model, BodyType::Steve);
    skin.set_model(None);
    assert_eq!(skin.model, BodyType::Alex);
}

#[test]
fn atlas_faces_are_upright_and_right_left_limbs_use_their_own_pixels() {
    let skin = Skin::decode(&png(&sample_image(BodyType::Steve, false))).unwrap();
    // Known atlas texels and their expected centres, in neck-relative units.
    for (uv, point, normal) in [
        ([8, 8], [3.5, 7.5, -4.0], [0.0, 0.0, -1.0]), // face, upper right
        ([15, 15], [-3.5, 0.5, -4.0], [0.0, 0.0, -1.0]),
        ([24, 8], [-3.5, 7.5, 4.0], [0.0, 0.0, 1.0]), // back of head
        ([8, 0], [3.5, 8.0, 3.5], [0.0, 1.0, 0.0]),   // crown
        ([16, 0], [3.5, 0.0, 3.5], [0.0, -1.0, 0.0]), // chin
        ([0, 8], [4.0, 7.5, 3.5], [1.0, 0.0, 0.0]),
        ([16, 8], [-4.0, 7.5, -3.5], [-1.0, 0.0, 0.0]),
        ([44, 20], [7.5, -0.5, -2.0], [0.0, 0.0, -1.0]), // right arm
        ([36, 52], [-4.5, -0.5, -2.0], [0.0, 0.0, -1.0]), // left arm
        ([4, 20], [3.5, -12.5, -2.0], [0.0, 0.0, -1.0]), // right leg
        ([20, 52], [-0.5, -12.5, -2.0], [0.0, 0.0, -1.0]), // left leg
    ] {
        let patch = skin
            .patches
            .iter()
            .find(|patch| patch.colour == [uv[0] + 64, uv[1] + 64, 120, 255])
            .unwrap();
        assert_eq!(centre(patch.corners), point, "atlas {uv:?}");
        assert_eq!(patch.normal, normal);
    }
}

#[test]
fn legacy_left_limbs_are_reflections_and_all_faces_have_outward_winding() {
    for legacy in [true, false] {
        let skin = Skin::decode(&png(&sample_image(BodyType::Steve, legacy))).unwrap();
        for patch in &skin.patches {
            let a: [f32; 3] = std::array::from_fn(|i| patch.corners[1][i] - patch.corners[0][i]);
            let b: [f32; 3] = std::array::from_fn(|i| patch.corners[2][i] - patch.corners[0][i]);
            let cross = [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ];
            assert!(
                cross
                    .iter()
                    .zip(patch.normal)
                    .map(|(a, b)| a * b)
                    .sum::<f32>()
                    > 0.0
            );
        }
        if legacy {
            assert_eq!(skin.detected, BodyType::Steve);
            let feet: Vec<_> = skin
                .patches
                .iter()
                .filter(|patch| patch.colour == [68, 84, 120, 255])
                .collect();
            assert_eq!(feet.len(), 2);
            assert_eq!(centre(feet[0].corners), [3.5, -12.5, -2.0]);
            assert_eq!(centre(feet[1].corners), [-3.5, -12.5, -2.0]);
        }
    }
}

#[test]
fn outer_layer_holes_and_partial_alpha_survive_head_tracking() {
    let mut image = sample_image(BodyType::Alex, false);
    image.put_pixel(40, 8, image::Rgba([255, 0, 0, 128]));
    let skin = Skin::decode(&png(&image)).unwrap();
    assert_eq!(skin.patches.iter().filter(|patch| patch.overlay).count(), 1);
    let figure = Figure {
        head_yaw: 35.0,
        head_pitch: 20.0,
        head_roll: -15.0,
    };
    let mut mesh = MeshBuilder::default();
    scene::build(
        &mut mesh,
        &Camera::default(),
        600.0,
        SceneInput {
            figure,
            skin: Some(&skin),
            ..Default::default()
        },
    );
    assert_eq!(
        mesh.transparent.len(),
        2,
        "both sides of the one translucent texel"
    );
    let patch = skin.patches.iter().find(|patch| patch.overlay).unwrap();
    let unit = params::FIGURE_SHOULDER_WIDTH / 16.0;
    let expected = patch.corners.map(|corner| {
        Figure::about_neck(
            corner,
            unit,
            params::ROOM_FLOOR_Y + 24.0 * unit,
            figure.head_axes(),
        )
    });
    for vertex in mesh.transparent.iter().flatten() {
        assert_eq!(vertex.colour[3], 128);
        assert!(expected.contains(&vertex.position));
    }
}

#[test]
fn malformed_oversized_and_wrong_dimension_images_are_rejected() {
    assert!(Skin::decode(b"not a PNG").is_err());
    assert!(Skin::decode(&vec![0; usize::try_from(MAX_PNG_BYTES).unwrap() + 1]).is_err());
    for (width, height) in [(32, 32), (64, 48), (128, 128)] {
        let error = Skin::decode(&png(&RgbaImage::new(width, height))).unwrap_err();
        assert!(error.contains("64×64"), "{error}");
    }
    let mut legacy = Skin::decode(&png(&sample_image(BodyType::Steve, true))).unwrap();
    legacy.set_model(Some(BodyType::Alex));
    assert_eq!(legacy.model, BodyType::Steve);
}
