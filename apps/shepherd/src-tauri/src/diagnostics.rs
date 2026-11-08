use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

const MAX_DIAGNOSTIC_RECORD_BYTES: usize = 256 * 1024;
const MAX_DIAGNOSTIC_HISTORY_RECORDS: usize = 200;

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
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let mut file = options.open(&self.path)?;
        ensure_regular_file(&file)?;
        file.lock()?;
        if file.metadata()?.len() > 0 {
            file.seek(SeekFrom::End(-1))?;
            let mut last = [0];
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                file.write_all(b"\n")?;
            }
        }
        let mut encoded = encode_diagnostic(record)?;
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
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let mut file = match options.open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        ensure_regular_file(&file)?;
        file.lock_shared()?;
        let start = recent_history_start(&mut file)?;
        file.seek(SeekFrom::Start(start))?;
        let mut records = Vec::new();
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        let mut line_number = 0;
        loop {
            line.clear();
            let line_offset = reader.stream_position()?;
            let Some(truncated) = read_diagnostic_line(&mut reader, &mut line)? else {
                break;
            };
            line_number += 1;
            let location = if start == 0 {
                format!("line {line_number}")
            } else {
                format!("byte {line_offset}")
            };
            let record = if truncated {
                Diagnostic {
                    id: format!("invalid-log-byte-{line_offset}"),
                    time: "Unknown time".into(),
                    operation: "diagnostic_history".into(),
                    message: "A diagnostic history record could not be read.".into(),
                    details: format!(
                        "Diagnostic record at {location} exceeds the {MAX_DIAGNOSTIC_RECORD_BYTES}-byte limit."
                    ),
                }
            } else {
                match serde_json::from_slice(&line) {
                    Ok(record) => record,
                    Err(error) => Diagnostic {
                        id: format!("invalid-log-byte-{line_offset}"),
                        time: "Unknown time".into(),
                        operation: "diagnostic_history".into(),
                        message: "A diagnostic history record could not be read.".into(),
                        details: format!("Invalid diagnostic record at {location}: {error}"),
                    },
                }
            };
            records.push(record);
        }
        Ok(records)
    }
}

fn recent_history_start(file: &mut fs::File) -> io::Result<u64> {
    let end = file.metadata()?.len();
    if end == 0 {
        return Ok(0);
    }
    let mut position = end;
    let mut delimiters = 0;
    let mut buffer = [0; 8192];
    while position > 0 {
        let length =
            usize::try_from(position.min(buffer.len() as u64)).map_err(io::Error::other)?;
        position -= length as u64;
        file.seek(SeekFrom::Start(position))?;
        file.read_exact(&mut buffer[..length])?;
        for index in (0..length).rev() {
            let offset = position + index as u64;
            if buffer[index] == b'\n' && offset + 1 < end {
                delimiters += 1;
                if delimiters == MAX_DIAGNOSTIC_HISTORY_RECORDS {
                    return Ok(offset + 1);
                }
            }
        }
    }
    Ok(0)
}

struct LimitedDiagnosticBuffer {
    bytes: Vec<u8>,
    exceeded: bool,
}

impl Write for LimitedDiagnosticBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > (MAX_DIAGNOSTIC_RECORD_BYTES - 1).saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(io::Error::other("diagnostic record exceeds history limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode_diagnostic(record: &Diagnostic) -> io::Result<Vec<u8>> {
    let mut output = LimitedDiagnosticBuffer {
        bytes: Vec::new(),
        exceeded: false,
    };
    match serde_json::to_writer(&mut output, record) {
        Ok(()) => Ok(output.bytes),
        Err(error) if !output.exceeded => Err(io::Error::other(error)),
        Err(_) => {
            let shortened = Diagnostic {
                id: diagnostic_excerpt(&record.id, 1024),
                time: diagnostic_excerpt(&record.time, 1024),
                operation: diagnostic_excerpt(&record.operation, 1024),
                message: diagnostic_excerpt(&record.message, 7 * 1024),
                details: diagnostic_excerpt(&record.details, 30 * 1024),
            };
            let mut output = LimitedDiagnosticBuffer {
                bytes: Vec::new(),
                exceeded: false,
            };
            serde_json::to_writer(&mut output, &shortened).map_err(io::Error::other)?;
            Ok(output.bytes)
        }
    }
}

fn diagnostic_excerpt(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    let mut head = limit / 2;
    while !value.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = value.len() - limit / 2;
    while !value.is_char_boundary(tail) {
        tail += 1;
    }
    format!(
        "{}\n[diagnostic field truncated]\n{}",
        &value[..head],
        &value[tail..]
    )
}

fn read_diagnostic_line(reader: &mut impl BufRead, line: &mut Vec<u8>) -> io::Result<Option<bool>> {
    let mut found_data = false;
    let mut truncated = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(found_data.then_some(truncated));
        }
        found_data = true;
        let end = available.iter().position(|byte| *byte == b'\n');
        let length = end.map_or(available.len(), |index| index + 1);
        let retained = length.min(MAX_DIAGNOSTIC_RECORD_BYTES.saturating_sub(line.len()));
        line.extend_from_slice(&available[..retained]);
        truncated |= retained < length;
        reader.consume(length);
        if end.is_some() {
            return Ok(Some(truncated));
        }
    }
}

