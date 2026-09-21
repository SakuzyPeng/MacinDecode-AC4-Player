//! Listener pose in canonical X-right/Y-front/Z-up coordinates. The control clock
//! is independent of egui repainting, including when the window is hidden.
use serde::{Deserialize, Serialize};
#[cfg(macinrender_output)]
use std::sync::atomic::AtomicU64;
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
    PoseBridge,
    Off,
}
impl HeadSource {
    pub const ALL: [Self; 5] = [
        Self::Automatic,
        Self::Manual,
        Self::AirPods,
        Self::PoseBridge,
        Self::Off,
    ];
    pub const fn label(self) -> &'static str {
        match self {
            Self::Automatic => "Auto · AirPods / manual",
            Self::Manual => "Manual",
            Self::AirPods => "AirPods",
            Self::PoseBridge => "PoseBridge sensor",
            Self::Off => "Fixed orientation",
        }
    }
    /// Whether this build can drive the pose from this source at all.
    ///
    /// The Head page and the scene header offer the same five, and a build that
    /// cannot reach one has to say so in both places. Keeping the conjunction
    /// here is what stops the two lists from drifting apart.
    pub const fn available(self) -> bool {
        match self {
            Self::AirPods => cfg!(target_os = "macos"),
            Self::PoseBridge => cfg!(posebridge_input),
            Self::Automatic | Self::Manual | Self::Off => true,
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
        let sin_pitch = (2.0 * (y * z + w * x)).clamp(-1.0, 1.0);
        let (yaw, roll) = if sin_pitch.abs() >= 1.0 - 1e-12 {
            // ZXY poles couple yaw and roll; choose roll=0 and preserve rotation.
            (
                (2.0 * (x * y + w * z)).atan2(1.0 - 2.0 * (y * y + z * z)),
                0.0,
            )
        } else {
            (
                (-(2.0 * (x * y - w * z))).atan2(1.0 - 2.0 * (x * x + z * z)),
                (-(2.0 * (x * z - w * y))).atan2(1.0 - 2.0 * (x * x + y * y)),
            )
        };
        [
            yaw.to_degrees() as f32,
            sin_pitch.asin().to_degrees() as f32,
            roll.to_degrees() as f32,
        ]
    }
    pub(crate) fn slerp(self, mut rhs: Self, amount: f64) -> Self {
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
    Held,
    System,
    Manual,
    AirPods,
    Waiting,
    Denied,
    Disconnected,
    MissingBundle,
    #[cfg(posebridge_input)]
    BridgeActive,
    #[cfg(posebridge_input)]
    BridgeWaiting,
    #[cfg(posebridge_input)]
    BridgeFrozen,
}
impl HeadStatus {
    pub const fn label(self) -> &'static str {
        match self {
            #[cfg(posebridge_input)]
            Self::BridgeActive => "PoseBridge tracking",
            #[cfg(posebridge_input)]
            Self::BridgeWaiting => "Connect a PoseBridge sensor",
            #[cfg(posebridge_input)]
            Self::BridgeFrozen => "PoseBridge stale · orientation frozen",
            Self::Fixed => "Fixed orientation",
            Self::Held => "Orientation held · H to resume",
            Self::System => "Orientation controlled by the system",
            Self::Manual => "Manual orientation",
            Self::AirPods => "AirPods tracking",
            Self::Waiting => "Waiting for AirPods · manual active",
            Self::Denied => "Motion permission denied · manual active",
            Self::Disconnected => "AirPods unavailable · manual active",
            Self::MissingBundle => "AirPods motion requires the packaged app · manual active",
        }
    }
    pub const fn confidence(self) -> Confidence {
        match self {
            #[cfg(posebridge_input)]
            Self::BridgeActive => Confidence::Tracking,
            #[cfg(posebridge_input)]
            Self::BridgeWaiting | Self::BridgeFrozen => Confidence::Degraded,
            Self::AirPods => Confidence::Tracking,
            Self::Fixed | Self::Held | Self::System | Self::Manual => Confidence::Held,
            Self::Waiting | Self::Denied | Self::Disconnected | Self::MissingBundle => {
                Confidence::Degraded
            }
        }
    }
}

