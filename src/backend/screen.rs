//! Screen geometry for the room renderer's screen mapping.
//!
//! Objects that are screen-referenced (`screenRef`) were placed relative to the
//! screen they were mixed against — the reference screen — and the renderer
//! warps them onto the screen they are watched on — the reproduction screen —
//! by BS.2127-1 §7.3.3's polar edges. Without a reproduction screen nothing
//! moves. Both are fixed for the life of an output session.
//!
//! [`ScreenGeometry::edges`] transcribes `MacinRender`'s own check
//! (`mradm-dsp` `room::screen::edges`) so the settings page can refuse a
//! geometry before a session ever sees it, and say which edge is wrong. The
//! renderer keeps the final word: this is a form check, not the mapping.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ScreenCoordinates {
    /// Centre as azimuth, elevation and distance; width in degrees.
    #[default]
    #[serde(rename = "polar")]
    Polar,
    /// Centre as ADM x/right, y/front, z/up; width along X.
    #[serde(rename = "cartesian")]
    Cartesian,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScreenGeometry {
    pub coordinates: ScreenCoordinates,
    pub centre: [f64; 3],
    pub width: f64,
    pub aspect_ratio: f64,
}

/// The standard reference screen of ITU-R BS.2076 / BS.2127: straight ahead,
/// 58° wide, 1.78:1. It is what the renderer assumes when none is given.
impl Default for ScreenGeometry {
    fn default() -> Self {
        Self {
            coordinates: ScreenCoordinates::Polar,
            centre: [0.0, 0.0, 1.0],
            width: 58.0,
            aspect_ratio: 1.78,
        }
    }
}

/// A screen's four edges as directions from the listener, in degrees, with
/// ADM's positive azimuth to the left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenEdges {
    pub right: f64,
    pub left: f64,
    pub bottom: f64,
    pub top: f64,
}

fn norm(v: [f64; 3]) -> f64 {
    v[0].hypot(v[1]).hypot(v[2])
}

/// Azimuth and elevation of a direction, ADM convention.
fn angles(v: [f64; 3]) -> [f64; 2] {
    [
        (-v[0]).atan2(v[1]).to_degrees(),
        v[2].atan2(v[0].hypot(v[1])).to_degrees(),
    ]
}

impl ScreenGeometry {
    pub fn edges(&self) -> Result<ScreenEdges, &'static str> {
        if !self.aspect_ratio.is_finite() || self.aspect_ratio <= 0.0 {
            return Err("The aspect ratio must be positive");
        }
        if !self.width.is_finite() || self.width <= 0.0 {
            return Err("The width must be positive");
        }
        if self.centre.iter().any(|value| !value.is_finite()) {
            return Err("The centre must be finite");
        }
        let (centre, horizontal, vertical) = match self.coordinates {
            ScreenCoordinates::Polar => {
                let [azimuth, elevation, distance] = self.centre;
                if !(-180.0..=180.0).contains(&azimuth) {
                    return Err("The azimuth must lie within ±180°");
                }
                if !(-90.0..90.0).contains(&elevation) {
                    return Err("The elevation must lie in [-90°, 90°)");
                }
                if distance <= 0.0 {
                    return Err("The distance must be positive");
                }
                if self.width >= 180.0 {
                    return Err("A polar width must be below 180°");
                }
                let (sa, ca) = azimuth.to_radians().sin_cos();
                let (se, ce) = elevation.to_radians().sin_cos();
                let width = (self.width / 2.0).to_radians().tan();
                let height = width / self.aspect_ratio;
                (
                    [-sa * ce, ca * ce, se],
                    [ca * width, sa * width, 0.0],
                    [sa * se * height, -ca * se * height, ce * height],
                )
            }
            ScreenCoordinates::Cartesian => {
                let length = norm(self.centre);
                if !length.is_finite() || length <= 1e-15 {
                    return Err("The centre must be away from the listener");
                }
                let width = self.width / length / 2.0;
                (
                    self.centre.map(|value| value / length),
                    [width, 0.0, 0.0],
                    [0.0, 0.0, width / self.aspect_ratio],
                )
            }
        };
        let points: [[f64; 3]; 4] = [
            std::array::from_fn(|i| centre[i] - horizontal[i]),
            std::array::from_fn(|i| centre[i] + horizontal[i]),
            std::array::from_fn(|i| centre[i] - vertical[i]),
            std::array::from_fn(|i| centre[i] + vertical[i]),
        ];
        if points
            .iter()
            .any(|point| point.iter().any(|value| !value.is_finite()) || norm(*point) <= 1e-15)
        {
            return Err("An edge of the screen passes through the listener");
        }
        let edges = ScreenEdges {
            right: angles(points[1])[0],
            left: angles(points[0])[0],
            bottom: angles(points[2])[1],
            top: angles(points[3])[1],
        };
        if !(edges.right > -180.0 && edges.right < edges.left && edges.left < 180.0) {
            return Err("The screen's sides must not reach directly behind");
        }
        if !(edges.bottom > -90.0 && edges.bottom < edges.top && edges.top < 90.0) {
            return Err("The screen's top and bottom must not reach straight up or down");
        }
        Ok(edges)
    }

    /// The same screen in the other coordinate system: exact for a screen
    /// straight ahead, and the same centre and angular width otherwise.
    #[must_use]
    pub fn converted(self, coordinates: ScreenCoordinates) -> Self {
        let centre = match (self.coordinates, coordinates) {
            (from, to) if from == to => return self,
            (ScreenCoordinates::Polar, _) => {
                let [azimuth, elevation, distance] = self.centre;
                let (sa, ca) = azimuth.to_radians().sin_cos();
                let (se, ce) = elevation.to_radians().sin_cos();
                [-sa * ce * distance, ca * ce * distance, se * distance]
            }
            (ScreenCoordinates::Cartesian, _) => {
                let [azimuth, elevation] = angles(self.centre);
                [azimuth, elevation, norm(self.centre)]
            }
        };
        let distance = match coordinates {
            ScreenCoordinates::Polar => centre[2],
            ScreenCoordinates::Cartesian => norm(centre),
        };
        let width = match coordinates {
            ScreenCoordinates::Polar => 2.0 * (self.width / distance / 2.0).atan().to_degrees(),
            ScreenCoordinates::Cartesian => 2.0 * distance * (self.width / 2.0).to_radians().tan(),
        };
        Self {
            coordinates,
            centre,
            width,
            aspect_ratio: self.aspect_ratio,
        }
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "coordinates": match self.coordinates {
                ScreenCoordinates::Polar => "polar",
                ScreenCoordinates::Cartesian => "cartesian",
            },
            "centre": self.centre,
            "width": self.width,
            "aspect_ratio": self.aspect_ratio,
        })
    }
}

