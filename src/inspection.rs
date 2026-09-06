use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;

use crate::media::MediaSource;
use macindecode_ac4_inspect::{
    FieldStatus, InspectReport, ReportedField, inspect_mp4_reader, inspect_raw_reader,
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
    id: u64,
    path: PathBuf,
    result: Result<InspectionSnapshot, String>,
}

struct InspectionRequest {
    id: u64,
    source: MediaSource,
    cancel: Arc<AtomicBool>,
}

struct PendingRequest {
    id: u64,
    path: PathBuf,
    cancel: Arc<AtomicBool>,
}

pub struct InspectionController {
    request_sender: Option<Sender<InspectionRequest>>,
    result_receiver: Receiver<InspectionResult>,
    states: HashMap<PathBuf, InspectionState>,
    startup_error: Option<String>,
    next_id: u64,
    pending: Option<PendingRequest>,
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
                next_id: 0,
                pending: None,
            },
            Err(error) => Self {
                request_sender: None,
                result_receiver,
                states: HashMap::new(),
                startup_error: Some(format!("Failed to start inspection worker: {error}")),
                next_id: 0,
                pending: None,
            },
        }
    }

    pub fn ensure_requested(&mut self, path: &Path) {
        self.ensure_requested_source(MediaSource::new(path));
    }

    pub fn ensure_requested_source(&mut self, source: MediaSource) {
        let path = source.path();
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.path != path)
        {
            self.cancel_pending();
        }
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
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);
        let cancel = Arc::new(AtomicBool::new(false));
        self.pending = Some(PendingRequest {
            id: self.next_id,
            path: owned_path.clone(),
            cancel: Arc::clone(&cancel),
        });
        if sender
            .send(InspectionRequest {
                id: self.next_id,
                source,
                cancel,
            })
            .is_err()
        {
            self.pending = None;
            self.states.insert(
                owned_path,
                InspectionState::Failed("Inspection worker stopped unexpectedly".to_owned()),
            );
        }
    }

    pub fn retry(&mut self, path: &Path) {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.path == path)
        {
            self.cancel_pending();
        }
        self.states.remove(path);
        self.ensure_requested(path);
    }

    pub fn poll(&mut self) {
        while let Ok(result) = self.result_receiver.try_recv() {
            if self
                .pending
                .as_ref()
                .is_none_or(|pending| pending.id != result.id || pending.path != result.path)
            {
                continue;
            }
            self.pending = None;
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
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| !retained.contains(&pending.path))
        {
            self.cancel_pending();
        }
        self.states
            .retain(|path, _state| retained.iter().any(|candidate| candidate == path));
    }

    pub fn has_pending(&self) -> bool {
        self.states
            .values()
            .any(|state| matches!(state, InspectionState::Pending))
    }

    fn cancel_pending(&mut self) {
        if let Some(pending) = self.pending.take() {
            pending.cancel.store(true, Ordering::Release);
            self.states.remove(&pending.path);
        }
    }
}

impl Drop for InspectionController {
    fn drop(&mut self) {
        self.cancel_pending();
    }
}

struct CancellableReader<R> {
    reader: R,
    cancel: Arc<AtomicBool>,
}

impl<R> CancellableReader<R> {
    fn check(&self) -> io::Result<()> {
        if self.cancel.load(Ordering::Acquire) {
            // read_exact retries Interrupted; cancellation must actually terminate IO.
            Err(io::Error::other("Inspection cancelled"))
        } else {
            Ok(())
        }
    }
}
impl<R: Read> Read for CancellableReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.check()?;
        self.reader.read(buffer)
    }
}
impl<R: Seek> Seek for CancellableReader<R> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.check()?;
        self.reader.seek(position)
    }
    fn seek_relative(&mut self, offset: i64) -> io::Result<()> {
        self.check()?;
        self.reader.seek_relative(offset)
    }
}

fn inspection_worker(
    request_receiver: &Receiver<InspectionRequest>,
    result_sender: &Sender<InspectionResult>,
) {
    while let Ok(request) = request_receiver.recv() {
        if request.cancel.load(Ordering::Acquire) {
            continue;
        }
        let path = request.source.path().to_path_buf();
        let result = request.source.open().and_then(|source| {
            let mut reader = CancellableReader {
                reader: source.reader(),
                cancel: Arc::clone(&request.cancel),
            };
            let name = path.display().to_string();
            let report = if source.is_raw() {
                inspect_raw_reader(&mut reader, &name)
            } else {
                let metadata = source.mp4_metadata()?;
                inspect_mp4_reader(&mut reader, &metadata, &name)
            };
            report
                .map(InspectionSnapshot::new)
                .map_err(|error| error.to_string())
        });
        if request.cancel.load(Ordering::Acquire) {
            continue;
        }
        if result_sender
            .send(InspectionResult {
                id: request.id,
                path,
                result,
            })
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
    fn switching_or_retrying_cancels_work_and_rejects_late_results() {
        let (requests, queued) = mpsc::channel();
        let (results, received) = mpsc::channel();
        let mut controller = InspectionController {
            request_sender: Some(requests),
            result_receiver: received,
            states: HashMap::new(),
            startup_error: None,
            next_id: 0,
            pending: None,
        };
        let a = Path::new("first.ac4");
        let b = Path::new("second.ac4");
        controller.ensure_requested(a);
        let old = queued.recv().unwrap();
        controller.ensure_requested(b);
        let replaced = queued.recv().unwrap();
        assert!(old.cancel.load(Ordering::Acquire));
        assert!(controller.state(a).is_none());
        controller.retry(b);
        let current = queued.recv().unwrap();
        assert!(replaced.cancel.load(Ordering::Acquire));
        results
            .send(InspectionResult {
                id: replaced.id,
                path: b.to_path_buf(),
                result: Err("stale".into()),
            })
            .unwrap();
        controller.poll();
        assert!(matches!(
            controller.state(b),
            Some(InspectionState::Pending)
        ));
        results
            .send(InspectionResult {
                id: current.id,
                path: b.to_path_buf(),
                result: Err("current".into()),
            })
            .unwrap();
        controller.poll();
        assert!(
            matches!(controller.state(b), Some(InspectionState::Failed(error)) if error == "current")
        );
    }

    #[test]
    fn cancellation_terminates_read_exact_even_with_buffered_input() {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut reader = CancellableReader {
            reader: io::Cursor::new([1, 2, 3, 4]),
            cancel: Arc::clone(&cancel),
        };
        let mut one = [0];
        reader.read_exact(&mut one).unwrap();
        cancel.store(true, Ordering::Release);
        assert_eq!(
            reader.read_exact(&mut one).unwrap_err().kind(),
            io::ErrorKind::Other
        );
        assert!(reader.seek_relative(1).is_err());
    }

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
