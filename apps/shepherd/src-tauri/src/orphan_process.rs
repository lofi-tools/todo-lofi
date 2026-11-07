use serde::Serialize;
use shepherd_core::terminal::ProcessRecord;

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProcessStatus {
    Matching,
    Gone,
    Reused,
    Changed,
    Unknown,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessInspection {
    id: String,
    kind: String,
    owner_id: String,
    pid: u32,
    status: ProcessStatus,
}

struct Observation {
    boot_id: Option<String>,
    exists: Option<bool>,
    started_at: Option<u64>,
    process_group: Option<u32>,
    session_id: Option<u32>,
    own_process_group: Option<u32>,
}

fn classify(record: &ProcessRecord, observed: &Observation) -> ProcessStatus {
    let Some(boot_id) = observed.boot_id.as_deref() else {
        return ProcessStatus::Unknown;
    };
    if boot_id != record.boot_id {
        return ProcessStatus::Gone;
    }
    if record.pid <= 1 || record.process_group <= 1 || record.session_id <= 1 {
        return ProcessStatus::Unknown;
    }
    match observed.exists {
        Some(false) => return ProcessStatus::Gone,
        None => return ProcessStatus::Unknown,
        Some(true) => {}
    }
    let Some(started_at) = observed.started_at else {
        return ProcessStatus::Unknown;
    };
    if started_at != record.started_at {
        return ProcessStatus::Reused;
    }
    let (Some(group), Some(session), Some(own_group)) = (
        observed.process_group,
        observed.session_id,
        observed.own_process_group,
    ) else {
        return ProcessStatus::Unknown;
    };
    if group != record.process_group || session != record.session_id || group == own_group {
        return ProcessStatus::Changed;
    }
    ProcessStatus::Matching
}

pub(crate) fn inspect(records: Vec<ProcessRecord>) -> Vec<ProcessInspection> {
    let boot_id = crate::pty_terminal::boot_id();
    records
        .into_iter()
        .map(|record| {
            let observed = observe(&record, boot_id.clone());
            let status = classify(&record, &observed);
            ProcessInspection {
                id: record.id,
                kind: record.kind,
                owner_id: record.owner_id,
                pid: record.pid,
                status,
            }
        })
        .collect()
}

#[cfg(unix)]
fn observe(record: &ProcessRecord, boot_id: Option<String>) -> Observation {
    let pid = libc::pid_t::try_from(record.pid)
        .ok()
        .filter(|pid| *pid > 1);
    let exists = pid.and_then(|pid| {
        if unsafe { libc::kill(pid, 0) } == 0 {
            Some(true)
        } else {
            match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::ESRCH) => Some(false),
                Some(libc::EPERM) => Some(true),
                _ => None,
            }
        }
    });
    let process_group = pid.and_then(|pid| u32::try_from(unsafe { libc::getpgid(pid) }).ok());
    let session_id = pid.and_then(|pid| u32::try_from(unsafe { libc::getsid(pid) }).ok());
    Observation {
        boot_id,
        exists,
        started_at: pid.and_then(crate::pty_terminal::process_start_time),
        process_group,
        session_id,
        own_process_group: u32::try_from(unsafe { libc::getpgrp() }).ok(),
    }
}

#[cfg(not(unix))]
fn observe(_record: &ProcessRecord, boot_id: Option<String>) -> Observation {
    Observation {
        boot_id,
        exists: None,
        started_at: None,
        process_group: None,
        session_id: None,
        own_process_group: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspection_refuses_unverified_and_reused_identities() {
        let record = ProcessRecord {
            id: "terminal".into(),
            kind: "terminal".into(),
            owner_id: "project".into(),
            pid: 123,
            started_at: 456,
            process_group: 123,
            session_id: 123,
            boot_id: "boot".into(),
        };
        let mut observed = Observation {
            boot_id: Some("boot".into()),
            exists: Some(true),
            started_at: Some(456),
            process_group: Some(123),
            session_id: Some(123),
            own_process_group: Some(789),
        };
        assert_eq!(classify(&record, &observed), ProcessStatus::Matching);
        observed.started_at = Some(457);
        assert_eq!(classify(&record, &observed), ProcessStatus::Reused);
        observed.started_at = Some(456);
        observed.process_group = Some(124);
        assert_eq!(classify(&record, &observed), ProcessStatus::Changed);
        observed.process_group = Some(123);
        observed.own_process_group = Some(123);
        assert_eq!(classify(&record, &observed), ProcessStatus::Changed);
        observed.own_process_group = None;
        assert_eq!(classify(&record, &observed), ProcessStatus::Unknown);
        observed.exists = Some(false);
        assert_eq!(classify(&record, &observed), ProcessStatus::Gone);
        observed.boot_id = None;
        assert_eq!(classify(&record, &observed), ProcessStatus::Unknown);
        observed.boot_id = Some("another boot".into());
        assert_eq!(classify(&record, &observed), ProcessStatus::Gone);
    }
}
