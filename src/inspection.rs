use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use crate::media::MediaSource;
use macindecode_ac4_inspect::{
    FieldStatus, InspectReport, InspectSourceHint, ReportedField, inspect_bytes,
};
use serde_json::Value;

#[derive(Debug)]
pub struct InspectionSnapshot {
    pub report: InspectReport,
    pub full_text: String,
}

impl InspectionSnapshot {
    fn new(report: InspectReport) -> Self {
        let full_text = report.render_text();
        Self { report, full_text }
    }
}

#[derive(Debug)]
pub enum InspectionState {
    Pending,
    Ready(Box<InspectionSnapshot>),
    Failed(String),
}

#[derive(Debug)]
struct InspectionResult {
    path: PathBuf,
    result: Result<InspectionSnapshot, String>,
}

pub struct InspectionController {
    request_sender: Option<Sender<MediaSource>>,
    result_receiver: Receiver<InspectionResult>,
    states: HashMap<PathBuf, InspectionState>,
    startup_error: Option<String>,
}

impl InspectionController {
    pub fn new() -> Self {
        let (request_sender, request_receiver) = mpsc::channel();
        let (result_sender, result_receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("ac4-inspection".to_owned())
            .spawn(move || inspection_worker(&request_receiver, &result_sender));

        match worker {
            Ok(_) => Self {
                request_sender: Some(request_sender),
                result_receiver,
                states: HashMap::new(),
                startup_error: None,
            },
            Err(error) => Self {
                request_sender: None,
                result_receiver,
                states: HashMap::new(),
                startup_error: Some(format!("Failed to start inspection worker: {error}")),
            },
        }
    }

    pub fn ensure_requested(&mut self, path: &Path) {
        self.ensure_requested_source(MediaSource::new(path));
    }

    pub fn ensure_requested_source(&mut self, source: MediaSource) {
        let path = source.path();
        if self.states.contains_key(path) {
            return;
        }

        let owned_path = path.to_path_buf();
        let Some(sender) = self.request_sender.as_ref() else {
            self.states.insert(
                owned_path,
                InspectionState::Failed(
                    self.startup_error
                        .clone()
                        .unwrap_or_else(|| "Inspection worker is unavailable".to_owned()),
                ),
            );
            return;
        };

        self.states
            .insert(owned_path.clone(), InspectionState::Pending);
        if sender.send(source).is_err() {
            self.states.insert(
                owned_path,
                InspectionState::Failed("Inspection worker stopped unexpectedly".to_owned()),
            );
        }
    }

    pub fn retry(&mut self, path: &Path) {
        self.states.remove(path);
        self.ensure_requested(path);
    }

    pub fn poll(&mut self) {
        while let Ok(result) = self.result_receiver.try_recv() {
            if !self.states.contains_key(&result.path) {
                continue;
            }
            let state = match result.result {
                Ok(snapshot) => InspectionState::Ready(Box::new(snapshot)),
                Err(error) => InspectionState::Failed(error),
            };
            self.states.insert(result.path, state);
        }
    }

    pub fn state(&self, path: &Path) -> Option<&InspectionState> {
        self.states.get(path)
    }

    pub fn retain_paths<'a>(&mut self, paths: impl IntoIterator<Item = &'a Path>) {
        let retained = paths.into_iter().map(Path::to_path_buf).collect::<Vec<_>>();
        self.states
            .retain(|path, _state| retained.iter().any(|candidate| candidate == path));
    }

    pub fn has_pending(&self) -> bool {
        self.states
            .values()
            .any(|state| matches!(state, InspectionState::Pending))
    }
}

fn inspection_worker(
    request_receiver: &Receiver<MediaSource>,
    result_sender: &Sender<InspectionResult>,
) {
    while let Ok(source) = request_receiver.recv() {
        let path = source.path().to_path_buf();
        let result = source.read().and_then(|bytes| {
            inspect_bytes(
                &bytes,
                InspectSourceHint {
                    name: Some(&path.display().to_string()),
                    ..Default::default()
                },
            )
            .map(InspectionSnapshot::new)
            .map_err(|error| error.to_string())
        });
        if result_sender
            .send(InspectionResult { path, result })
            .is_err()
        {
            break;
        }
    }
}

pub fn field_text(field: &ReportedField, include_raw: bool) -> String {
    match field.status {
        FieldStatus::Present => present_field_text(field, include_raw),
        FieldStatus::NotPresent => "Not present".to_owned(),
        FieldStatus::NotApplicable => "Not applicable".to_owned(),
        FieldStatus::Unknown => unavailable_field_text("Unknown", field.reason.as_deref()),
        FieldStatus::Unsupported => unavailable_field_text("Unsupported", field.reason.as_deref()),
    }
}

pub fn field_summary_text(field: &ReportedField) -> String {
    match field.status {
        FieldStatus::Present => present_field_text(field, false),
        FieldStatus::NotPresent => "Not present".to_owned(),
        FieldStatus::NotApplicable => "Not applicable".to_owned(),
        FieldStatus::Unknown => "Unknown".to_owned(),
        FieldStatus::Unsupported => "Unsupported".to_owned(),
    }
}

fn present_field_text(field: &ReportedField, include_raw: bool) -> String {
    let mut text = field
        .value
        .as_ref()
        .map_or_else(|| "Present".to_owned(), value_text);
    if let Some(unit) = field.unit {
        text.push(' ');
        text.push_str(unit);
    }
    if include_raw && let Some(raw) = field.raw_code.as_ref() {
        text.push_str(" (raw: ");
        text.push_str(&value_text(raw));
        text.push(')');
    }
    text
}

fn unavailable_field_text(label: &str, reason: Option<&str>) -> String {
    reason.map_or_else(|| label.to_owned(), |reason| format!("{label}: {reason}"))
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Bool(value) => if *value { "True" } else { "False" }.to_owned(),
        Value::Number(number) => number.to_string(),
        Value::Object(object) => object
            .get("display")
            .and_then(Value::as_str)
            .map_or_else(|| value.to_string(), ToOwned::to_owned),
        Value::Array(_) => value.to_string(),
        Value::Null => "null".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use serde_json::json;

    use super::*;

    #[test]
    fn field_text_preserves_status_units_and_optional_raw_code() {
        let present = ReportedField {
            status: FieldStatus::Present,
            value: Some(json!(-18.0)),
            unit: Some("LKFS"),
            raw_code: Some(json!(844)),
            reason: None,
        };
        assert_eq!(field_text(&present, false), "-18.0 LKFS");
        assert_eq!(field_text(&present, true), "-18.0 LKFS (raw: 844)");
        assert_eq!(field_summary_text(&present), "-18.0 LKFS");

        let unavailable = ReportedField {
            status: FieldStatus::Unknown,
            value: None,
            unit: None,
            raw_code: None,
            reason: Some("value changes within the stream".to_owned()),
        };
        assert_eq!(
            field_text(&unavailable, true),
            "Unknown: value changes within the stream"
        );
        assert_eq!(field_summary_text(&unavailable), "Unknown");
    }

    #[test]
    fn missing_input_is_reported_by_the_background_worker() {
        let path = std::env::temp_dir().join(format!(
            "macindecode-ac4-player-missing-{}-{}.ac4",
            std::process::id(),
            1
        ));
        let mut controller = InspectionController::new();
        controller.ensure_requested(&path);

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            controller.poll();
            if matches!(controller.state(&path), Some(InspectionState::Failed(_))) {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("inspection worker did not report the missing input before the deadline");
    }
}
