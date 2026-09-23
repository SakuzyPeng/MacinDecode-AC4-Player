//! Which directions the magnetic field has been read from during magnetic
//! calibration, and which way to turn the headset to reach the rest.
//!
//! The player cannot see the sensor's calibration. It can see the field the
//! sensor reports, and everything here is a statement about those readings —
//! which directions they cover, where their centre sits, how far their
//! magnitude spreads — never a verdict on the sensor.
//! `docs/POSEBRIDGE-MAGNETIC.md` carries the flow this serves and the questions
//! still open on the interface that will feed it.
//!
//! The grid is [`COLUMNS`] of azimuth by [`ROWS`] of sin(elevation). Steps
//! uniform in sin(elevation) give every cell the same solid angle, 4π/48, so a
//! filled grid is a covered sphere rather than a picture that merely looks
//! full. Its two axes are also the two motions a pair of hands makes — turning
//! across, tipping down — which is why it is a grid and not the polar disc the
//! first mockup drew: an empty column already says "turn further that way".
//!
//! One [`Coverage`] holds one sweep. Nothing here reaches a device or a
//! listener.
use super::mounting::{self, Motion};
use std::collections::VecDeque;
use std::f64::consts::{PI, TAU};

/// Columns of azimuth, 30° each, counted from directly behind. Azimuth runs
/// from forward toward right, so forward is the middle of the grid and behind
/// is both of its edges.
pub const COLUMNS: usize = 12;
/// Rows of sin(elevation), the top row first: +30..+90°, 0..+30°, −30..0°,
/// −90..−30°.
pub const ROWS: usize = 4;
pub const CELLS: usize = COLUMNS * ROWS;
/// A column's width in radians and a row's height in sin(elevation), beside
/// the counts they divide. The test that fills every cell from an even sphere
/// fails if the two pairs ever disagree.
const COLUMN_WIDTH: f64 = TAU / 12.0;
const ROW_HEIGHT: f64 = 2.0 / 4.0;

/// Readings one sweep keeps: minutes at the 5–10 Hz the grid needs. A longer
/// sweep keeps its latest.
const MAX_READINGS: usize = 4096;
/// Readings before anything is binned. A direction is read from a centre, and
/// a handful of readings does not locate one.
const MIN_READINGS: usize = 16;
/// How far the readings must spread out of a plane before a sphere is fitted
/// through them, as [`span`] measures it. Provisional until measured on a real
/// headset.
const MIN_SPAN: f64 = 0.05;

/// A field reading in headset axes — X right, Y forward, Z up — in whatever
/// unit the interface delivers, as long as one sweep keeps to one.
pub type Field = [f64; 3];

/// One sweep's readings, oldest first.
#[derive(Debug, Default)]
pub struct Coverage {
    readings: VecDeque<Field>,
}

impl Coverage {
    /// Keep a reading, dropping the oldest past [`MAX_READINGS`]. A non-finite
    /// reading is refused: one NaN would move the centre of the whole sweep.
    pub fn push(&mut self, field: Field) -> bool {
        if !field.iter().all(|value| value.is_finite()) {
            return false;
        }
        if self.readings.len() == MAX_READINGS {
            self.readings.pop_front();
        }
        self.readings.push_back(field);
        true
    }

    /// Start the next sweep.
    pub fn clear(&mut self) {
        self.readings.clear();
    }

    /// What the panel draws, recomputed from every kept reading.
    ///
    /// Each reading is binned from the centre as it stands now, so a cell
    /// filled in the first seconds can move while the centre settles. That is
    /// the honest picture: it was the earlier binning that was wrong.
    pub fn snapshot(&self) -> Snapshot {
        let mut snapshot = Snapshot {
            counts: [0; CELLS],
            readings: self.readings.len(),
            centre: None,
            spread: None,
            next: None,
        };
        if self.readings.len() < MIN_READINGS {
            return snapshot;
        }
        let located = centre(&self.readings);
        let origin = located.point();
        let mut magnitudes: [Vec<f64>; CELLS] = std::array::from_fn(|_| Vec::new());
        for &reading in &self.readings {
            if let Some(cell) = cell_of(sub(reading, origin)) {
                snapshot.counts[cell] += 1;
                magnitudes[cell].push(norm(reading));
            }
        }
        snapshot.spread = spread(magnitudes);
        snapshot.next = self
            .readings
            .back()
            .and_then(|&latest| next(&snapshot.counts, sub(latest, origin)));
        snapshot.centre = Some(located);
        snapshot
    }
}

