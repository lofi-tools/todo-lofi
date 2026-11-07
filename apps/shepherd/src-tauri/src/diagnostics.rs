use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    id: String,
    time: String,
    operation: String,
    message: String,
    details: String,
}

pub struct DiagnosticLog {
    path: PathBuf,
    lock: Mutex<()>,
}

impl DiagnosticLog {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    fn append(&self, record: &Diagnostic) -> io::Result<()> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("diagnostic log lock poisoned"))?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| io::Error::other("missing log directory"))?;
        fs::create_dir_all(parent)?;
        let mut options = OpenOptions::new();
        options.create(true).read(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&self.path)?;
        file.lock()?;
        if file.metadata()?.len() > 0 {
            file.seek(SeekFrom::End(-1))?;
            let mut last = [0];
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                file.write_all(b"\n")?;
            }
        }
        let mut encoded = serde_json::to_vec(record)?;
        encoded.push(b'\n');
        file.write_all(&encoded)?;
        file.unlock()
    }

    fn recent(&self) -> io::Result<Vec<Diagnostic>> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("diagnostic log lock poisoned"))?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = match options.open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        file.lock_shared()?;
        let mut records = std::collections::VecDeque::new();
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        let mut line_number = 0;
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            line_number += 1;
            let record = match serde_json::from_slice(&line) {
                Ok(record) => record,
                Err(error) => Diagnostic {
                    id: format!("invalid-log-line-{line_number}"),
                    time: "Unknown time".into(),
                    operation: "diagnostic_history".into(),
                    message: "A diagnostic history record could not be read.".into(),
                    details: format!("Invalid diagnostic record on line {line_number}: {error}"),
                },
            };
            if records.len() == 200 {
                records.pop_front();
            }
            records.push_back(record);
        }
        Ok(records.into_iter().collect())
    }
}

#[tauri::command]
pub fn record_diagnostic(
    log: tauri::State<'_, DiagnosticLog>,
    record: Diagnostic,
) -> Result<(), String> {
    log.append(&record)
        .map_err(|error| format!("Could not write the diagnostic log: {error}"))
}

#[tauri::command]
pub fn diagnostic_history(log: tauri::State<'_, DiagnosticLog>) -> Result<Vec<Diagnostic>, String> {
    log.recent()
        .map_err(|error| format!("Could not read the diagnostic log: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_persist_across_log_instances() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("logs/errors.jsonl");
        let log = DiagnosticLog::new(path.clone());
        assert!(log.recent()?.is_empty());
        log.append(&Diagnostic {
            id: "fixture".into(),
            time: "2026-01-01T00:00:00Z".into(),
            operation: "list_files".into(),
            message: "Permission denied".into(),
            details: "Permission denied\nCaused by: denied".into(),
        })?;
        let records = DiagnosticLog::new(path).recent()?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, "fixture");
        assert_eq!(records[0].details, "Permission denied\nCaused by: denied");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn newly_created_log_is_private() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("errors.jsonl");
        let log = DiagnosticLog::new(path.clone());
        log.append(&Diagnostic {
            id: "fixture".into(),
            time: "fixture".into(),
            operation: "fixture".into(),
            message: "fixture error".into(),
            details: String::new(),
        })?;
        assert_eq!(std::fs::metadata(path)?.permissions().mode() & 0o077, 0);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_symlinked_log_file() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let target = directory.path().join("other.txt");
        let path = directory.path().join("errors.jsonl");
        fs::write(&target, "unchanged")?;
        std::os::unix::fs::symlink(&target, &path)?;
        let log = DiagnosticLog::new(path);
        let record = Diagnostic {
            id: "fixture".into(),
            time: "fixture".into(),
            operation: "fixture".into(),
            message: "fixture error".into(),
            details: String::new(),
        };
        assert!(log.append(&record).is_err());
        assert!(log.recent().is_err());
        assert_eq!(fs::read_to_string(target)?, "unchanged");
        Ok(())
    }

    #[test]
    fn history_keeps_readable_records_around_a_damaged_line()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("errors.jsonl");
        let log = DiagnosticLog::new(path.clone());
        let record = |id: &str| Diagnostic {
            id: id.into(),
            time: "2026-01-01T00:00:00Z".into(),
            operation: "fixture".into(),
            message: "fixture error".into(),
            details: String::new(),
        };
        log.append(&record("before"))?;
        OpenOptions::new()
            .append(true)
            .open(&path)?
            .write_all(b"{\xff")?;
        log.append(&record("after"))?;
        let history = log.recent()?;
        assert_eq!(history.len(), 3);
        assert_eq!(
            history.first().map(|record| record.id.as_str()),
            Some("before")
        );
        let damaged = history.get(1).ok_or("missing damaged record notice")?;
        assert_eq!(damaged.operation, "diagnostic_history");
        assert!(damaged.details.contains("line 2"));
        assert_eq!(
            history.get(2).map(|record| record.id.as_str()),
            Some("after")
        );
        Ok(())
    }

    #[test]
    fn independent_logs_can_append_concurrently() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("errors.jsonl");
        std::thread::scope(|scope| -> Result<(), Box<dyn std::error::Error>> {
            let mut threads = Vec::new();
            for worker in 0..2 {
                let path = path.clone();
                threads.push(scope.spawn(move || -> io::Result<()> {
                    let log = DiagnosticLog::new(path);
                    for index in 0..25 {
                        log.append(&Diagnostic {
                            id: format!("{worker}-{index}"),
                            time: "fixture".into(),
                            operation: "fixture".into(),
                            message: "fixture error".into(),
                            details: String::new(),
                        })?;
                    }
                    Ok(())
                }));
            }
            for thread in threads {
                thread.join().map_err(|_| "diagnostic writer panicked")??;
            }
            Ok(())
        })?;
        let records = DiagnosticLog::new(path).recent()?;
        assert_eq!(records.len(), 50);
        assert!(records.iter().all(|record| record.operation == "fixture"));
        Ok(())
    }
}