/// The two screens as the room renderer's settings hold them.
///
/// Each keeps its geometry while switched off, so turning a screen off and on
/// again does not lose what was typed into it.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ScreenSettings {
    /// Off means no screen mapping at all: positions reach the room unchanged.
    pub reproduction_enabled: bool,
    pub reproduction: ScreenGeometry,
    /// Off means the standard reference screen.
    pub reference_custom: bool,
    pub reference: ScreenGeometry,
}

impl ScreenSettings {
    /// Whether the renderer would move anything: a reproduction screen must
    /// be given, and must differ from the reference it maps from.
    pub fn maps(&self) -> bool {
        self.reproduction_enabled && self.reproduction.edges() != self.reference().edges()
    }
    pub fn reference(&self) -> ScreenGeometry {
        if self.reference_custom {
            self.reference
        } else {
            ScreenGeometry::default()
        }
    }
    /// The first screen that the renderer would refuse, named.
    pub fn problem(&self) -> Option<String> {
        let named = |name: &str, screen: &ScreenGeometry| {
            screen
                .edges()
                .err()
                .map(|error| format!("{name} screen: {error}"))
        };
        if self.reference_custom
            && let Some(problem) = named("Reference", &self.reference)
        {
            return Some(problem);
        }
        if self.reproduction_enabled {
            return named("Reproduction", &self.reproduction);
        }
        None
    }
    /// `mradm.screen-config` v1, as `adm_render_options_set_screen_config_json`
    /// and the `mradm --screen-config` option read it. A screen that is off is
    /// left out, which is what selects the renderer's own default.
    pub fn config_json(&self) -> String {
        let mut config = serde_json::json!({
            "schema": "mradm.screen-config",
            "version": 1,
        });
        if self.reference_custom {
            config["reference"] = self.reference.json();
        }
        if self.reproduction_enabled {
            config["reproduction"] = self.reproduction.json();
        }
        config.to_string()
    }
    /// Drop what a stored file could carry that the page would never apply:
    /// a geometry the renderer refuses goes back to the standard screen.
    pub fn validated(mut self) -> Self {
        for screen in [&mut self.reproduction, &mut self.reference] {
            if screen.edges().is_err() {
                *screen = ScreenGeometry::default();
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn the_standard_screen_spans_its_own_width_and_height() {
        let edges = ScreenGeometry::default().edges().unwrap();
        assert!(close(edges.left, 29.0) && close(edges.right, -29.0));
        let half_height = ((29.0_f64).to_radians().tan() / 1.78).atan().to_degrees();
        assert!(close(edges.top, half_height) && close(edges.bottom, -half_height));
    }

    #[test]
    fn geometry_the_renderer_refuses_is_refused_here() {
        let polar = ScreenGeometry::default();
        for broken in [
            ScreenGeometry {
                width: 180.0,
                ..polar
            },
            ScreenGeometry {
                aspect_ratio: 0.0,
                ..polar
            },
            ScreenGeometry {
                centre: [0.0, 90.0, 1.0],
                ..polar
            },
            ScreenGeometry {
                centre: [181.0, 0.0, 1.0],
                ..polar
            },
            ScreenGeometry {
                centre: [0.0, 0.0, 0.0],
                ..polar
            },
            // Straight behind, its sides would straddle ±180°.
            ScreenGeometry {
                centre: [180.0, 0.0, 1.0],
                ..polar
            },
            ScreenGeometry {
                coordinates: ScreenCoordinates::Cartesian,
                centre: [0.0, 0.0, 0.0],
                ..polar
            },
            ScreenGeometry {
                width: f64::NAN,
                ..polar
            },
        ] {
            assert!(broken.edges().is_err(), "{broken:?} should be refused");
        }
    }

    #[test]
    fn switching_coordinates_keeps_a_screen_straight_ahead() {
        let polar = ScreenGeometry {
            width: 90.0,
            ..ScreenGeometry::default()
        };
        let cartesian = polar.converted(ScreenCoordinates::Cartesian);
        assert_eq!(cartesian.coordinates, ScreenCoordinates::Cartesian);
        let (a, b) = (polar.edges().unwrap(), cartesian.edges().unwrap());
        for (x, y) in [
            (a.left, b.left),
            (a.right, b.right),
            (a.top, b.top),
            (a.bottom, b.bottom),
        ] {
            assert!(close(x, y), "{a:?} vs {b:?}");
        }
        let back = cartesian.converted(ScreenCoordinates::Polar);
        assert!(close(back.width, 90.0) && close(back.centre[2], 1.0));
    }

    #[test]
    fn the_config_names_only_the_screens_that_are_on() {
        let mut screens = ScreenSettings::default();
        let empty: serde_json::Value = serde_json::from_str(&screens.config_json()).unwrap();
        assert_eq!(
            empty,
            serde_json::json!({"schema": "mradm.screen-config", "version": 1})
        );
        assert!(!screens.maps());

        screens.reproduction_enabled = true;
        // The reproduction screen starts out as the reference: nothing moves.
        assert!(!screens.maps());
        screens.reproduction.width = 90.0;
        assert!(screens.maps());
        let config: serde_json::Value = serde_json::from_str(&screens.config_json()).unwrap();
        assert_eq!(
            config["reproduction"],
            serde_json::json!({
                "coordinates": "polar", "centre": [0.0, 0.0, 1.0],
                "width": 90.0, "aspect_ratio": 1.78,
            })
        );
        assert!(config.get("reference").is_none());

        screens.reference_custom = true;
        screens.reference.coordinates = ScreenCoordinates::Cartesian;
        let config: serde_json::Value = serde_json::from_str(&screens.config_json()).unwrap();
        assert_eq!(config["reference"]["coordinates"], "cartesian");
    }

    #[test]
    fn a_refused_screen_is_named_and_an_unused_one_is_not() {
        let mut screens = ScreenSettings {
            reference: ScreenGeometry {
                width: 0.0,
                ..ScreenGeometry::default()
            },
            ..ScreenSettings::default()
        };
        // The broken reference is not in use: the standard one is.
        assert_eq!(screens.problem(), None);
        screens.reference_custom = true;
        assert!(
            screens
                .problem()
                .is_some_and(|problem| problem.starts_with("Reference screen"))
        );
    }

    #[test]
    fn stored_screens_come_back_whole() {
        let screens = ScreenSettings {
            reproduction_enabled: true,
            reproduction: ScreenGeometry {
                coordinates: ScreenCoordinates::Cartesian,
                centre: [0.0, 2.0, 0.1],
                width: 1.5,
                aspect_ratio: 2.39,
            },
            ..ScreenSettings::default()
        };
        let reloaded: ScreenSettings =
            serde_json::from_str(&serde_json::to_string(&screens).unwrap()).unwrap();
        assert_eq!(reloaded, screens);
        let old: ScreenSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(old, ScreenSettings::default());
        for width in [f64::INFINITY, 200.0] {
            let broken = ScreenSettings {
                reference: ScreenGeometry {
                    width,
                    ..ScreenGeometry::default()
                },
                ..screens
            };
            let validated = broken.validated();
            assert_eq!(validated.reference, ScreenGeometry::default());
            assert_eq!(validated.reproduction, screens.reproduction);
        }
    }
}