/// One sweep as the panel draws it.
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// Readings per cell: rows from the top, columns from directly behind, so
    /// cell `row * COLUMNS + column`.
    pub counts: [u32; CELLS],
    /// Readings the sweep holds, binned yet or not.
    pub readings: usize,
    /// Where directions are measured from, once there are enough readings to
    /// say.
    pub centre: Option<Centre>,
    /// How far the field's magnitude differs between directions, as
    /// (strongest − weakest) / median over each covered cell's own median
    /// magnitude.
    ///
    /// One value per direction, so lingering in one cannot outvote the rest,
    /// and each a median, so one glitch cannot decide it. Magnitudes are taken
    /// as delivered, before any centring: turning cannot change how strong the
    /// Earth's field is, so whatever spread remains is the sensor reading
    /// itself wrong. Two sweeps' spreads compare only when both are
    /// [`complete`](Snapshot::complete).
    pub spread: Option<f64>,
    /// The nearest direction not read yet, and the motion toward it.
    pub next: Option<Next>,
}

impl Snapshot {
    pub fn covered(&self) -> usize {
        self.counts.iter().filter(|&&held| held > 0).count()
    }

    /// Whether every direction has been read.
    pub fn complete(&self) -> bool {
        self.covered() == CELLS
    }
}

/// Where a sweep's directions are measured from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Centre {
    /// The centre of a sphere fitted through readings that spread in all
    /// three dimensions. Its distance from the origin is the hard-iron offset
    /// the readings still carry.
    Fitted(Field),
    /// The readings' mean, while they lie too close to a plane for a sphere to
    /// be located — a turn about one axis traces a ring. It keeps the ring's
    /// directions in the ring's plane, which is exactly what leaves empty the
    /// rows that axis cannot reach. It estimates no offset.
    Mean(Field),
}

impl Centre {
    pub const fn point(self) -> Field {
        match self {
            Self::Fitted(point) | Self::Mean(point) => point,
        }
    }
}

/// The nearest direction the field has not been read from, and the headset
/// motion that brings the reading toward it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Next {
    pub cell: usize,
    pub motion: Motion,
    /// Whether the motion runs the way [`Motion::instruction`] calls positive:
    /// up for a nod, left for a shake, toward the right for a tilt.
    pub positive: bool,
}

impl Next {
    /// The motion in words for a headset held in the hands, which is how a
    /// sweep is done: a neck reaches too few directions.
    pub const fn instruction(self) -> &'static str {
        match (self.motion, self.positive) {
            (Motion::Nod, true) => "Tip its front up",
            (Motion::Nod, false) => "Tip its front down",
            (Motion::Shake, true) => "Turn it to its left",
            (Motion::Shake, false) => "Turn it to its right",
            (Motion::Tilt, true) => "Roll it toward its right",
            (Motion::Tilt, false) => "Roll it toward its left",
        }
    }
}

/// A reading in sensor axes, in headset axes instead.
///
/// `axes` is the array `Device::mounting` stores and the mounting check
/// writes: entry `i` is the signed sensor axis, 1 to 3 for X to Z, that points
/// along head direction `i` — right, forward, up. So each headset component is
/// one sensor component with a sign. Anything but a [`proper`] rotation is
/// refused: a mirrored mounting would turn every instruction the wrong way
/// while the grid went on looking plausible.
pub fn headset_axes(axes: [i8; 3], sensor: Field) -> Option<Field> {
    proper(axes).then(|| axes.map(|axis| dot(mounting::unit(axis).map(f64::from), sensor)))
}

/// Whether a mounting is a rotation: three distinct axes out of X, Y and Z,
/// right-handed. Mirrored, repeated, missing and out-of-range ones are not.
pub fn proper(axes: [i8; 3]) -> bool {
    let [right, forward, up] = axes;
    axes.iter()
        .all(|axis| (1..=3).contains(&axis.unsigned_abs()))
        && mounting::cross(mounting::unit(right), mounting::unit(forward)) == mounting::unit(up)
}

