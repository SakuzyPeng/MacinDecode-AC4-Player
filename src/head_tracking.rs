//! Listener pose in canonical X-right/Y-front/Z-up coordinates. The control clock
//! is independent of egui repainting, including when the window is hidden.
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum HeadSource {
    #[default]
    Automatic,
    Manual,
    AirPods,
    Off,
}
impl HeadSource {
    pub const ALL: [Self; 4] = [Self::Automatic, Self::Manual, Self::AirPods, Self::Off];
    pub const fn label(self) -> &'static str {
        match self {
            Self::Automatic => "Auto · AirPods / manual",
            Self::Manual => "Manual",
            Self::AirPods => "AirPods",
            Self::Off => "Fixed orientation",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quaternion(pub [f64; 4]);
impl Default for Quaternion {
    fn default() -> Self {
        Self([1.0, 0.0, 0.0, 0.0])
    }
}
impl Quaternion {
    pub fn normalized(self) -> Self {
        let length = self.0.iter().map(|x| x * x).sum::<f64>().sqrt();
        if !length.is_finite() || length < 1.0e-12 {
            return Self::default();
        }
        Self(self.0.map(|x| x / length))
    }
    pub fn conjugate(self) -> Self {
        let [w, x, y, z] = self.0;
        Self([w, -x, -y, -z])
    }
    pub fn multiply(self, rhs: Self) -> Self {
        let [aw, ax, ay, az] = self.0;
        let [bw, bx, by, bz] = rhs.0;
        Self([
            aw * bw - ax * bx - ay * by - az * bz,
            aw * bx + ax * bw + ay * bz - az * by,
            aw * by - ax * bz + ay * bw + az * bx,
            aw * bz + ax * by - ay * bx + az * bw,
        ])
    }
    pub fn from_euler([yaw, pitch, roll]: [f32; 3]) -> Self {
        let (sy, cy) = (f64::from(yaw).to_radians() * 0.5).sin_cos();
        let (sp, cp) = (f64::from(pitch.clamp(-85.0, 85.0)).to_radians() * 0.5).sin_cos();
        let (sr, cr) = (f64::from(roll).to_radians() * 0.5).sin_cos();
        Self([cy, 0.0, 0.0, sy])
            .multiply(Self([cp, sp, 0.0, 0.0]))
            .multiply(Self([cr, 0.0, sr, 0.0]))
            .normalized()
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "unit quaternion angles are bounded degrees"
    )]
    pub fn euler(self) -> [f32; 3] {
        let [w, x, y, z] = self.normalized().0;
        [
            (-(2.0 * (x * y - w * z)))
                .atan2(1.0 - 2.0 * (x * x + z * z))
                .to_degrees() as f32,
            (2.0 * (y * z + w * x)).clamp(-1.0, 1.0).asin().to_degrees() as f32,
            (-(2.0 * (x * z - w * y)))
                .atan2(1.0 - 2.0 * (x * x + y * y))
                .to_degrees() as f32,
        ]
    }
    fn slerp(self, mut rhs: Self, amount: f64) -> Self {
        let mut dot = self.0.iter().zip(rhs.0).map(|(a, b)| a * b).sum::<f64>();
        if dot < 0.0 {
            rhs.0 = rhs.0.map(|v| -v);
            dot = -dot;
        }
        if dot > 0.9995 {
            return Self(std::array::from_fn(|i| {
                self.0[i] + amount * (rhs.0[i] - self.0[i])
            }))
            .normalized();
        }
        let angle = dot.clamp(-1.0, 1.0).acos();
        let a = ((1.0 - amount) * angle).sin() / angle.sin();
        let b = (amount * angle).sin() / angle.sin();
        Self(std::array::from_fn(|i| a * self.0[i] + b * rhs.0[i])).normalized()
    }
    #[cfg_attr(not(windows_spatial_output), allow(dead_code))]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "rotating bounded normalized listener coordinates"
    )]
    pub fn rotate_listener(self, [x, y, z]: [f32; 3]) -> [f32; 3] {
        let canonical = Self([0.0, f64::from(x), -f64::from(z), f64::from(y)]);
        let rotated = self.conjugate().multiply(canonical).multiply(self);
        [
            rotated.0[1] as f32,
            rotated.0[3] as f32,
            -rotated.0[2] as f32,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(macinrender_output),
    allow(
        dead_code,
        reason = "sensor statuses are dormant without the native motion module"
    )
)]
pub enum HeadStatus {
    Fixed,
    System,
    Manual,
    AirPods,
    Waiting,
    Denied,
    Disconnected,
    MissingBundle,
}
impl HeadStatus {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Fixed => "Fixed orientation",
            Self::System => "Orientation controlled by the system",
            Self::Manual => "Manual orientation",
            Self::AirPods => "AirPods tracking",
            Self::Waiting => "Waiting for AirPods · manual active",
            Self::Denied => "Motion permission denied · manual active",
            Self::Disconnected => "AirPods unavailable · manual active",
            Self::MissingBundle => "AirPods motion requires the packaged app · manual active",
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub struct HeadSnapshot {
    pub pose: Quaternion,
    pub status: HeadStatus,
}
impl Default for HeadSnapshot {
    fn default() -> Self {
        Self {
            pose: Quaternion::default(),
            status: HeadStatus::Fixed,
        }
    }
}
#[derive(Default)]
pub struct PoseMirror(Mutex<HeadSnapshot>);
impl PoseMirror {
    pub fn snapshot(&self) -> HeadSnapshot {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    #[cfg(windows_spatial_output)]
    pub fn try_pose(&self) -> Option<Quaternion> {
        self.0.try_lock().ok().map(|pose| pose.pose)
    }
}

#[derive(Clone)]
struct Desired {
    source: HeadSource,
    enabled: bool,
    system: bool,
    manual: Quaternion,
    revision: u64,
    recenter: u64,
}
impl Default for Desired {
    fn default() -> Self {
        Self {
            source: HeadSource::Automatic,
            enabled: false,
            system: false,
            manual: Quaternion::default(),
            revision: 0,
            recenter: 0,
        }
    }
}

#[cfg(macinrender_output)]
pub type NativeTarget = Arc<Mutex<Option<macindecode_macinrender::Control>>>;
pub struct HeadTracker {
    desired: Arc<Mutex<Desired>>,
    mirror: Arc<PoseMirror>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    #[cfg(macinrender_output)]
    target: Arc<Mutex<Option<NativeTarget>>>,
}
impl HeadTracker {
    #[allow(
        clippy::too_many_lines,
        reason = "one control worker owns sensor lifecycle and pose continuity"
    )]
    pub fn new() -> Self {
        let desired = Arc::new(Mutex::new(Desired::default()));
        let mirror = Arc::new(PoseMirror::default());
        let stop = Arc::new(AtomicBool::new(false));
        #[cfg(macinrender_output)]
        let target = Arc::new(Mutex::new(None::<NativeTarget>));
        let d = Arc::clone(&desired);
        let m = Arc::clone(&mirror);
        let s = Arc::clone(&stop);
        #[cfg(macinrender_output)]
        let t = Arc::clone(&target);
        let join = thread::Builder::new()
            .name("listener-orientation".into())
            .spawn(move || {
                let mut resolved = Quaternion::default();
                let mut fallback = resolved;
                let mut revision = 0;
                let mut recenter = 0;
                let mut last_tick = Instant::now();
                #[cfg(macinrender_output)]
                let mut sensor = None::<macindecode_macinrender::motion::Motion>;
                #[cfg(macinrender_output)]
                let mut reference = Quaternion::default();
                #[cfg(macinrender_output)]
                let mut was_sensor = false;
                #[cfg(macinrender_output)]
                let mut last_sent = None::<(usize, [f32; 3])>;
                while !s.load(Ordering::Relaxed) {
                    let desired = d
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    let elapsed = last_tick.elapsed().as_secs_f64().min(0.1);
                    last_tick = Instant::now();
                    if revision != desired.revision {
                        fallback = desired.manual;
                        revision = desired.revision;
                    }
                    let mut goal = fallback;
                    let mut status = HeadStatus::Manual;
                    if !desired.enabled || desired.source == HeadSource::Off {
                        goal = Quaternion::default();
                        status = if desired.system {
                            HeadStatus::System
                        } else {
                            HeadStatus::Fixed
                        };
                    }
                    #[cfg(macinrender_output)]
                    {
                        let wants_sensor = desired.enabled
                            && matches!(
                                desired.source,
                                HeadSource::Automatic | HeadSource::AirPods
                            )
                            && cfg!(target_os = "macos");
                        if wants_sensor {
                            if sensor.is_none() {
                                sensor = macindecode_macinrender::motion::Motion::new().ok();
                            }
                            let sample = sensor.as_mut().and_then(|device| device.sample().ok());
                            if let Some(sample) = sample.filter(|sample| sample.state == 2) {
                                // AirPods reports head yaw around Z and pitch around X. Our
                                // canonical ZXY pose uses those same axes, unlike .NET's YXZ.
                                let raw = Quaternion(sample.quaternion).normalized();
                                if !was_sensor {
                                    reference = raw.multiply(resolved.conjugate());
                                }
                                if desired.recenter != recenter {
                                    reference = raw;
                                }
                                goal = reference.conjugate().multiply(raw).normalized();
                                status = HeadStatus::AirPods;
                                was_sensor = true;
                                fallback = resolved;
                            } else {
                                if was_sensor {
                                    fallback = resolved;
                                    goal = fallback;
                                }
                                was_sensor = false;
                                status = match sample.map(|value| value.state) {
                                    Some(1) => HeadStatus::Waiting,
                                    Some(3) => HeadStatus::Denied,
                                    Some(5) => HeadStatus::MissingBundle,
                                    _ => HeadStatus::Disconnected,
                                };
                            }
                        } else {
                            sensor = None;
                            was_sensor = false;
                        }
                    }
                    if recenter != desired.recenter {
                        recenter = desired.recenter;
                        goal = Quaternion::default();
                        fallback = goal;
                    }
                    resolved = resolved.slerp(goal, 1.0 - (-elapsed / 0.024).exp());
                    if !desired.enabled {
                        resolved = goal;
                    }
                    *m.0.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = HeadSnapshot {
                        pose: resolved,
                        status,
                    };
                    #[cfg(macinrender_output)]
                    if let Some(slot) = t
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone()
                        && let Some(control) = slot
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone()
                    {
                        let pose = if desired.enabled {
                            resolved.euler()
                        } else {
                            [0.0; 3]
                        };
                        let target_id = Arc::as_ptr(&slot) as usize;
                        if last_sent.is_none_or(|(previous_id, previous)| {
                            previous_id != target_id
                                || previous
                                    .into_iter()
                                    .zip(pose)
                                    .any(|(a, b)| (a - b).abs() >= 0.05)
                        }) && control.orientation(pose).is_ok()
                        {
                            last_sent = Some((target_id, pose));
                        }
                    }
                    thread::sleep(Duration::from_millis(16));
                }
            })
            .ok();
        Self {
            desired,
            mirror,
            stop,
            join,
            #[cfg(macinrender_output)]
            target,
        }
    }
    pub fn mirror(&self) -> Arc<PoseMirror> {
        Arc::clone(&self.mirror)
    }
    pub fn snapshot(&self) -> HeadSnapshot {
        self.mirror.snapshot()
    }
    pub fn configure(&self, source: HeadSource, enabled: bool, system: bool) {
        let mut desired = self
            .desired
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        desired.source = source;
        desired.enabled = enabled;
        desired.system = system;
    }
    pub fn manual(&self, euler: [f32; 3]) {
        let mut desired = self
            .desired
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        desired.manual = Quaternion::from_euler(euler);
        desired.revision += 1;
        desired.source = HeadSource::Manual;
    }
    pub fn recenter(&self) {
        self.desired
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recenter += 1;
    }
    #[cfg(macinrender_output)]
    pub fn set_target(&self, target: Option<NativeTarget>) {
        *self
            .target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = target;
    }
}
impl Drop for HeadTracker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_pose_round_trips_all_three_axes() {
        for angles in [
            [30.0, 0.0, 0.0],
            [0.0, 40.0, 0.0],
            [0.0, 0.0, -50.0],
            [51.0, 25.0, -30.0],
        ] {
            for (actual, expected) in Quaternion::from_euler(angles)
                .euler()
                .into_iter()
                .zip(angles)
            {
                assert!((actual - expected).abs() < 0.001);
            }
        }
    }
    #[test]
    fn turning_left_moves_a_world_front_source_to_the_right() {
        let point = Quaternion::from_euler([90.0, 0.0, 0.0]).rotate_listener([0.0, 0.0, -1.0]);
        assert!((point[0] - 1.0).abs() < 0.0001);
        assert!(point[2].abs() < 0.0001);
    }
    #[test]
    fn invalid_pose_and_antipodal_slerp_stay_finite() {
        assert_eq!(
            Quaternion([f64::NAN; 4]).normalized(),
            Quaternion::default()
        );
        assert_eq!(
            Quaternion::default().slerp(Quaternion([-1.0, 0.0, 0.0, 0.0]), 0.5),
            Quaternion::default()
        );
    }
}