fn ensure_regular_file(file: &fs::File) -> io::Result<()> {
    if file.metadata()?.is_file() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "diagnostic log is not a regular file",
        ))
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

    #[cfg(unix)]
    #[test]
    fn refuses_a_fifo_without_waiting_for_a_writer() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::ffi::OsStrExt;
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("errors.jsonl");
        let path_bytes = std::ffi::CString::new(path.as_os_str().as_bytes())?;
        if unsafe { libc::mkfifo(path_bytes.as_ptr(), 0o600) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        let log = DiagnosticLog::new(path);
        let record = Diagnostic {
            id: "fixture".into(),
            time: "fixture".into(),
            operation: "fixture".into(),
            message: "fixture error".into(),
            details: String::new(),
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        let _worker = std::thread::spawn(move || {
            let results = (log.recent().map(|_| ()), log.append(&record));
            if let Err(error) = sender.send(results) {
                eprintln!("FIFO fixture receiver closed: {error}");
            }
        });
        let (read, write) = receiver.recv_timeout(std::time::Duration::from_secs(5))?;
        assert!(matches!(read, Err(error) if error.kind() == io::ErrorKind::InvalidInput));
        assert!(matches!(write, Err(error) if error.kind() == io::ErrorKind::InvalidInput));
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
    fn history_reads_the_latest_records_and_reports_absolute_offsets()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("errors.jsonl");
        let log = DiagnosticLog::new(path.clone());
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        for index in 0..205 {
            writeln!(
                file,
                "{{\"id\":\"{index}\",\"time\":\"t\",\"operation\":\"o\",\"message\":\"m\",\"details\":\"d\"}}"
            )?;
        }
        write!(file, "invalid")?;
        drop(file);
        let history = log.recent()?;
        assert_eq!(history.len(), MAX_DIAGNOSTIC_HISTORY_RECORDS);
        assert_eq!(history[0].id, "6");
        assert_eq!(history[198].id, "204");
        assert!(history[199].details.contains("at byte "));
        Ok(())
    }

    #[test]
    fn oversized_diagnostics_are_saved_as_readable_excerpts()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("errors.jsonl");
        let log = DiagnosticLog::new(path.clone());
        let record = Diagnostic {
            id: "fixture".into(),
            time: "fixture".into(),
            operation: "fixture".into(),
            message: "message".into(),
            details: format!(
                "start{}end",
                "\u{0001}é".repeat(MAX_DIAGNOSTIC_RECORD_BYTES)
            ),
        };
        log.append(&record)?;
        assert!(fs::metadata(path)?.len() <= MAX_DIAGNOSTIC_RECORD_BYTES as u64);
        let history = log.recent()?;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, "fixture");
        assert!(history[0].details.starts_with("start"));
        assert!(history[0].details.contains("[diagnostic field truncated]"));
        assert!(history[0].details.ends_with("end"));
        Ok(())
    }

    #[test]
    fn history_skips_oversized_records_and_preserves_following_entries()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("errors.jsonl");
        let log = DiagnosticLog::new(path.clone());
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        file.write_all(&vec![b'x'; MAX_DIAGNOSTIC_RECORD_BYTES * 3])?;
        file.write_all(b"\n")?;
        drop(file);
        log.append(&Diagnostic {
            id: "after".into(),
            time: "fixture".into(),
            operation: "fixture".into(),
            message: "fixture error".into(),
            details: String::new(),
        })?;
        let history = log.recent()?;
        assert_eq!(history.len(), 2);
        assert!(history[0].details.contains("exceeds"));
        assert_eq!(history[1].id, "after");
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