/// The cell a direction falls in. It need not be a unit vector; a zero or
/// non-finite one has no cell. A direction on a boundary belongs to the cell
/// below it or to its right.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "Both indices are floored and non-negative; the cast saturates and the grid clamps"
)]
fn cell_of(direction: Field) -> Option<usize> {
    let length = norm(direction);
    if !length.is_finite() || length <= 0.0 {
        return None;
    }
    let [right, forward, up] = direction.map(|value| value / length);
    let row = (((1.0 - up) / ROW_HEIGHT).floor().max(0.0) as usize).min(ROWS - 1);
    let column = (((right.atan2(forward) + PI) / COLUMN_WIDTH)
        .floor()
        .max(0.0) as usize)
        .min(COLUMNS - 1);
    Some(row * COLUMNS + column)
}

/// The middle of a cell as a unit direction: halfway across its azimuth and
/// halfway through its sin(elevation), so its two halves hold equal solid
/// angle.
fn centre_of(cell: usize) -> Field {
    let up = 1.0 - (index(cell / COLUMNS) + 0.5) * ROW_HEIGHT;
    let azimuth = (index(cell % COLUMNS) + 0.5) * COLUMN_WIDTH - PI;
    let across = (1.0 - up * up).sqrt();
    [across * azimuth.sin(), across * azimuth.cos(), up]
}

#[allow(clippy::cast_precision_loss, reason = "Grid indices stay below 48")]
fn index(value: usize) -> f64 {
    value as f64
}

/// The readings' mean, their covariance about it, and Σ|p|²p / 2n over the
/// readings p taken from the mean — the right-hand side of the sphere fit.
fn moments(readings: &VecDeque<Field>) -> (Field, [[f64; 3]; 3], Field) {
    let mut count = 0.0;
    let mut sum = [0.0; 3];
    for &reading in readings {
        count += 1.0;
        sum = add(sum, reading);
    }
    let mean = scale(sum, 1.0 / count);
    let mut covariance = [[0.0; 3]; 3];
    let mut moment = [0.0; 3];
    for &reading in readings {
        let local = sub(reading, mean);
        for (row, along) in covariance.iter_mut().zip(local) {
            for (entry, across) in row.iter_mut().zip(local) {
                *entry += along * across / count;
            }
        }
        moment = add(moment, scale(local, dot(local, local) / (2.0 * count)));
    }
    (mean, covariance, moment)
}

/// How far a covariance spreads out of a plane, with its scale divided out:
/// det C / (tr C / 3)³. 1 for an even sphere, about 0.076 for readings over a
/// cap reaching 30° from its middle, 0 for a ring.
fn span(covariance: [[f64; 3]; 3]) -> f64 {
    let [first, second, third] = covariance;
    let size = (first[0] + second[1] + third[2]) / 3.0;
    if size <= 0.0 {
        return 0.0;
    }
    dot(first, cross(second, third)) / size.powi(3)
}

/// Where the readings' directions are measured from.
///
/// An algebraic sphere fit, solved about the readings' mean: there its normal
/// equations come down to C·c = Σ|p|²p / 2n, with C the readings' own
/// covariance. So one matrix is both what gets inverted and what says whether
/// it can be — a ring makes it singular, which is what [`span`] measures. The
/// inverse's columns are its rows' pairwise cross products over the
/// determinant.
fn centre(readings: &VecDeque<Field>) -> Centre {
    let (mean, covariance, moment) = moments(readings);
    if span(covariance) < MIN_SPAN {
        return Centre::Mean(mean);
    }
    let [first, second, third] = covariance;
    let determinant = dot(first, cross(second, third));
    let offset = add(
        add(
            scale(cross(second, third), moment[0]),
            scale(cross(third, first), moment[1]),
        ),
        scale(cross(first, second), moment[2]),
    );
    Centre::Fitted(add(mean, scale(offset, 1.0 / determinant)))
}

fn spread(mut magnitudes: [Vec<f64>; CELLS]) -> Option<f64> {
    let mut medians: Vec<f64> = magnitudes
        .iter_mut()
        .filter_map(|cell| median(cell))
        .collect();
    if medians.len() < 2 {
        return None;
    }
    // `median` sorts in place, so afterwards the ends are the extremes.
    let middle = median(&mut medians)?;
    let (weakest, strongest) = (*medians.first()?, *medians.last()?);
    (middle > 0.0).then_some((strongest - weakest) / middle)
}

