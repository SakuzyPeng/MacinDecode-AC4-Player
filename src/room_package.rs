//! A room package's geometry, as the settings page shows it.
//!
//! The room renderer models a shoebox room around a fixed listening position.
//! Its manifest (`room.json`, schema `mradm.room`) carries that geometry in the
//! clear beside the component hashes, so the page can say what room this is and
//! hold it against the room-geometry recommendations of ITU-R BS.1116-3 and
//! EBU Tech 3276 (with Supplement 1 for multichannel) without loading a single
//! filter.
//!
//! Only geometry is judged here. Whether the decay and the early reflections
//! meet the same documents is a measurement of the package's filters, which
//! `MacinRender`'s own acoustic check makes and reports; reading a nominal
//! reverberation time off the volume is the target, not the result, and is
//! drawn as one. Background noise, the operational room response and the
//! listening level depend on the headphones and the room the listener actually
//! sits in, so a model cannot claim them at all.

use std::io::Read;
use std::path::Path;

use serde::Deserialize;

/// A manifest is a few kilobytes of JSON; anything near this is not one.
const MAX_MANIFEST_BYTES: u64 = 1 << 20;

/// The reference volume `V0` of the nominal reverberation time.
const REFERENCE_VOLUME: f64 = 100.0;

/// How far either side of the nominal reverberation time the 200 Hz – 4 kHz
/// part of the tolerance mask reaches.
pub const REVERBERATION_TOLERANCE: f64 = 0.05;

/// A listener this close to the lateral centre line counts as on it.
const CENTRE_LINE_TOLERANCE: f64 = 0.01;

/// The elevations the Scene-unit fit is reported at: the ear-level ring, the
/// upper layers of 7.1.4 / 9.1.6 / 22.2, straight up, and 22.2's lower layer.
pub const FIT_ELEVATIONS: [i32; 5] = [0, 30, 45, 90, -30];

#[derive(Deserialize)]
struct Manifest {
    schema: String,
    version: u32,
    id: String,
    #[serde(default)]
    name: String,
    dimensions: [f64; 3],
    listener: [f64; 3],
    reference_distance: f64,
    minimum_distance: f64,
    boundary_clearance: f64,
}

/// The parts of a room package that describe the room rather than its filters.
///
/// Room coordinates are the renderer's: x is front–back, y left–right and z
/// up, in metres from a corner.
#[derive(Debug, Clone, PartialEq)]
pub struct RoomGeometry {
    pub id: String,
    pub name: String,
    pub dimensions: [f64; 3],
    pub listener: [f64; 3],
    /// The distance at which propagation is unity gain.
    pub reference_distance: f64,
    /// Sources closer than this are pushed out to it.
    pub minimum_distance: f64,
    /// Sources are kept at least this far inside every wall.
    pub boundary_clearance: f64,
}

/// One recommendation, held against this room.
#[derive(Debug, Clone, PartialEq)]
pub struct Criterion {
    pub label: &'static str,
    /// The room's own numbers for it, so a reader can check the verdict.
    pub detail: String,
    pub met: bool,
}

/// How far a source can sit from the listener in one direction before the
/// renderer pulls it back inside the walls.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reach {
    pub elevation: i32,
    pub metres: f64,
}

impl RoomGeometry {
    pub fn read(path: &Path) -> Result<Self, String> {
        let mut text = String::new();
        std::fs::File::open(path)
            .and_then(|file| file.take(MAX_MANIFEST_BYTES + 1).read_to_string(&mut text))
            .map_err(|error| error.to_string())?;
        if text.len() as u64 > MAX_MANIFEST_BYTES {
            return Err("This file is too large to be a room manifest".into());
        }
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let manifest: Manifest = serde_json::from_str(text)
            .map_err(|error| format!("This is not a room manifest: {error}"))?;
        if manifest.schema != "mradm.room" {
            return Err(format!(
                "This is a {} file, not a room manifest",
                manifest.schema
            ));
        }
        if manifest.version != 1 {
            return Err(format!(
                "Room manifest version {} is not supported",
                manifest.version
            ));
        }
        let finite = |values: &[f64]| values.iter().all(|value| value.is_finite());
        if !finite(&manifest.dimensions) || manifest.dimensions.iter().any(|side| *side <= 0.0) {
            return Err("The room's dimensions must be positive".into());
        }
        if !finite(&[
            manifest.reference_distance,
            manifest.minimum_distance,
            manifest.boundary_clearance,
        ]) || manifest.reference_distance <= 0.0
            || manifest.minimum_distance <= 0.0
            || manifest.boundary_clearance < 0.0
        {
            return Err("The room's distances must be positive".into());
        }
        if !finite(&manifest.listener)
            || manifest
                .listener
                .iter()
                .zip(manifest.dimensions)
                .any(|(position, side)| {
                    *position <= manifest.boundary_clearance
                        || *position >= side - manifest.boundary_clearance
                })
        {
            return Err("The listening position is not inside the room".into());
        }
        Ok(Self {
            id: manifest.id,
            name: manifest.name,
            dimensions: manifest.dimensions,
            listener: manifest.listener,
            reference_distance: manifest.reference_distance,
            minimum_distance: manifest.minimum_distance,
            boundary_clearance: manifest.boundary_clearance,
        })
    }

