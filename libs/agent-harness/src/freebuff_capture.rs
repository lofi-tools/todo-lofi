use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{HarnessError, HarnessResult};

const CAPTURE_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct CaptureHeader {
    format: String,
    version: u32,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CaptureEvent {
    Runtime {
        launcher_version: Option<String>,
        runtime_version: Option<String>,
        target: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cached_runtime_sha256: Option<String>,
    },
    Terminal {
        term: String,
        color_term: String,
        renderer: String,
    },
    Exit {
        exit_code: Option<i32>,
        signal: Option<String>,
        operator_stopped: bool,
    },
    Input {
        bytes: Vec<u8>,
    },
    Output {
        bytes: Vec<u8>,
    },
    Resize {
        columns: u16,
        rows: u16,
    },
    Screen {
        state: Value,
    },
    Response {
        text: String,
        complete: bool,
    },
    WorktreeBase {
        identity: Value,
    },
    WorktreeEffect {
        path: PathBuf,
        contents: Option<Vec<u8>>,
    },
    WorktreeFile {
        path: PathBuf,
        contents: Vec<u8>,
        mode: Option<u32>,
    },
    WorktreeDeleted {
        path: PathBuf,
    },
    Complete,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureRecord {
    pub elapsed_micros: u64,
    pub event: CaptureEvent,
}

#[derive(Default)]
pub struct RuntimeChangeTracker {
    previous: Option<RuntimeObservation>,
    changed: bool,
}

#[derive(PartialEq, Eq)]
struct RuntimeObservation {
    launcher_version: Option<String>,
    runtime_version: Option<String>,
    target: Option<String>,
    cached_runtime_sha256: Option<String>,
}

impl RuntimeChangeTracker {
    pub fn observe(&mut self, event: &CaptureEvent) {
        let CaptureEvent::Runtime {
            launcher_version,
            runtime_version,
            target,
            cached_runtime_sha256,
        } = event
        else {
            return;
        };
        let current = RuntimeObservation {
            launcher_version: launcher_version.clone(),
            runtime_version: runtime_version.clone(),
            target: target.clone(),
            cached_runtime_sha256: cached_runtime_sha256.clone(),
        };
        if self
            .previous
            .as_ref()
            .is_some_and(|previous| previous != &current)
        {
            self.changed = true;
        }
        self.previous = Some(current);
    }

    pub fn changed(&self) -> bool {
        self.changed
    }
}

pub struct CaptureWriter {
    file: File,
    path: PathBuf,
    last_elapsed_micros: Option<u64>,
    write_failed: bool,
    finished: bool,
}

impl CaptureWriter {
    pub fn create(workspace: &Path, scenario: &str) -> HarnessResult<Self> {
        if scenario.is_empty()
            || !scenario
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(HarnessError::Protocol(
                "invalid Freebuff capture name".into(),
            ));
        }
        let directory = workspace.join(".cache/shepherd-live/freebuff");
        fs::create_dir_all(&directory).map_err(|error| {
            HarnessError::Protocol(format!(
                "could not create Freebuff capture directory {}: {error}",
                directory.display()
            ))
        })?;
        let path = directory.join(format!("{scenario}.jsonl"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(|error| {
            HarnessError::Protocol(format!(
                "could not create Freebuff capture at {}: {error}",
                path.display()
            ))
        })?;
        let header = CaptureHeader {
            format: "shepherd-freebuff".into(),
            version: CAPTURE_VERSION,
        };
        serde_json::to_writer(&mut file, &header).map_err(|error| {
            HarnessError::Protocol(format!(
                "could not write Freebuff capture header at {}: {error}",
                path.display()
            ))
        })?;
        file.write_all(b"\n")
            .and_then(|()| file.flush())
            .map_err(|error| {
                HarnessError::Protocol(format!(
                    "could not write Freebuff capture header at {}: {error}",
                    path.display()
                ))
            })?;
        Ok(Self {
            file,
            path,
            last_elapsed_micros: None,
            write_failed: false,
            finished: false,
        })
    }

    pub fn append(&mut self, record: &CaptureRecord) -> HarnessResult<()> {
        if self.finished {
            return Err(HarnessError::Protocol(
                "Freebuff capture is already complete".into(),
            ));
        }
        if self.write_failed {
            return Err(HarnessError::Protocol(format!(
                "Freebuff capture at {} previously failed; recording cannot continue",
                self.path.display()
            )));
        }
        validate_record(record)?;
        if self
            .last_elapsed_micros
            .is_some_and(|previous| record.elapsed_micros < previous)
        {
            return Err(HarnessError::Protocol(
                "Freebuff capture events are out of order".into(),
            ));
        }
        let mut line = serde_json::to_vec(record).map_err(|error| {
            HarnessError::Protocol(format!("could not encode Freebuff capture: {error}"))
        })?;
        line.push(b'\n');
        if let Err(error) = self.file.write_all(&line).and_then(|()| self.file.flush()) {
            self.write_failed = true;
            return Err(HarnessError::Protocol(format!(
                "could not write Freebuff capture at {}: {error}",
                self.path.display()
            )));
        }
        self.last_elapsed_micros = Some(record.elapsed_micros);
        self.finished = matches!(record.event, CaptureEvent::Complete);
        Ok(())
    }
}

pub struct CaptureSession {
    started: Instant,
    state: Mutex<CaptureSessionState>,
}

struct CaptureSessionState {
    writer: CaptureWriter,
    failed: bool,
    finished: bool,
}

impl CaptureSession {
    pub fn new(writer: CaptureWriter) -> Self {
        Self {
            started: Instant::now(),
            state: Mutex::new(CaptureSessionState {
                writer,
                failed: false,
                finished: false,
            }),
        }
    }

    /// Serialize terminal operations with their capture and derived screen events.
    /// The first event records an attempt; an error leaves an incomplete capture.
    /// The callback must not re-enter this session.
    pub fn record_and_apply<T>(
        &self,
        event: CaptureEvent,
        apply: impl FnOnce() -> HarnessResult<(T, Vec<CaptureEvent>)>,
    ) -> HarnessResult<T> {
        self.record_apply_and_publish(event, apply, |_| Ok(()))
    }

    pub fn record_apply_and_publish<T>(
        &self,
        event: CaptureEvent,
        apply: impl FnOnce() -> HarnessResult<(T, Vec<CaptureEvent>)>,
        publish: impl FnOnce(&T) -> HarnessResult<()>,
    ) -> HarnessResult<T> {
        let mut state = self.state.lock().map_err(|_| {
            HarnessError::Protocol("Freebuff capture session lock is poisoned".into())
        })?;
        if state.failed || state.finished {
            return Err(HarnessError::Protocol(
                "Freebuff capture session has failed or finished".into(),
            ));
        }
        if matches!(event, CaptureEvent::Complete) {
            return Err(HarnessError::Protocol(
                "use finish to complete a Freebuff capture".into(),
            ));
        }
        let outcome = (|| {
            self.append_event(&mut state.writer, event)?;
            let (result, derived_events) = apply()?;
            for derived_event in derived_events {
                if matches!(derived_event, CaptureEvent::Complete) {
                    return Err(HarnessError::Protocol(
                        "completion cannot be a derived capture event".into(),
                    ));
                }
                self.append_event(&mut state.writer, derived_event)?;
            }
            publish(&result)?;
            Ok(result)
        })();
        if outcome.is_err() {
            state.failed = true;
        }
        outcome
    }

    pub fn fail(&self) -> HarnessResult<()> {
        let mut state = self.state.lock().map_err(|_| {
            HarnessError::Protocol("Freebuff capture session lock is poisoned".into())
        })?;
        state.failed = true;
        Ok(())
    }

    /// Run a nonblocking operation while serialized with capture. `None` means
    /// no progress; callers wait outside the lock before trying again.
    pub fn try_apply_and_record<T>(
        &self,
        apply: impl FnOnce() -> HarnessResult<Option<(T, CaptureEvent)>>,
    ) -> HarnessResult<Option<T>> {
        let mut state = self.state.lock().map_err(|_| {
            HarnessError::Protocol("Freebuff capture session lock is poisoned".into())
        })?;
        if state.failed || state.finished {
            return Err(HarnessError::Protocol(
                "Freebuff capture session has failed or finished".into(),
            ));
        }
        let outcome = (|| {
            let Some((result, event)) = apply()? else {
                return Ok(None);
            };
            if matches!(event, CaptureEvent::Complete) {
                return Err(HarnessError::Protocol(
                    "completion cannot be an operation event".into(),
                ));
            }
            self.append_event(&mut state.writer, event)?;
            Ok(Some(result))
        })();
        if outcome.is_err() {
            state.failed = true;
        }
        outcome
    }

    /// Call only after the PTY, screen, responses and effects are fully drained.
    pub fn finish(&self) -> HarnessResult<()> {
        let mut state = self.state.lock().map_err(|_| {
            HarnessError::Protocol("Freebuff capture session lock is poisoned".into())
        })?;
        if state.failed || state.finished {
            return Err(HarnessError::Protocol(
                "Freebuff capture session has failed or finished".into(),
            ));
        }
        if let Err(error) = self.append_event(&mut state.writer, CaptureEvent::Complete) {
            state.failed = true;
            return Err(error);
        }
        state.finished = true;
        Ok(())
    }

    fn append_event(&self, writer: &mut CaptureWriter, event: CaptureEvent) -> HarnessResult<()> {
        let elapsed_micros = u64::try_from(self.started.elapsed().as_micros()).map_err(|_| {
            HarnessError::Protocol("Freebuff capture duration exceeds its format limit".into())
        })?;
        writer.append(&CaptureRecord {
            elapsed_micros,
            event,
        })
    }
}

pub fn read_capture(path: &Path) -> HarnessResult<Vec<CaptureRecord>> {
    let mut records = Vec::new();
    visit_capture(path, |record| {
        records.push(record);
        Ok(())
    })?;
    Ok(records)
}

pub fn read_complete_capture(path: &Path) -> HarnessResult<Vec<CaptureRecord>> {
    let mut records = Vec::new();
    visit_complete_capture(path, |record| {
        records.push(record);
        Ok(())
    })?;
    Ok(records)
}

/// Visitors must defer external effects until this function returns successfully.
pub fn visit_complete_capture(
    path: &Path,
    mut visit: impl FnMut(CaptureRecord) -> HarnessResult<()>,
) -> HarnessResult<()> {
    let mut file = File::open(path).map_err(|error| {
        HarnessError::Protocol(format!(
            "could not open Freebuff capture at {}: {error}",
            path.display()
        ))
    })?;
    let mut last = [0u8; 1];
    file.seek(SeekFrom::End(-1))
        .and_then(|_| file.read_exact(&mut last))
        .map_err(|error| {
            HarnessError::Protocol(format!(
                "could not read Freebuff capture ending at {}: {error}",
                path.display()
            ))
        })?;
    if last != [b'\n'] {
        return Err(HarnessError::Protocol(format!(
            "Freebuff capture at {} has an incomplete final record",
            path.display()
        )));
    }
    let mut complete = false;
    visit_capture(path, |record| {
        complete = matches!(record.event, CaptureEvent::Complete);
        visit(record)
    })?;
    if !complete {
        return Err(HarnessError::Protocol(format!(
            "Freebuff capture at {} is incomplete",
            path.display()
        )));
    }
    Ok(())
}

pub fn visit_capture(
    path: &Path,
    mut visit: impl FnMut(CaptureRecord) -> HarnessResult<()>,
) -> HarnessResult<()> {
    let file = File::open(path).map_err(|error| {
        HarnessError::Protocol(format!(
            "could not open Freebuff capture at {}: {error}",
            path.display()
        ))
    })?;
    let mut lines = BufReader::new(file).lines();
    let header = lines
        .next()
        .ok_or_else(|| {
            HarnessError::Protocol(format!(
                "missing Freebuff capture header at {}",
                path.display()
            ))
        })?
        .map_err(|error| {
            HarnessError::Protocol(format!(
                "could not read Freebuff capture header at {}: {error}",
                path.display()
            ))
        })?;
    let header: CaptureHeader = serde_json::from_str(&header).map_err(|error| {
        HarnessError::Protocol(format!(
            "invalid Freebuff capture header at {}: {error}",
            path.display()
        ))
    })?;
    if header.format != "shepherd-freebuff" || header.version != CAPTURE_VERSION {
        return Err(HarnessError::Protocol(format!(
            "unsupported Freebuff capture format or version at {}",
            path.display()
        )));
    }
    let mut previous_elapsed_micros = None;
    let mut finished = false;
    for (index, line) in lines.enumerate() {
        let line = line.map_err(|error| {
            HarnessError::Protocol(format!(
                "could not read Freebuff capture at {} line {}: {error}",
                path.display(),
                index + 2
            ))
        })?;
        let record: CaptureRecord = serde_json::from_str(&line).map_err(|error| {
            HarnessError::Protocol(format!(
                "invalid Freebuff capture at {} line {}: {error}",
                path.display(),
                index + 2
            ))
        })?;
        if finished {
            return Err(HarnessError::Protocol(format!(
                "Freebuff capture at {} contains events after completion",
                path.display()
            )));
        }
        validate_record(&record)?;
        if previous_elapsed_micros.is_some_and(|previous| previous > record.elapsed_micros) {
            return Err(HarnessError::Protocol(format!(
                "Freebuff capture at {} is out of order",
                path.display()
            )));
        }
        previous_elapsed_micros = Some(record.elapsed_micros);
        finished = matches!(record.event, CaptureEvent::Complete);
        visit(record)?;
    }
    Ok(())
}

fn validate_record(record: &CaptureRecord) -> HarnessResult<()> {
    let path = match &record.event {
        CaptureEvent::WorktreeEffect { path, .. }
        | CaptureEvent::WorktreeFile { path, .. }
        | CaptureEvent::WorktreeDeleted { path } => Some(path),
        _ => None,
    };
    if let Some(path) = path
        && (path.as_os_str().is_empty()
            || path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_))))
    {
        return Err(HarnessError::Protocol(
            "invalid Freebuff capture worktree path".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_changes_remain_flagged_after_metadata_returns_to_its_original_value() {
        let runtime = |version: &str, hash: Option<&str>| CaptureEvent::Runtime {
            launcher_version: Some("0.0.171".into()),
            runtime_version: Some(version.into()),
            target: Some("darwin-arm64".into()),
            cached_runtime_sha256: hash.map(str::to_owned),
        };
        let mut tracker = RuntimeChangeTracker::default();
        tracker.observe(&runtime("0.2.36", Some("first")));
        tracker.observe(&runtime("0.2.36", Some("first")));
        assert!(!tracker.changed());
        tracker.observe(&runtime("0.2.36", Some("second")));
        tracker.observe(&runtime("0.2.36", Some("first")));
        assert!(tracker.changed());
    }

    #[test]
    fn nonblocking_attempts_record_only_progress_and_stop_after_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let writer = CaptureWriter::create(directory.path(), "nonblocking")?;
        let path = writer.path.clone();
        let session = CaptureSession::new(writer);
        let pending: Option<usize> = session.try_apply_and_record(|| Ok(None))?;
        assert!(pending.is_none());
        assert_eq!(
            session.try_apply_and_record(|| Ok(Some((
                2,
                CaptureEvent::Input {
                    bytes: b"hi".to_vec()
                }
            ))))?,
            Some(2)
        );
        let failed: HarnessResult<Option<usize>> =
            session.try_apply_and_record(|| Err(HarnessError::Protocol("failed".into())));
        assert!(failed.is_err());
        let mut ran = false;
        let blocked: HarnessResult<Option<usize>> = session.try_apply_and_record(|| {
            ran = true;
            Ok(None)
        });
        assert!(blocked.is_err());
        assert!(!ran);
        let records = read_capture(&path)?;
        assert_eq!(records.len(), 1);
        assert!(matches!(&records[0].event, CaptureEvent::Input { bytes } if bytes == b"hi"));
        Ok(())
    }

    #[test]
    fn complete_capture_requires_a_final_marker_and_rejects_later_events()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let writer = CaptureWriter::create(directory.path(), "complete")?;
        let path = writer.path.clone();
        let session = CaptureSession::new(writer);
        session.record_and_apply(
            CaptureEvent::Input {
                bytes: b"hello".to_vec(),
            },
            || Ok(((), Vec::new())),
        )?;
        assert!(read_complete_capture(&path).is_err());
        session.finish()?;
        assert_eq!(read_complete_capture(&path)?.len(), 2);
        let size = fs::metadata(&path)?.len();
        OpenOptions::new()
            .write(true)
            .open(&path)?
            .set_len(size - 1)?;
        assert!(read_complete_capture(&path).is_err());
        OpenOptions::new()
            .append(true)
            .open(&path)?
            .write_all(b"\n")?;
        assert!(
            session
                .record_and_apply(CaptureEvent::Output { bytes: vec![] }, || Ok((
                    (),
                    Vec::new()
                )))
                .is_err()
        );
        assert!(session.finish().is_err());
        OpenOptions::new()
            .append(true)
            .open(&path)?
            .write_all(b"{\"elapsed_micros\":0,\"event\":{\"kind\":\"complete\"}}\n")?;
        assert!(read_complete_capture(&path).is_err());
        Ok(())
    }

    #[test]
    fn session_records_fake_process_output_and_stops_after_an_operation_fails()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let writer = CaptureWriter::create(directory.path(), "fake-process")?;
        let path = writer.path.clone();
        let session = CaptureSession::new(writer);
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", "printf 'fake output'"])
            .output()?;
        assert!(output.status.success());
        let bytes = output.stdout;
        session.record_and_apply(
            CaptureEvent::Output {
                bytes: bytes.clone(),
            },
            || {
                Ok((
                    (),
                    vec![CaptureEvent::Screen {
                        state: serde_json::json!({"text": "fake output"}),
                    }],
                ))
            },
        )?;
        let failed: HarnessResult<()> = session.record_and_apply(
            CaptureEvent::Input {
                bytes: b"attempt".to_vec(),
            },
            || Err(HarnessError::Protocol("fake write failed".into())),
        );
        assert!(failed.is_err());
        let mut later_operation_ran = false;
        let blocked: HarnessResult<()> = session.record_and_apply(
            CaptureEvent::Input {
                bytes: b"must not run".to_vec(),
            },
            || {
                later_operation_ran = true;
                Ok(((), Vec::new()))
            },
        );
        assert!(blocked.is_err());
        assert!(!later_operation_ran);
        let records = read_capture(&path)?;
        assert_eq!(records.len(), 3);
        assert!(
            matches!(&records[0].event, CaptureEvent::Output { bytes: recorded } if recorded == &bytes)
        );
        assert!(matches!(&records[1].event, CaptureEvent::Screen { .. }));
        assert!(matches!(&records[2].event, CaptureEvent::Input { .. }));
        Ok(())
    }

    #[test]
    fn a_failed_capture_write_permanently_stops_recording() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        let mut writer = CaptureWriter::create(directory.path(), "failed-write")?;
        writer.file = File::open(&writer.path)?;
        let record = CaptureRecord {
            elapsed_micros: 0,
            event: CaptureEvent::Input { bytes: vec![1] },
        };
        assert!(writer.append(&record).is_err());
        writer.file = OpenOptions::new().append(true).open(&writer.path)?;
        assert!(writer.append(&record).is_err());
        assert!(read_capture(&writer.path)?.is_empty());
        Ok(())
    }

    #[test]
    fn capture_round_trips_ordered_terminal_and_effect_records()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let mut writer = CaptureWriter::create(directory.path(), "fixture")?;
        let records = [
            CaptureRecord {
                elapsed_micros: 0,
                event: CaptureEvent::Runtime {
                    launcher_version: Some("0.0.171".into()),
                    runtime_version: Some("0.2.36".into()),
                    target: Some("darwin-arm64".into()),
                    cached_runtime_sha256: None,
                },
            },
            CaptureRecord {
                elapsed_micros: 0,
                event: CaptureEvent::Output {
                    bytes: vec![0, 255, 27],
                },
            },
            CaptureRecord {
                elapsed_micros: 1,
                event: CaptureEvent::Resize {
                    columns: 80,
                    rows: 24,
                },
            },
            CaptureRecord {
                elapsed_micros: 2,
                event: CaptureEvent::Screen {
                    state: serde_json::json!({"cursor": [2, 1]}),
                },
            },
            CaptureRecord {
                elapsed_micros: 3,
                event: CaptureEvent::Response {
                    text: "reply".into(),
                    complete: false,
                },
            },
            CaptureRecord {
                elapsed_micros: 4,
                event: CaptureEvent::WorktreeEffect {
                    path: "file.txt".into(),
                    contents: Some(vec![42]),
                },
            },
        ];
        for record in &records {
            writer.append(record)?;
        }
        let restored = read_capture(
            &directory
                .path()
                .join(".cache/shepherd-live/freebuff/fixture.jsonl"),
        )?;
        assert!(restored == records);
        assert!(CaptureWriter::create(directory.path(), "fixture").is_err());
        Ok(())
    }

    #[test]
    fn rejects_missing_and_unsupported_capture_headers() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("capture.jsonl");
        for contents in [
            "",
            "{}\n",
            "{\"format\":\"shepherd-freebuff\",\"version\":2}\n",
            "{\"format\":\"other\",\"version\":1}\n",
        ] {
            fs::write(&path, contents)?;
            assert!(read_capture(&path).is_err());
        }
        Ok(())
    }

    #[test]
    fn replay_visits_records_in_order_and_stops_on_consumer_error()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let mut writer = CaptureWriter::create(directory.path(), "replay")?;
        for byte in 1..=3 {
            writer.append(&CaptureRecord {
                elapsed_micros: u64::from(byte),
                event: CaptureEvent::Output { bytes: vec![byte] },
            })?;
        }
        let path = directory
            .path()
            .join(".cache/shepherd-live/freebuff/replay.jsonl");
        let mut observed = Vec::new();
        let result = visit_capture(&path, |record| {
            observed.push(record.elapsed_micros);
            if record.elapsed_micros == 2 {
                return Err(HarnessError::Protocol("consumer stopped".into()));
            }
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(observed, [1, 2]);
        Ok(())
    }

    #[test]
    fn rejects_invalid_names_and_out_of_order_records() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        assert!(CaptureWriter::create(directory.path(), "../escape").is_err());
        let mut writer = CaptureWriter::create(directory.path(), "fixture")?;
        writer.append(&CaptureRecord {
            elapsed_micros: 10,
            event: CaptureEvent::Input { bytes: vec![3] },
        })?;
        assert!(
            writer
                .append(&CaptureRecord {
                    elapsed_micros: 9,
                    event: CaptureEvent::Input { bytes: vec![4] }
                })
                .is_err()
        );
        assert!(
            writer
                .append(&CaptureRecord {
                    elapsed_micros: 11,
                    event: CaptureEvent::WorktreeEffect {
                        path: "../outside".into(),
                        contents: None
                    }
                })
                .is_err()
        );
        Ok(())
    }
}