/// The median, sorting `values` in place on the way.
fn median(values: &mut [f64]) -> Option<f64> {
    values.sort_by(f64::total_cmp);
    let half = values.len() / 2;
    let upper = *values.get(half)?;
    Some(if values.len().is_multiple_of(2) {
        f64::midpoint(values[half - 1], upper)
    } else {
        upper
    })
}

/// The empty cell nearest the latest reading, and the motion toward it.
fn next(counts: &[u32; CELLS], latest: Field) -> Option<Next> {
    let length = norm(latest);
    if length <= 0.0 {
        return None;
    }
    let current = scale(latest, 1.0 / length);
    let (cell, target) = counts
        .iter()
        .enumerate()
        .filter(|&(_, &held)| held == 0)
        .map(|(cell, _)| (cell, centre_of(cell)))
        .max_by(|(_, one), (_, other)| dot(*one, current).total_cmp(&dot(*other, current)))?;
    let (motion, positive) = towards(current, target)?;
    Some(Next {
        cell,
        motion,
        positive,
    })
}

/// The headset motion that carries a reading from `current` toward `target`.
///
/// The field holds still while the headset turns, so in headset axes it turns
/// the other way: turning the headset by a rotation moves the reading by its
/// inverse. The rotation wanted therefore carries the target onto the current
/// reading, about `target × current`. The reverse, `current × target`, points
/// every instruction backwards while each one still looks nearly right. The
/// axis's largest component names the motion and its sign the direction, in
/// [`Motion`]'s own terms. At the exact antipode every axis serves.
fn towards(current: Field, target: Field) -> Option<(Motion, bool)> {
    let axis = cross(target, current);
    let largest = (0..3).max_by(|&one, &other| axis[one].abs().total_cmp(&axis[other].abs()))?;
    let motion = Motion::ALL
        .into_iter()
        .find(|motion| motion.axis() == largest)?;
    Some((motion, axis[largest] > 0.0))
}

fn add(one: Field, other: Field) -> Field {
    [one[0] + other[0], one[1] + other[1], one[2] + other[2]]
}

fn sub(one: Field, other: Field) -> Field {
    [one[0] - other[0], one[1] - other[1], one[2] - other[2]]
}

fn scale(vector: Field, factor: f64) -> Field {
    vector.map(|value| value * factor)
}

fn dot(one: Field, other: Field) -> f64 {
    one[0] * other[0] + one[1] * other[1] + one[2] * other[2]
}

fn cross(one: Field, other: Field) -> Field {
    [
        one[1] * other[2] - one[2] * other[1],
        one[2] * other[0] - one[0] * other[2],
        one[0] * other[1] - one[1] * other[0],
    ]
}