/// What a status says about the pose that came with it.
///
/// The label already says which source and what went wrong; this is the part a
/// drawing needs — one colour, and whether the pose moves without anyone here
/// touching anything, which is what decides the repaint cadence while nothing
/// is playing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// A sensor is arriving and the pose changes on its own.
    Tracking,
    /// The pose is exactly what was last asked for and will not move by itself.
    Held,
    /// A sensor was asked for and is not arriving. The pose is whatever it was
    /// left at, which is not the same as being wrong — only unattended.
    Degraded,
}
#[derive(Debug, Clone, Copy)]
pub struct HeadSnapshot {
    pub pose: Quaternion,
    pub status: HeadStatus,
    #[cfg_attr(
        not(posebridge_input),
        allow(
            dead_code,
            reason = "Optional sensor mounting checks consume the measured pose"
        )
    )]
    pub measurement: Option<Quaternion>,
}
impl Default for HeadSnapshot {
    fn default() -> Self {
        Self {
            pose: Quaternion::default(),
            status: HeadStatus::Fixed,
            measurement: None,
        }
    }
}
#[derive(Default)]
pub struct PoseMirror(Mutex<HeadSnapshot>);
impl PoseMirror {
    #[cfg(all(test, windows_spatial_output))]
    pub(crate) fn set_test_pose(&self, pose: Quaternion) {
        self.0.lock().unwrap().pose = pose;
    }

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
    /// Session-only presentation override. The selected source keeps sampling
    /// with its existing reference while the listener stays at this pose.
    held_pose: Option<Quaternion>,
    revision: u64,
    recenter: u64,
    #[cfg(posebridge_input)]
    bridge_smoothing_ms: f32,
    #[cfg(posebridge_input)]
    bridge_max_age_ms: u32,
    #[cfg(posebridge_input)]
    bridge_enhanced: bool,
}
impl Default for Desired {
    fn default() -> Self {
        Self {
            source: HeadSource::Automatic,
            enabled: false,
            system: false,
            manual: Quaternion::default(),
            held_pose: None,
            revision: 0,
            recenter: 0,
            #[cfg(posebridge_input)]
            bridge_smoothing_ms: 10.0,
            #[cfg(posebridge_input)]
            bridge_max_age_ms: 100,
            #[cfg(posebridge_input)]
            bridge_enhanced: false,
        }
    }
}

