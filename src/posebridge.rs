//! Player-owned preferences and consumption policy; device I/O stays on a worker.
use serde::{Deserialize, Serialize};

#[cfg(posebridge_input)]
pub mod service;
#[cfg(posebridge_input)]
pub mod ui;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transport {
    #[default]
    Ble,
    Usb,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Input {
    #[default]
    Automatic,
    RegisterQuaternion,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Device {
    pub transport: Transport,
    pub id: String,
    pub name: String,
    pub baud: u32,
    pub mounting: [i8; 3],
    pub input: Input,
    pub throughput: bool,
}
impl Default for Device {
    fn default() -> Self {
        Self {
            transport: Transport::Ble,
            id: String::new(),
            name: String::new(),
            baud: 115_200,
            mounting: [0; 3],
            input: Input::Automatic,
            throughput: false,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub device: Device,
    pub remembered: Vec<Device>,
    pub smoothing_ms: f32,
    pub max_age_ms: u32,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            device: Device::default(),
            remembered: Vec::new(),
            smoothing_ms: 10.0,
            max_age_ms: 100,
        }
    }
}
impl Preferences {
    pub fn validate(&mut self) {
        if !self.smoothing_ms.is_finite() {
            self.smoothing_ms = 10.0;
        }
        self.smoothing_ms = self.smoothing_ms.clamp(0.0, 50.0);
        self.max_age_ms = self.max_age_ms.clamp(50, 500);
        for device in std::iter::once(&mut self.device).chain(self.remembered.iter_mut()) {
            if device.baud == 0 {
                device.baud = 115_200;
            }
            if !cfg!(target_os = "windows") {
                device.throughput = false;
            }
        }
    }
    #[cfg(posebridge_input)]
    pub fn remember(&mut self) {
        if self.device.id.is_empty() {
            return;
        }
        self.remembered
            .retain(|d| d.transport != self.device.transport || d.id != self.device.id);
        self.remembered.push(self.device.clone());
    }
    #[cfg(posebridge_input)]
    pub fn select(&mut self, transport: Transport, id: String, name: String) {
        self.remember();
        self.device = self
            .remembered
            .iter()
            .find(|d| d.transport == transport && d.id == id)
            .cloned()
            .unwrap_or(Device {
                transport,
                id,
                name,
                ..Device::default()
            });
    }
}

#[cfg(any(posebridge_input, test))]
mod consumption;
#[cfg(posebridge_input)]
pub use consumption::{Consumer, Sample};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_preferences_and_invalid_latency_have_safe_defaults() {
        let mut p: Preferences = serde_json::from_str("{}").unwrap();
        assert_eq!(p.max_age_ms, 100);
        assert_eq!(p.device.mounting, [0; 3]);
        p.smoothing_ms = f32::NAN;
        p.max_age_ms = 0;
        p.validate();
        assert_eq!(p.smoothing_ms.to_bits(), 10.0_f32.to_bits());
        assert_eq!(p.max_age_ms, 50);
    }
}