fn norm(vector: Field) -> f64 {
    dot(vector, vector).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hard-iron offset of the size a headphone driver's magnet can put on a
    /// sensor clipped beside it.
    const OFFSET: Field = [11.0, -8.0, 6.0];
    /// The Earth's field in round numbers, in the same unit.
    const EARTH: f64 = 50.0;

    /// `count` directions spread evenly over the sphere: uniform in height,
    /// golden-angle steps around. Deterministic, so a failure repeats.
    fn even_sphere(count: u32) -> Vec<Field> {
        let golden = PI * (3.0 - 5.0_f64.sqrt());
        (0..count)
            .map(|step| {
                let step = f64::from(step);
                let up = 1.0 - (2.0 * step + 1.0) / f64::from(count);
                let across = (1.0 - up * up).sqrt();
                let azimuth = golden * step;
                [across * azimuth.sin(), across * azimuth.cos(), up]
            })
            .collect()
    }

    /// A sweep that reads the Earth's field from each direction through
    /// `offset`.
    fn sweep(directions: impl IntoIterator<Item = Field>, offset: Field) -> Coverage {
        let mut coverage = Coverage::default();
        for direction in directions {
            assert!(coverage.push(add(scale(direction, EARTH), offset)));
        }
        coverage
    }

    fn fitted(snapshot: &Snapshot) -> Field {
        match snapshot.centre {
            Some(Centre::Fitted(centre)) => centre,
            other => panic!("expected a fitted centre, got {other:?}"),
        }
    }

    /// The claim the whole picture rests on: every cell is the same share of
    /// the sphere, so an even sphere fills them evenly.
    #[test]
    fn an_even_sphere_fills_every_cell_equally() {
        let mut counts = [0_u32; CELLS];
        for direction in even_sphere(48_000) {
            counts[cell_of(direction).unwrap()] += 1;
        }
        for (cell, &count) in counts.iter().enumerate() {
            assert!((990..=1010).contains(&count), "cell {cell}: {count}");
        }
    }

    /// A target drawn in one cell and binned into another would send the
    /// listener chasing a cell they are already in.
    #[test]
    fn every_cell_centre_lies_in_its_own_cell() {
        for cell in 0..CELLS {
            assert_eq!(cell_of(centre_of(cell)), Some(cell));
        }
    }

    #[test]
    fn the_grid_reads_the_way_it_is_drawn() {
        let at = |azimuth: f64, elevation: f64| {
            let (azimuth, elevation) = (azimuth.to_radians(), elevation.to_radians());
            cell_of([
                elevation.cos() * azimuth.sin(),
                elevation.cos() * azimuth.cos(),
                elevation.sin(),
            ])
            .unwrap()
        };
        // Forward and a little up: the second row, just right of the middle.
        assert_eq!(at(15.0, 15.0), COLUMNS + 6);
        // Right is right of the middle on screen, left is left of it.
        assert_eq!(at(105.0, 15.0) % COLUMNS, 9);
        assert_eq!(at(-105.0, 15.0) % COLUMNS, 2);
        // Straight up and straight down are the top and bottom rows.
        assert_eq!(at(0.0, 89.0) / COLUMNS, 0);
        assert_eq!(at(0.0, -89.0) / COLUMNS, ROWS - 1);
        // Directly behind is the same place at both edges.
        assert_eq!(at(179.0, 15.0) % COLUMNS, COLUMNS - 1);
        assert_eq!(at(-179.0, 15.0) % COLUMNS, 0);
        assert_eq!(cell_of([0.0; 3]), None);
        assert_eq!(cell_of([f64::NAN, 0.0, 1.0]), None);
    }

    /// Pins the numbers [`span`]'s documentation and [`MIN_SPAN`]'s rest on.
    #[test]
    fn span_tells_a_sphere_from_a_cap_from_a_ring() {
        let span_of = |directions: Vec<Field>| span(moments(&directions.into()).1);
        assert!(span_of(even_sphere(4000)) > 0.999);
        let cap = even_sphere(40_000)
            .into_iter()
            .filter(|direction| direction[2] >= 30_f64.to_radians().cos())
            .collect();
        let cap = span_of(cap);
        assert!((0.07..0.083).contains(&cap), "{cap}");
        let ring = (0..720)
            .map(|step| {
                let heading = f64::from(step).to_radians() / 2.0;
                [0.57 * heading.sin(), 0.57 * heading.cos(), -0.82]
            })
            .collect();
        assert!(span_of(ring) < 1e-9);
    }

    /// Why the centre is fitted rather than averaged: a sweep that has only
    /// been turned, not yet turned over, pulls the mean far toward the side it
    /// covered, and every direction read from there would be skewed.
    #[test]
    fn the_fitted_centre_survives_uneven_coverage_where_the_mean_does_not() {
        let upper: Vec<Field> = even_sphere(4000)
            .into_iter()
            .filter(|direction| direction[2] > 0.35)
            .collect();
        let snapshot = sweep(upper.iter().copied(), OFFSET).snapshot();
        let centre = fitted(&snapshot);
        assert!(norm(sub(centre, OFFSET)) < 1e-6, "{centre:?}");
        let (mean, ..) = moments(
            &upper
                .iter()
                .map(|&direction| add(scale(direction, EARTH), OFFSET))
                .collect(),
        );
        assert!(norm(sub(mean, OFFSET)) > 20.0, "{mean:?}");
    }

    #[test]
    fn turning_about_one_axis_leaves_the_top_and_bottom_rows_empty() {
        // The field dips 55° below the horizon, and turning the headset about
        // its own up axis carries it round at that one height.
        let dip = 55_f64.to_radians();
        let ring = (0..720).map(|step| {
            let heading = f64::from(step).to_radians() / 2.0;
            [
                dip.cos() * heading.sin(),
                dip.cos() * heading.cos(),
                -dip.sin(),
            ]
        });
        let snapshot = sweep(ring, OFFSET).snapshot();
        assert!(
            matches!(snapshot.centre, Some(Centre::Mean(_))),
            "{:?}",
            snapshot.centre
        );
        let row = |index: usize| &snapshot.counts[index * COLUMNS..(index + 1) * COLUMNS];
        assert!(
            row(0).iter().all(|&held| held == 0),
            "{:?}",
            snapshot.counts
        );
        assert!(row(ROWS - 1).iter().all(|&held| held == 0));
        assert!(snapshot.covered() <= 2 * COLUMNS);
        assert!(!snapshot.complete());
    }

    /// A right-handed turn about one headset axis — 0 right, 1 forward, 2 up.
    fn about(axis: usize, degrees: f64) -> [[f64; 3]; 3] {
        let (sin, cos) = degrees.to_radians().sin_cos();
        let (from, to) = ((axis + 1) % 3, (axis + 2) % 3);
        let mut matrix = [[0.0; 3]; 3];
        matrix[axis][axis] = 1.0;
        matrix[from][from] = cos;
        matrix[from][to] = -sin;
        matrix[to][from] = sin;
        matrix[to][to] = cos;
        matrix
    }

    fn multiply(outer: [[f64; 3]; 3], inner: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
        std::array::from_fn(|row| {
            std::array::from_fn(|column| (0..3).map(|k| outer[row][k] * inner[k][column]).sum())
        })
    }

    /// A world direction as the headset at `pose` reads it.
    fn to_headset(pose: [[f64; 3]; 3], world: Field) -> Field {
        std::array::from_fn(|axis| (0..3).map(|k| pose[k][axis] * world[k]).sum())
    }

    /// The load-bearing test for [`towards`]. It does not reuse that
    /// function's argument about inverses: it turns a simulated headset in a
    /// fixed field by the motion named, about the headset's own axis, and
    /// checks the reading really moved toward the target. Swap the cross
    /// product and every case here fails.
    #[test]
    fn the_named_motion_moves_the_reading_toward_the_target() {
        let dip = 55_f64.to_radians();
        let field = [0.0, dip.cos(), -dip.sin()];
        for [heading, pitch, roll] in [
            [0.0, 0.0, 0.0],
            [70.0, -20.0, 35.0],
            [-150.0, 60.0, -80.0],
            [10.0, 170.0, 5.0],
        ] {
            let pose = multiply(multiply(about(2, heading), about(0, pitch)), about(1, roll));
            let current = to_headset(pose, field);
            for target in (0..CELLS).map(centre_of) {
                let before = dot(target, current);
                if before.abs() > 0.999 {
                    continue;
                }
                let (motion, positive) = towards(current, target).unwrap();
                let step = if positive { 0.1 } else { -0.1 };
                let turned = to_headset(multiply(pose, about(motion.axis(), step)), field);
                assert!(
                    dot(target, turned) > before,
                    "pose {heading} {pitch} {roll}, target {target:?}: {motion:?} {positive}"
                );
            }
        }
    }

    /// The in-hand words keep the mounting check's sense of positive, so one
    /// convention runs through both.
    #[test]
    fn positive_means_what_the_mounting_check_means() {
        for (motion, positive_word, negative_word) in [
            (Motion::Nod, "up", "down"),
            (Motion::Shake, "left", "right"),
            (Motion::Tilt, "right", "left"),
        ] {
            assert!(motion.instruction().contains(positive_word), "{motion:?}");
            let forward = Next {
                cell: 0,
                motion,
                positive: true,
            };
            let back = Next {
                positive: false,
                ..forward
            };
            assert!(forward.instruction().ends_with(positive_word));
            assert!(back.instruction().ends_with(negative_word));
        }
    }

    #[test]
    fn the_target_is_the_one_direction_not_yet_read() {
        for missing in [0, 17, CELLS - 1] {
            let directions: Vec<Field> = (0..CELLS)
                .filter(|&cell| cell != missing)
                .map(centre_of)
                .collect();
            let passes = directions
                .iter()
                .copied()
                .cycle()
                .take(directions.len() * 3);
            let snapshot = sweep(passes, OFFSET).snapshot();
            assert!(norm(sub(fitted(&snapshot), OFFSET)) < 1e-6);
            assert_eq!(snapshot.covered(), CELLS - 1);
            assert!(!snapshot.complete());
            assert_eq!(snapshot.next.map(|next| next.cell), Some(missing));
        }
    }

    #[test]
    fn a_complete_sweep_has_nowhere_left_to_go() {
        let snapshot = sweep(even_sphere(2000), OFFSET).snapshot();
        assert!(snapshot.complete());
        assert_eq!(snapshot.next, None);
        assert!(norm(sub(fitted(&snapshot), OFFSET)) < 1e-6);
    }

    #[test]
    fn spread_reads_the_field_as_delivered_one_vote_per_direction() {
        // A sensor that reads the field right: every direction equally strong.
        let exact = sweep(even_sphere(2000), [0.0; 3]).snapshot();
        assert!(exact.spread.unwrap() < 1e-9);
        // The same sweep through an uncorrected offset. Centring still finds
        // every direction, but the magnitudes do not agree, and that
        // disagreement is the reading.
        let offset = sweep(even_sphere(2000), OFFSET);
        let spread = offset.snapshot().spread.unwrap();
        assert!((0.5..0.6).contains(&spread), "{spread}");
        // Dwelling on one direction a thousand readings longer changes nothing.
        let mut lingering = offset;
        for _ in 0..1000 {
            assert!(lingering.push(add(scale(centre_of(5), EARTH), OFFSET)));
        }
        let lingered = lingering.snapshot().spread.unwrap();
        assert!(
            (lingered - spread).abs() < 1e-3,
            "{lingered} against {spread}"
        );
    }

    #[test]
    fn too_few_readings_bin_nothing() {
        let snapshot =
            sweep(even_sphere(2000).into_iter().take(MIN_READINGS - 1), OFFSET).snapshot();
        assert_eq!(snapshot.readings, MIN_READINGS - 1);
        assert_eq!(snapshot.covered(), 0);
        assert!(snapshot.centre.is_none());
        assert!(snapshot.spread.is_none());
        assert!(snapshot.next.is_none());
    }

    #[test]
    fn a_sweep_keeps_its_latest_finite_readings() {
        let mut coverage = Coverage::default();
        assert!(!coverage.push([f64::NAN, 0.0, 0.0]));
        assert!(!coverage.push([0.0, f64::INFINITY, 0.0]));
        let mut value = 0.0;
        for _ in 0..MAX_READINGS + 10 {
            value += 1.0;
            assert!(coverage.push([value, 1.0, 1.0]));
        }
        assert_eq!(coverage.snapshot().readings, MAX_READINGS);
        let oldest = coverage.readings.front().unwrap();
        assert!((oldest[0] - 11.0).abs() < 0.5, "{oldest:?}");
        coverage.clear();
        assert_eq!(coverage.snapshot().readings, 0);
    }

    #[test]
    fn a_sensor_reading_is_read_through_the_recorded_mounting() {
        let reading = [1.0, 2.0, 3.0];
        let identity = headset_axes([1, 2, 3], reading).unwrap();
        assert!(norm(sub(identity, reading)) < 1e-12);
        // The mounting the manual records for this project's own sensor:
        // right is sensor −Y, forward is sensor +X, up is sensor +Z.
        let recorded = headset_axes([-2, 1, 3], reading).unwrap();
        assert!(norm(sub(recorded, [-2.0, 1.0, 3.0])) < 1e-12);
        // Mirrored, repeated, missing and out-of-range axes are no rotation.
        for refused in [[2, 1, 3], [1, 1, 3], [0, 2, 3], [1, 2, 4], [0, 0, 0]] {
            assert!(!proper(refused), "{refused:?}");
            assert!(headset_axes(refused, reading).is_none(), "{refused:?}");
        }
        assert!(proper([1, 2, 3]) && proper([-2, 1, 3]));
    }
}