#[cfg(macinrender_output)]
pub type NativeTarget = Arc<Mutex<Option<macindecode_macinrender::Control>>>;
pub struct HeadTracker {
    #[cfg(posebridge_input)]
    pub bridge: crate::posebridge::service::Service,
    desired: Arc<Mutex<Desired>>,
    mirror: Arc<PoseMirror>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    #[cfg(macinrender_output)]
    target: Arc<Mutex<Option<NativeTarget>>>,
    #[cfg(macinrender_output)]
    target_revision: Arc<AtomicU64>,
}
impl HeadTracker {
    #[allow(
        clippy::too_many_lines,
        reason = "one control worker owns sensor lifecycle and pose continuity"
    )]
    pub fn new() -> Self {
        #[cfg(posebridge_input)]
        let bridge = crate::posebridge::service::Service::new();
        #[cfg(posebridge_input)]
        let bridge_samples = Arc::clone(&bridge.sample);
        #[cfg(posebridge_input)]
        let bridge_motion = Arc::clone(&bridge.motion);
        #[cfg(posebridge_input)]
        let bridge_diagnostics = Arc::clone(&bridge.enhancement);
        let desired = Arc::new(Mutex::new(Desired::default()));
        let mirror = Arc::new(PoseMirror::default());
        let stop = Arc::new(AtomicBool::new(false));
        #[cfg(macinrender_output)]
        let target = Arc::new(Mutex::new(None::<NativeTarget>));
        #[cfg(macinrender_output)]
        let target_revision = Arc::new(AtomicU64::new(0));
        let d = Arc::clone(&desired);
        let m = Arc::clone(&mirror);
        let s = Arc::clone(&stop);
        #[cfg(macinrender_output)]
        let t = Arc::clone(&target);
        #[cfg(macinrender_output)]
        let target_version = Arc::clone(&target_revision);
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
                let mut last_sent = None::<(u64, [f32; 3])>;
                #[cfg(posebridge_input)]
                let mut consumer = crate::posebridge::Consumer::default();
                #[cfg(posebridge_input)]
                let mut was_bridge = false;
                #[cfg(posebridge_input)]
                let mut enhancement=crate::posebridge::enhancement::Enhancement::default();
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
                    #[allow(unused_mut)]
                    let mut measurement=None;
                    #[allow(unused_mut)]
                    let mut bridge_frozen=false;
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
                    #[cfg(posebridge_input)]
                    let using_bridge = desired.enabled && desired.source == HeadSource::PoseBridge;
                    #[cfg(posebridge_input)]
                    #[cfg_attr(not(macinrender_output), allow(unused_variables, unused_assignments, reason = "Windows object output consumes the pose mirror, not C ABI keepalive"))]
                    let mut bridge_new_sample = false;
                    #[cfg(posebridge_input)]
                    if using_bridge {
                        let (sample, inbox) = {
                            let sample = bridge_samples.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            let mut motion = bridge_motion.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            (sample.clone(), std::mem::take(&mut *motion))
                        };
                        enhancement.ingest(inbox, resolved);
                        let now = Instant::now();
                        let max_age = Duration::from_millis(u64::from(desired.bridge_max_age_ms));
                        let valid = !desired.bridge_enhanced || !enhancement.expired(now, max_age);
                        #[cfg_attr(not(macinrender_output), allow(unused_assignments, reason = "No native keepalive in this build"))]
                        { bridge_new_sample = consumer.update(
                            sample.as_ref().filter(|_| valid), now, max_age, resolved,
                            recenter != desired.recenter,
                        ); }
                        if recenter != desired.recenter && consumer.active {
                            enhancement.recenter(now, max_age);
                        }
                        measurement = enhancement.measured();
                        (goal, bridge_frozen) = enhancement.apply(consumer.goal, resolved, consumer.active,
                            desired.bridge_enhanced, desired.held_pose.is_some(), now, max_age);
                        if bridge_frozen {consumer.active=false;}
                        *bridge_diagnostics.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=enhancement.diagnostics();
                        status = if consumer.active {
                            HeadStatus::BridgeActive
                        } else if sample.is_some() || consumer.has_reference() {
                            HeadStatus::BridgeFrozen
                        } else {
                            HeadStatus::BridgeWaiting
                        };
                        fallback = resolved;
                        was_bridge = true;
                    } else if was_bridge {
                        consumer.reset();
                        enhancement.reset();
                        *bridge_diagnostics.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=enhancement.diagnostics();
                        was_bridge = false;
                    }
                    if recenter != desired.recenter {
                        recenter = desired.recenter;
                        if desired.source != HeadSource::PoseBridge {
                            goal = Quaternion::default();
                            fallback = goal;
                        }
                    }
                    #[allow(unused_mut)]
                    let mut smoothing = 0.024;
                    #[cfg(posebridge_input)]
                    if using_bridge {
                        smoothing = f64::from(desired.bridge_smoothing_ms) / 1000.0;
                    }
                    resolved = if let Some(held) = desired.held_pose {
                        status = HeadStatus::Held;
                        held
                    } else if bridge_frozen || !desired.enabled || smoothing <= 0.0 {
                        goal
                    } else {
                        resolved.slerp(goal, 1.0 - (-elapsed / smoothing).exp())
                    };
                    *m.0.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = HeadSnapshot {
                        pose: resolved,
                        status,
                        measurement,
                    };
                    #[cfg(macinrender_output)]
                    let active_target = {
                        let current = t.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        current
                            .as_ref()
                            .map(|slot| (target_version.load(Ordering::Relaxed), Arc::clone(slot)))
                    };
                    #[cfg(macinrender_output)]
                    if let Some((target_id, slot)) = active_target
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
                        #[allow(unused_mut)]
                        let mut send_fresh = false;
                        #[allow(unused_mut)]
                        let mut allow_send = true;
                        #[cfg(posebridge_input)]
                        if using_bridge {
                            send_fresh = bridge_new_sample;
                            allow_send =
                                consumer.active || last_sent.is_none_or(|(id, _)| id != target_id);
                        }
                        if allow_send
                            && (send_fresh
                                || last_sent.is_none_or(|(previous_id, previous)| {
                                    previous_id != target_id
                                        || previous
                                            .into_iter()
                                            .zip(pose)
                                            .any(|(a, b)| (a - b).abs() >= 0.05)
                                }))
                            && control.orientation(pose).is_ok()
                        {
                            last_sent = Some((target_id, pose));
                        }
                    }
                    let fast = cfg!(posebridge_input)
                        && desired.enabled
                        && desired.source == HeadSource::PoseBridge;
                    thread::park_timeout(Duration::from_millis(if fast { 5 } else { 16 }));
                }
            })
            .ok();
        #[cfg(posebridge_input)]
        if let Some(join) = &join {
            bridge.set_listener(join.thread().clone());
        }
        Self {
            #[cfg(posebridge_input)]
            bridge,
            desired,
            mirror,
            stop,
            join,
            #[cfg(macinrender_output)]
            target,
            #[cfg(macinrender_output)]
            target_revision,
        }
    }
    pub fn mirror(&self) -> Arc<PoseMirror> {
        Arc::clone(&self.mirror)
    }
    pub fn snapshot(&self) -> HeadSnapshot {
        self.mirror.snapshot()
    }
    pub fn configure(&self, source: HeadSource, enabled: bool, system: bool) {
        #[cfg(posebridge_input)]
        if source != HeadSource::PoseBridge || !enabled {
            self.bridge.stop_tracking();
        }
        let mut desired = self
            .desired
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if source != desired.source || !enabled {
            desired.held_pose = None;
        }
        desired.source = source;
        desired.enabled = enabled;
        desired.system = system;
    }
    #[cfg(posebridge_input)]
    pub fn configure_bridge(&self, preferences: &crate::posebridge::Preferences) {
        let mut desired = self
            .desired
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        desired.bridge_max_age_ms = preferences.max_age_ms;
        desired.bridge_smoothing_ms = preferences.smoothing_ms;
        desired.bridge_enhanced = preferences.enhanced_tracking;
    }
    pub fn manual(&self, euler: [f32; 3]) {
        let mut desired = self
            .desired
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        desired.manual = Quaternion::from_euler(euler);
        desired.held_pose = None;
        desired.revision += 1;
        desired.source = HeadSource::Manual;
    }
    pub fn recenter(&self) {
        let mut desired = self
            .desired
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        desired.held_pose = None;
        desired.recenter += 1;
    }
    /// Freeze the presented pose without changing the source or disconnecting
    /// its sensor. Releasing resumes samples in the same reference frame.
    pub fn toggle_hold(&self) {
        let mut desired = self
            .desired
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if desired.enabled && desired.source != HeadSource::Off {
            desired.held_pose = if desired.held_pose.is_some() {
                None
            } else {
                Some(self.snapshot().pose)
            };
        }
        drop(desired);
        if let Some(join) = &self.join {
            join.thread().unpark();
        }
    }
    #[cfg(macinrender_output)]
    pub fn set_target(&self, target: Option<NativeTarget>) {
        let mut current = self
            .target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *current = target;
        // Allocators can reuse the old slot address during a fast handoff.
        // A new output must receive the pose even when the head has not moved.
        self.target_revision.fetch_add(1, Ordering::Relaxed);
    }
}
impl Drop for HeadTracker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            join.thread().unpark();
            let _ = join.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_zxy_output_preserves_compound_rotations_wrap_and_pitch_poles() {
        // Construct without the manual UI's 85-degree pitch clamp.
        let physical = |[yaw, pitch, roll]: [f64; 3]| {
            let axis = |index, degrees: f64| {
                let half = degrees.to_radians() / 2.;
                let mut q = [half.cos(), 0., 0., 0.];
                q[index] = half.sin();
                Quaternion(q)
            };
            axis(3, yaw)
                .multiply(axis(1, pitch))
                .multiply(axis(2, roll))
        };
        for yaw in [-180., -179.9, 30., 179.9, 180.] {
            for pitch in [-90., -89.99, 20., 89.99, 90.] {
                for roll in [-40., 0., 35.] {
                    let input = physical([yaw, pitch, roll]);
                    let output = physical(input.euler().map(f64::from));
                    let dot = input
                        .0
                        .iter()
                        .zip(output.0)
                        .map(|(a, b)| a * b)
                        .sum::<f64>();
                    assert!(
                        (dot.abs() - 1.).abs() < 1e-10,
                        "{yaw} {pitch} {roll}: {:?}",
                        input.euler()
                    );
                }
            }
        }
    }

    fn until(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !condition() {
            assert!(Instant::now() < deadline, "head control timed out");
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn holding_preserves_the_presented_pose_and_source() {
        let tracker = HeadTracker::new();
        tracker.configure(HeadSource::Manual, true, false);
        tracker.manual([45.0, 20.0, -10.0]);
        until(|| (tracker.snapshot().pose.euler()[0] - 45.0).abs() < 0.01);
        tracker.toggle_hold();
        until(|| tracker.snapshot().status == HeadStatus::Held);
        let held = tracker.snapshot().pose;
        assert!((held.euler()[0] - 45.0).abs() < 0.01);
        // Unrelated output settings may configure the same source again.
        tracker.configure(HeadSource::Manual, true, false);
        thread::sleep(Duration::from_millis(50));
        assert_eq!(tracker.snapshot().pose, held);
        assert_eq!(tracker.snapshot().status, HeadStatus::Held);
        tracker.toggle_hold();
        until(|| tracker.snapshot().status == HeadStatus::Manual);
        assert!((tracker.snapshot().pose.euler()[0] - 45.0).abs() < 0.01);
    }

    #[test]
    fn explicit_head_controls_release_a_hold() {
        let tracker = HeadTracker::new();
        for action in 0..4 {
            tracker.configure(HeadSource::Manual, true, false);
            tracker.manual([45.0, 0.0, 0.0]);
            until(|| (tracker.snapshot().pose.euler()[0] - 45.0).abs() < 0.01);
            tracker.toggle_hold();
            until(|| tracker.snapshot().status == HeadStatus::Held);
            let (status, yaw) = match action {
                0 => {
                    tracker.manual([-30.0, 0.0, 0.0]);
                    (HeadStatus::Manual, -30.0)
                }
                1 => {
                    tracker.recenter();
                    (HeadStatus::Manual, 0.0)
                }
                2 => {
                    tracker.configure(HeadSource::Off, true, false);
                    (HeadStatus::Fixed, 0.0)
                }
                _ => {
                    tracker.configure(HeadSource::Manual, false, true);
                    (HeadStatus::System, 0.0)
                }
            };
            until(|| {
                let head = tracker.snapshot();
                head.status == status && (head.pose.euler()[0] - yaw).abs() < 0.01
            });
        }
        // A system-owned pose cannot acquire a hidden hold to apply later.
        tracker.toggle_hold();
        tracker.configure(HeadSource::Manual, true, false);
        until(|| tracker.snapshot().status == HeadStatus::Manual);
    }

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

#[cfg(all(test, posebridge_input))]
mod bridge_tests {
    use super::*;
    use crate::posebridge::Sample;
    fn until(mut condition: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(2);
        while !condition() {
            assert!(Instant::now() < end, "head control timed out");
            thread::sleep(Duration::from_millis(2));
        }
    }
    fn publish(tracker: &HeadTracker, sequence: u64, angles: [f32; 3], fresh: bool) {
        *tracker.bridge.sample.lock().unwrap() = Some(Sample {
            instance: 1,
            session: 1,
            reference: 1,
            sequence,
            angles,
            fresh,
            age: Duration::ZERO,
            queried_at: Instant::now(),
        });
        tracker.join.as_ref().unwrap().thread().unpark();
    }
    #[test]
    fn twenty_hz_worker_supports_live_toggle_hold_and_stop_without_reconnection() {
        use crate::posebridge::{Preferences, enhancement::State, service::Command};
        let tracker = HeadTracker::new();
        let mut prefs = Preferences {
            smoothing_ms: 0.,
            max_age_ms: 500,
            ..Preferences::default()
        };
        tracker.configure_bridge(&prefs);
        tracker.configure(HeadSource::PoseBridge, true, false);
        tracker.bridge.send(Command::Simulate { rate: 20 }).unwrap();
        until(|| tracker.bridge.enhancement.lock().unwrap().samples >= 10);
        assert_eq!(
            tracker.bridge.enhancement.lock().unwrap().state,
            State::Normal
        );
        let request = tracker.bridge.view().request;
        prefs.enhanced_tracking = true;
        tracker.configure_bridge(&prefs);
        until(|| tracker.bridge.enhancement.lock().unwrap().state == State::Predicting);
        assert_eq!(tracker.bridge.view().request, request);
        assert!(tracker.snapshot().measurement.is_some());
        tracker.toggle_hold();
        until(|| tracker.snapshot().status == HeadStatus::Held);
        let held = tracker.snapshot().pose;
        thread::sleep(Duration::from_millis(75));
        assert_eq!(tracker.snapshot().pose, held);
        tracker.toggle_hold();
        until(|| tracker.snapshot().status == HeadStatus::BridgeActive);
        prefs.enhanced_tracking = false;
        tracker.configure_bridge(&prefs);
        until(|| tracker.bridge.enhancement.lock().unwrap().state == State::Normal);
        assert_eq!(tracker.bridge.view().request, request);
        tracker.bridge.send(Command::Stop).unwrap();
        until(|| tracker.snapshot().status == HeadStatus::BridgeFrozen);
        let frozen = tracker.snapshot().pose;
        thread::sleep(Duration::from_millis(25));
        assert_eq!(tracker.snapshot().pose, frozen);
    }
    #[test]
    fn hidden_window_independent_control_freezes_presented_pose() {
        let tracker = HeadTracker::new();
        tracker.configure_bridge(&crate::posebridge::Preferences {
            smoothing_ms: 0.0,
            ..Default::default()
        });
        tracker.configure(HeadSource::PoseBridge, true, false);
        publish(&tracker, 1, [0.; 3], true);
        until(|| tracker.snapshot().status == HeadStatus::BridgeActive);
        publish(&tracker, 2, [60., 20., 10.], true);
        until(|| (tracker.snapshot().pose.euler()[0] - 60.).abs() < 0.01);
        let presented = tracker.snapshot().pose;
        publish(&tracker, 3, [-90., 0., 0.], false);
        until(|| tracker.snapshot().status == HeadStatus::BridgeFrozen);
        thread::sleep(Duration::from_millis(25));
        assert_eq!(tracker.snapshot().pose, presented);
        publish(&tracker, 4, [70., 20., 10.], true);
        until(|| tracker.snapshot().status == HeadStatus::BridgeActive);
        until(|| (tracker.snapshot().pose.euler()[0] - 70.).abs() < 0.01);
    }

    #[test]
    fn releasing_hold_uses_the_current_sample_in_the_original_reference() {
        let tracker = HeadTracker::new();
        tracker.configure_bridge(&crate::posebridge::Preferences {
            smoothing_ms: 0.0,
            max_age_ms: 500,
            ..Default::default()
        });
        tracker.configure(HeadSource::PoseBridge, true, false);
        publish(&tracker, 1, [0.0; 3], true);
        until(|| tracker.snapshot().status == HeadStatus::BridgeActive);
        publish(&tracker, 2, [40.0, 20.0, 10.0], true);
        until(|| (tracker.snapshot().pose.euler()[0] - 40.0).abs() < 0.01);
        tracker.toggle_hold();
        until(|| tracker.snapshot().status == HeadStatus::Held);
        let held = tracker.snapshot().pose;
        publish(&tracker, 3, [70.0, 20.0, 10.0], true);
        thread::sleep(Duration::from_millis(25));
        assert_eq!(tracker.snapshot().pose, held);
        tracker.toggle_hold();
        until(|| {
            let snapshot = tracker.snapshot();
            snapshot.status == HeadStatus::BridgeActive
                && (snapshot.pose.euler()[0] - 70.0).abs() < 0.01
        });
    }
}