    pub fn floor_area(&self) -> f64 {
        self.dimensions[0] * self.dimensions[1]
    }

    pub fn volume(&self) -> f64 {
        self.floor_area() * self.dimensions[2]
    }

    /// `Tm = 0.25 (V / V0)^(1/3)` seconds: the reverberation time both
    /// documents centre their tolerance mask on.
    pub fn nominal_reverberation(&self) -> f64 {
        0.25 * (self.volume() / REFERENCE_VOLUME).cbrt()
    }

    /// Length, width and height in the documents' sense: length is the longer
    /// side of the floor, whichever way the listener faces.
    fn proportions(&self) -> (f64, f64, f64) {
        let [front, side, height] = self.dimensions;
        (front.max(side), front.min(side), height)
    }

    pub fn criteria(&self) -> [Criterion; 5] {
        let (length, width, height) = self.proportions();
        let area = self.floor_area();
        let (l_h, w_h, l_w) = (length / height, width / height, length / width);
        let (lower, upper) = (1.1 * w_h, 4.5 * w_h - 4.0);
        let near_integer = [("l/w", l_w), ("l/h", l_h), ("w/h", w_h)]
            .into_iter()
            .filter_map(|(name, ratio)| {
                let integer = ratio.round();
                let off = (ratio - integer).abs() / integer;
                (integer >= 1.0 && off < 0.05)
                    .then(|| format!("{name} {ratio:.2} is {:.1} % from {integer}", off * 100.0))
            })
            .collect::<Vec<_>>();
        let off_centre = (self.listener[1] - self.dimensions[1] / 2.0).abs();
        [
            Criterion {
                label: "Floor area 30–70 m² for multichannel",
                detail: format!("{area:.1} m²"),
                met: (30.0..=70.0).contains(&area),
            },
            Criterion {
                label: "1.1·w/h ≤ l/h ≤ 4.5·w/h − 4",
                detail: format!("{lower:.2} ≤ {l_h:.2} ≤ {upper:.2}"),
                met: lower <= l_h && l_h <= upper,
            },
            Criterion {
                label: "l/h < 3 and w/h < 3",
                detail: format!("l/h {l_h:.2} · w/h {w_h:.2}"),
                met: l_h < 3.0 && w_h < 3.0,
            },
            Criterion {
                label: "No ratio within ±5 % of an integer",
                detail: if near_integer.is_empty() {
                    format!("l/w {l_w:.2} · l/h {l_h:.2} · w/h {w_h:.2}")
                } else {
                    near_integer.join(" · ")
                },
                met: near_integer.is_empty(),
            },
            Criterion {
                label: "Listener on the left–right centre line",
                detail: format!("{off_centre:.2} m off centre"),
                met: off_centre <= CENTRE_LINE_TOLERANCE,
            },
        ]
    }

    /// The farthest a source can sit in each of [`FIT_ELEVATIONS`], at its
    /// worst azimuth, before the walls pull it in.
    pub fn reach(&self) -> [Reach; FIT_ELEVATIONS.len()] {
        let [x, y, z] = self.listener;
        let [front, side, height] = self.dimensions;
        let clearance = self.boundary_clearance;
        let around = x.min(front - x).min(y).min(side - y) - clearance;
        let above = height - z - clearance;
        let below = z - clearance;
        FIT_ELEVATIONS.map(|elevation| {
            let (sin, cos) = f64::from(elevation).to_radians().sin_cos();
            let mut metres = f64::INFINITY;
            if cos > 1e-9 {
                metres = metres.min(around / cos);
            }
            if sin.abs() > 1e-9 {
                metres = metres.min(if sin > 0.0 { above } else { below } / sin.abs());
            }
            Reach { elevation, metres }
        })
    }

    /// How much louder than the room's unity reference a source at one Scene
    /// unit plays, before any wall moves it.
    pub fn unit_distance_gain_db(&self, metres_per_unit: f64) -> f64 {
        20.0 * (self.reference_distance / metres_per_unit.max(self.minimum_distance)).log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The geometry of `MacinRender`'s reference preset,
    /// `reference-7x6x3.2-corrected-v1`, as its manifest states it.
    const REFERENCE: &str = r#"{
        "schema": "mradm.room", "version": 1, "kind": "hybrid-shoebox-v1",
        "id": "reference-7x6x3.2-corrected-v1",
        "name": "Corrected reference listening room", "sample_rate": 48000,
        "dimensions": [7.0, 6.0, 3.2], "listener": [3.4, 3.0, 1.2],
        "speed_of_sound": 344.02, "reference_distance": 2.1,
        "minimum_distance": 0.25, "boundary_clearance": 0.15,
        "assets": {}
    }"#;

