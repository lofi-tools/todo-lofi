use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
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
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        serde_json::to_writer(&mut file, record)?;
        file.write_all(b"\n")
    }

    fn recent(&self) -> io::Result<Vec<Diagnostic>> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("diagnostic log lock poisoned"))?;
        let file = match fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut records = std::collections::VecDeque::new();
        for line in BufReader::new(file).lines() {
            let line = line?;
            let record = serde_json::from_str(&line).map_err(io::Error::other)?;
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
}
