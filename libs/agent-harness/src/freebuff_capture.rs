use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{HarnessError, HarnessResult};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CaptureEvent {
    Runtime {
        launcher_version: Option<String>,
        runtime_version: Option<String>,
        target: Option<String>,
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
    WorktreeEffect {
        path: PathBuf,
        contents: Option<Vec<u8>>,
    },
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureRecord {
    pub elapsed_micros: u64,
    pub event: CaptureEvent,
}

pub struct CaptureWriter {
    file: File,
    path: PathBuf,
    last_elapsed_micros: Option<u64>,
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
        let file = options.open(&path).map_err(|error| {
            HarnessError::Protocol(format!(
                "could not create Freebuff capture at {}: {error}",
                path.display()
            ))
        })?;
        Ok(Self {
            file,
            path,
            last_elapsed_micros: None,
        })
    }

    pub fn append(&mut self, record: &CaptureRecord) -> HarnessResult<()> {
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
        self.file
            .write_all(&line)
            .and_then(|()| self.file.flush())
            .map_err(|error| {
                HarnessError::Protocol(format!(
                    "could not write Freebuff capture at {}: {error}",
                    self.path.display()
                ))
            })?;
        self.last_elapsed_micros = Some(record.elapsed_micros);
        Ok(())
    }
}

pub fn read_capture(path: &Path) -> HarnessResult<Vec<CaptureRecord>> {
    let file = File::open(path).map_err(|error| {
        HarnessError::Protocol(format!(
            "could not open Freebuff capture at {}: {error}",
            path.display()
        ))
    })?;
    let mut records = Vec::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|error| {
            HarnessError::Protocol(format!(
                "could not read Freebuff capture at {} line {}: {error}",
                path.display(),
                index + 1
            ))
        })?;
        let record: CaptureRecord = serde_json::from_str(&line).map_err(|error| {
            HarnessError::Protocol(format!(
                "invalid Freebuff capture at {} line {}: {error}",
                path.display(),
                index + 1
            ))
        })?;
        validate_record(&record)?;
        if records
            .last()
            .is_some_and(|previous: &CaptureRecord| previous.elapsed_micros > record.elapsed_micros)
        {
            return Err(HarnessError::Protocol(format!(
                "Freebuff capture at {} is out of order",
                path.display()
            )));
        }
        records.push(record);
    }
    Ok(records)
}

fn validate_record(record: &CaptureRecord) -> HarnessResult<()> {
    if let CaptureEvent::WorktreeEffect { path, .. } = &record.event
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