    fn room(dimensions: [f64; 3], listener: [f64; 3]) -> RoomGeometry {
        RoomGeometry {
            dimensions,
            listener,
            ..RoomGeometry::parse(REFERENCE).unwrap()
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 5e-3
    }

    #[test]
    fn the_reference_preset_meets_every_geometric_criterion() {
        let room = RoomGeometry::parse(REFERENCE).unwrap();
        assert_eq!(room.name, "Corrected reference listening room");
        assert!(close(room.floor_area(), 42.0) && close(room.volume(), 134.4));
        assert!(close(room.nominal_reverberation(), 0.276));
        for criterion in room.criteria() {
            assert!(criterion.met, "{criterion:?}");
        }
        assert_eq!(room.criteria()[1].detail, "2.06 ≤ 2.19 ≤ 4.44");
    }

    #[test]
    fn each_criterion_fails_on_the_room_that_breaks_it() {
        // Too small a floor for a multichannel room.
        assert!(!room([6.0, 4.8, 2.8], [3.0, 2.4, 1.2]).criteria()[0].met);
        // Long and low: l/h above both its bound and 3.
        let corridor = room([10.0, 4.0, 2.4], [5.0, 2.0, 1.2]).criteria();
        assert!(!corridor[1].met && !corridor[2].met);
        // w/h of 1.94 is within 5 % of 2.
        let near_two = room([9.0, 7.0, 3.6], [4.3, 3.5, 1.2]).criteria();
        assert!(!near_two[3].met);
        assert!(near_two[3].detail.starts_with("w/h 1.94 is 2.8 % from 2"));
        // A square floor is the integer ratio 1.
        assert!(!room([6.0, 6.0, 2.8], [3.0, 3.0, 1.2]).criteria()[3].met);
        // Off the centre line.
        assert!(!room([7.0, 6.0, 3.2], [3.4, 2.8, 1.2]).criteria()[4].met);
    }

    #[test]
    fn length_is_the_longer_side_whichever_way_the_listener_faces() {
        let deep = room([7.0, 6.0, 3.2], [3.4, 3.0, 1.2]).criteria();
        let wide = room([6.0, 7.0, 3.2], [3.0, 3.5, 1.2]).criteria();
        assert_eq!(deep[1].detail, wide[1].detail);
    }

    #[test]
    fn the_reach_is_set_by_the_nearest_wall_floor_or_ceiling() {
        let reach = RoomGeometry::parse(REFERENCE).unwrap().reach();
        let metres = reach.map(|reach| reach.metres);
        // 3.0 m to the side walls, 2.0 m to the ceiling, 1.2 m to the floor,
        // each less the 0.15 m clearance.
        for (got, want) in metres.into_iter().zip([2.85, 3.291, 2.616, 1.85, 2.1]) {
            assert!(close(got, want), "{metres:?}");
        }
    }

    #[test]
    fn unit_distance_gain_is_measured_against_the_reference_distance() {
        let room = RoomGeometry::parse(REFERENCE).unwrap();
        assert!(close(room.unit_distance_gain_db(2.1), 0.0));
        assert!(close(room.unit_distance_gain_db(1.5), 2.923));
        // Closer than the minimum, a source is pushed out to it.
        assert!(close(
            room.unit_distance_gain_db(0.01),
            room.unit_distance_gain_db(0.25)
        ));
    }

    #[test]
    fn a_manifest_that_is_not_a_room_is_refused_with_a_reason() {
        for (text, reason) in [
            ("{}".to_owned(), "not a room manifest"),
            (
                REFERENCE.replace("mradm.room\"", "mradm.hrir\""),
                "mradm.hrir file",
            ),
            (
                REFERENCE.replace("\"version\": 1", "\"version\": 2"),
                "version 2",
            ),
            (
                REFERENCE.replace("[7.0, 6.0, 3.2]", "[7.0, 0.0, 3.2]"),
                "dimensions",
            ),
            (
                REFERENCE.replace("[3.4, 3.0, 1.2]", "[3.4, 6.0, 1.2]"),
                "listening position",
            ),
        ] {
            let error = RoomGeometry::parse(&text).unwrap_err();
            assert!(error.contains(reason), "{error}");
        }
    }

    #[test]
    fn reading_stops_at_the_size_of_a_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("room.json");
        std::fs::write(&path, REFERENCE).unwrap();
        assert!(RoomGeometry::read(&path).is_ok());
        let padded = format!(
            "{REFERENCE}{}",
            " ".repeat(usize::try_from(MAX_MANIFEST_BYTES).unwrap())
        );
        std::fs::write(&path, padded).unwrap();
        assert!(RoomGeometry::read(&path).unwrap_err().contains("too large"));
    }
}
