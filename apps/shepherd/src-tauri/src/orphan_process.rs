use serde::Serialize;
use shepherd_core::terminal::ProcessRecord;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io;
#[cfg(any(test, not(unix)))]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;

pub(crate) struct ProcessLease(File);

impl ProcessLease {
    #[cfg(any(test, not(unix)))]
    pub(crate) fn create(directory: &Path, id: &str) -> io::Result<Self> {
        Self::open(directory, id, true)
    }

    #[cfg(test)]
    pub(crate) fn recover(directory: &Path, id: &str) -> io::Result<Self> {
        Self::open(directory, id, false)
    }

    #[cfg(any(test, not(unix)))]
    fn open(directory: &Path, id: &str, create: bool) -> io::Result<Self> {
        let id = uuid::Uuid::parse_str(id).map_err(io::Error::other)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(create);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(directory.join(format!("{id}.lock")))?;
        file.try_lock()?;
        Ok(Self(file))
    }

    #[cfg(unix)]
    fn create_at(directory: &File, id: &str) -> io::Result<Self> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        let id = uuid::Uuid::parse_str(id).map_err(io::Error::other)?;
        let name = std::ffi::CString::new(format!("{id}.lock")).map_err(io::Error::other)?;
        let descriptor = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_fd(descriptor) };
        file.try_lock()?;
        Ok(Self(file))
    }

    /// Take over a lease left by an earlier instance. The lock proves that no
    /// live instance still owns the process.
    #[cfg(unix)]
    fn adopt_at(directory: &File, id: &str) -> io::Result<Self> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        use std::os::unix::fs::MetadataExt as _;
        let name = lease_name(id)?;
        let descriptor = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        let lease = Self(unsafe { File::from_raw_fd(descriptor) });
        let metadata = lease.0.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "agent process lease file is unsafe",
            ));
        }
        lease.0.try_lock().map_err(|error| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("a running Shepherd instance owns the agent process lease: {error}"),
            )
        })?;
        lease.verify_entry(directory, id)?;
        Ok(lease)
    }

    /// Confirm that the locked file is still the entry at the lease name, so a
    /// replacement cannot stand in for a live owner's lease.
    #[cfg(unix)]
    fn verify_entry(&self, directory: &File, id: &str) -> io::Result<()> {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::fs::MetadataExt as _;
        let name = lease_name(id)?;
        let mut entry: libc::stat = unsafe { std::mem::zeroed() };
        let checked = unsafe {
            libc::fstatat(
                directory.as_raw_fd(),
                name.as_ptr(),
                &mut entry,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if checked != 0 {
            return Err(io::Error::last_os_error());
        }
        let metadata = self.0.metadata()?;
        if entry.st_dev as u64 != metadata.dev() || entry.st_ino != metadata.ino() {
            return Err(io::Error::other(
                "agent process lease file was replaced while opening",
            ));
        }
        Ok(())
    }
}

#[cfg(unix)]
fn lease_name(id: &str) -> io::Result<std::ffi::CString> {
    let id = uuid::Uuid::parse_str(id).map_err(io::Error::other)?;
    std::ffi::CString::new(format!("{id}.lock")).map_err(io::Error::other)
}

pub(crate) struct ProcessLeases {
    directory: PathBuf,
    #[cfg(unix)]
    directory_handle: File,
    active: Mutex<BTreeMap<String, ProcessLease>>,
}

impl ProcessLeases {
    pub(crate) fn new(directory: PathBuf) -> io::Result<Self> {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        builder.create(&directory)?;
        let metadata = std::fs::symlink_metadata(&directory)?;
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "agent process lease path is not a directory",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
            if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "agent process lease directory must be owned by the current user and not writable by others",
                ));
            }
            let directory_handle = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&directory)?;
            let opened = directory_handle.metadata()?;
            if opened.dev() != metadata.dev()
                || opened.ino() != metadata.ino()
                || opened.uid() != unsafe { libc::geteuid() }
                || opened.mode() & 0o022 != 0
            {
                return Err(io::Error::other(
                    "agent process lease directory changed while opening",
                ));
            }
            Ok(Self {
                directory,
                directory_handle,
                active: Mutex::new(BTreeMap::new()),
            })
        }
        #[cfg(not(unix))]
        Ok(Self {
            directory,
            active: Mutex::new(BTreeMap::new()),
        })
    }

    pub(crate) fn claim(&self, id: &str) -> io::Result<()> {
        #[cfg(unix)]
        let lease = {
            self.verify_directory()?;
            let lease = ProcessLease::create_at(&self.directory_handle, id)?;
            self.verify_directory()?;
            lease
        };
        #[cfg(not(unix))]
        let lease = ProcessLease::create(&self.directory, id)?;
        let mut active = self
            .active
            .lock()
            .map_err(|error| io::Error::other(error.to_string()))?;
        if active.contains_key(id) {
            return Err(io::Error::other("agent process lease already exists"));
        }
        active.insert(id.to_owned(), lease);
        Ok(())
    }

    /// Take the retained lease for a recorded process. The returned lease is
    /// held while that process is stopped so another instance cannot adopt it.
    #[cfg(unix)]
    pub(crate) fn adopt(&self, id: &str) -> io::Result<ProcessLease> {
        self.verify_directory()?;
        let lease = ProcessLease::adopt_at(&self.directory_handle, id)?;
        self.verify_directory()?;
        Ok(lease)
    }

    #[cfg(unix)]
    fn verify_directory(&self) -> io::Result<()> {
        use std::os::unix::fs::MetadataExt as _;
        let path = std::fs::symlink_metadata(&self.directory)?;
        let opened = self.directory_handle.metadata()?;
        if !path.is_dir()
            || path.dev() != opened.dev()
            || path.ino() != opened.ino()
            || opened.uid() != unsafe { libc::geteuid() }
            || opened.mode() & 0o022 != 0
        {
            return Err(io::Error::other(
                "agent process lease directory was replaced or is unsafe",
            ));
        }
        Ok(())
    }

    pub(crate) fn release(&self, id: &str) -> io::Result<()> {
        let mut active = self
            .active
            .lock()
            .map_err(|error| io::Error::other(error.to_string()))?;
        if active.remove(id).is_none() {
            tracing::debug!("agent process lease was already released");
        }
        Ok(())
    }
}

impl Drop for ProcessLease {
    fn drop(&mut self) {
        if let Err(error) = self.0.unlock() {
            tracing::warn!(%error, "could not release agent process lease");
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProcessStatus {
    Matching,
    Gone,
    Reused,
    Changed,
    Unknown,
    GroupUnverified,
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
            #[cfg(unix)]
            let status = if record.kind == "agent" && status == ProcessStatus::Gone {
                match crate::pty_terminal::process_group_has_other_members(&record) {
                    Ok(true) => ProcessStatus::GroupUnverified,
                    Ok(false) => status,
                    Err(error) => {
                        tracing::warn!(%error, "could not inspect a previous agent process group");
                        ProcessStatus::Unknown
                    }
                }
            } else {
                status
            };
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

/// Stop a process recorded by an earlier instance.
///
/// The retained lease proves that no running instance still owns the process.
/// While the leader still matches, its recorded session and group identify its
/// members. Once the leader has exited, membership is proven by the launch
/// token in each member's environment instead, so a reused PID or another
/// launch is never signalled; without a token, or when the environment cannot
/// be read, the stop is refused.
#[cfg(unix)]
pub(crate) fn stop(leases: &ProcessLeases, record: &ProcessRecord) -> io::Result<()> {
    let _lease = leases.adopt(&record.id)?;
    _lease.verify_entry(&leases.directory_handle, &record.id)?;
    let status = classify(record, &observe(record, crate::pty_terminal::boot_id()));
    match status {
        ProcessStatus::Matching => stop_members(record, None),
        ProcessStatus::Gone | ProcessStatus::GroupUnverified => {
            stop_members(record, Some(record.token.as_str()))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the recorded process identity no longer matches a live process",
        )),
    }
}

#[cfg(unix)]
fn stop_members(record: &ProcessRecord, token: Option<&str>) -> io::Result<()> {
    let mut members = verified_members(record, token)?;
    if members.is_empty() {
        return match token {
            Some(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "no surviving member carried the recorded launch token",
            )),
            None => Ok(()),
        };
    }
    signal_members(&members, libc::SIGTERM)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(10));
        members = verified_members(record, token)?;
        if members.is_empty() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
    }
    signal_members(&members, libc::SIGKILL)?;
    Ok(())
}

/// Session members that can be attributed to this launch. A token filter only
/// keeps processes whose environment still carries it, and refuses the whole
/// stop when any member's environment cannot be read.
#[cfg(unix)]
fn verified_members(record: &ProcessRecord, token: Option<&str>) -> io::Result<Vec<libc::pid_t>> {
    let members = crate::pty_terminal::session_members(record)?;
    let Some(token) = token else {
        return Ok(members);
    };
    if token.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the recorded process has no launch token, so its members cannot be verified",
        ));
    }
    let needle = format!("{}={token}", agent_harness::PROCESS_TOKEN_ENV).into_bytes();
    let mut verified = Vec::new();
    for pid in members {
        if crate::pty_terminal::process_environment_contains(pid, &needle)? {
            verified.push(pid);
        }
    }
    Ok(verified)
}

#[cfg(not(unix))]
pub(crate) fn stop(_leases: &ProcessLeases, _record: &ProcessRecord) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "stopping recorded processes is unavailable on this platform",
    ))
}

// macOS refuses `killpg` for a group in another session, so signal each
// recorded session member instead.
#[cfg(unix)]
fn signal_members(members: &[libc::pid_t], signal: libc::c_int) -> io::Result<()> {
    let mut failure = None;
    for pid in members {
        if unsafe { libc::kill(*pid, signal) } == 0 {
            continue;
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) && failure.is_none() {
            failure = Some(error);
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
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

    #[cfg(unix)]
    #[test]
    fn lease_claim_rejects_replaced_or_repermissioned_directory()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir()?;
        let directory = root.path().join("leases");
        let leases = ProcessLeases::new(directory.clone())?;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o777))?;
        assert!(leases.claim(&uuid::Uuid::new_v4().to_string()).is_err());
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        std::fs::rename(&directory, root.path().join("old-leases"))?;
        std::fs::create_dir(&directory)?;
        let id = uuid::Uuid::new_v4().to_string();
        assert!(leases.claim(&id).is_err());
        assert!(!directory.join(format!("{id}.lock")).exists());
        let anchored = ProcessLease::create_at(&leases.directory_handle, &id)?;
        assert!(
            root.path()
                .join("old-leases")
                .join(format!("{id}.lock"))
                .exists()
        );
        assert!(!directory.join(format!("{id}.lock")).exists());
        drop(anchored);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn lease_directory_rejects_symlinks_and_other_user_write_access()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
        let root = tempfile::tempdir()?;
        let directory = root.path().join("leases");
        let leases = ProcessLeases::new(directory.clone())?;
        assert_eq!(std::fs::metadata(&directory)?.mode() & 0o077, 0);
        drop(leases);
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755))?;
        drop(ProcessLeases::new(directory.clone())?);
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o777))?;
        let error = ProcessLeases::new(directory.clone())
            .err()
            .ok_or("unsafe directory accepted")?;
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        let link = root.path().join("lease-link");
        symlink(&directory, &link)?;
        assert!(ProcessLeases::new(link).is_err());
        Ok(())
    }

    #[test]
    fn process_lease_blocks_other_owners_and_requires_existing_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        if let Some(directory) = std::env::var_os("SHEPHERD_LEASE_TEST_DIRECTORY") {
            let id = std::env::var("SHEPHERD_LEASE_TEST_ID")?;
            assert!(ProcessLease::recover(Path::new(&directory), &id).is_err());
            return Ok(());
        }
        let directory = tempfile::tempdir()?;
        let id = uuid::Uuid::new_v4().to_string();
        assert_eq!(
            ProcessLease::recover(directory.path(), &id)
                .err()
                .ok_or("missing error")?
                .kind(),
            io::ErrorKind::NotFound
        );
        let lease = ProcessLease::create(directory.path(), &id)?;
        assert!(ProcessLease::recover(directory.path(), &id).is_err());
        let status = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "orphan_process::tests::process_lease_blocks_other_owners_and_requires_existing_identity"])
            .env("SHEPHERD_LEASE_TEST_DIRECTORY", directory.path())
            .env("SHEPHERD_LEASE_TEST_ID", &id)
            .status()?;
        assert!(status.success());
        drop(lease);
        let recovered = ProcessLease::recover(directory.path(), &id)?;
        assert!(ProcessLease::recover(directory.path(), &id).is_err());
        drop(recovered);
        assert!(ProcessLease::recover(directory.path(), "../other").is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn lease_adoption_requires_an_unowned_lease_file() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir()?;
        let directory = root.path().join("leases");
        let leases = ProcessLeases::new(directory.clone())?;
        let id = uuid::Uuid::new_v4().to_string();
        let path = directory.join(format!("{id}.lock"));
        let lease = ProcessLease::create(&directory, &id)?;
        let error = leases.adopt(&id).err().ok_or("adopted an owned lease")?;
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        drop(lease);
        let adopted = leases.adopt(&id)?;
        assert!(leases.adopt(&id).is_err());
        std::fs::rename(&path, root.path().join("moved.lock"))?;
        std::fs::write(&path, "")?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        assert!(adopted.verify_entry(&leases.directory_handle, &id).is_err());
        drop(adopted);
        drop(leases.adopt(&id)?);
        std::fs::remove_file(&path)?;
        std::os::unix::fs::symlink(root.path().join("moved.lock"), &path)?;
        assert!(leases.adopt(&id).is_err());
        std::fs::remove_file(&path)?;
        std::fs::write(&path, "")?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666))?;
        let error = leases.adopt(&id).err().ok_or("adopted a shared lease")?;
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn stop_signals_only_a_verified_unowned_process_group() -> Result<(), Box<dyn std::error::Error>>
    {
        use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
        if crate::pty_terminal::boot_id().is_none() {
            eprintln!("Skipping process stop check: the OS boot identifier is unavailable");
            return Ok(());
        }
        let root = tempfile::tempdir()?;
        let directory = root.path().join("leases");
        let leases = ProcessLeases::new(directory.clone())?;
        let spawn =
            || -> Result<(std::process::Child, ProcessRecord), Box<dyn std::error::Error>> {
                let mut command = std::process::Command::new("/bin/sh");
                command
                    .args(["-c", "exec sleep 30"])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                unsafe {
                    command.pre_exec(|| {
                        if libc::setsid() < 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
                let child = command.spawn()?;
                let pid = libc::pid_t::try_from(child.id())?;
                let record = crate::pty_terminal::capture_process_record(
                    pid,
                    "agent",
                    "fixture",
                    "fixture-token",
                )
                .ok_or("could not capture the fixture process")?;
                Ok((child, record))
            };

        let (mut child, record) = spawn()?;
        let lease = ProcessLease::create(&directory, &record.id)?;
        let error = stop(&leases, &record)
            .err()
            .ok_or("stopped an owned process")?;
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(
            unsafe { libc::kill(libc::pid_t::try_from(record.pid)?, 0) },
            0
        );
        drop(lease);
        stop(&leases, &record)?;
        assert!(child.wait()?.signal().is_some());

        let (mut child, mut reused) = spawn()?;
        drop(ProcessLease::create(&directory, &reused.id)?);
        reused.started_at += 1;
        let error = stop(&leases, &reused)
            .err()
            .ok_or("stopped a reused identity")?;
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            unsafe { libc::kill(libc::pid_t::try_from(reused.pid)?, 0) },
            0
        );
        unsafe { libc::killpg(libc::pid_t::try_from(reused.process_group)?, libc::SIGKILL) };
        child.wait()?;
        Ok(())
    }

    #[cfg(unix)]
    fn alive(pid: libc::pid_t) -> bool {
        (unsafe { libc::kill(pid, 0) }) == 0
    }

    #[cfg(unix)]
    fn wait_for_member_pids(
        path: &Path,
        count: usize,
    ) -> Result<Vec<libc::pid_t>, Box<dyn std::error::Error>> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Ok(contents) = std::fs::read_to_string(path) {
                let pids: Vec<libc::pid_t> = contents
                    .lines()
                    .filter_map(|line| line.trim().parse().ok())
                    .collect();
                if pids.len() >= count {
                    return Ok(pids);
                }
            }
            if std::time::Instant::now() >= deadline {
                return Err("the fixture did not record its members".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// A leader that exits leaves its children running in the recorded session.
    /// The launch token decides which of them belong to this launch.
    #[cfg(unix)]
    #[test]
    fn stop_signals_only_tokened_members_of_a_leaderless_group()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::process::CommandExt as _;
        if crate::pty_terminal::boot_id().is_none() {
            eprintln!("Skipping process stop check: the OS boot identifier is unavailable");
            return Ok(());
        }
        let root = tempfile::tempdir()?;
        let directory = root.path().join("leases");
        let leases = ProcessLeases::new(directory.clone())?;
        let members_file = root.path().join("members");
        let token = "launch-token-fixture";
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args([
                "-c",
                "sleep 30 & echo $! >> \"$1\"; SHEPHERD_PROCESS_TOKEN=other-launch sleep 30 & echo $! >> \"$1\"; sleep 0.5; exit 3",
                "fixture",
            ])
            .arg(&members_file)
            .env(agent_harness::PROCESS_TOKEN_ENV, token)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut leader = command.spawn()?;
        let leader_pid = libc::pid_t::try_from(leader.id())?;
        let record =
            crate::pty_terminal::capture_process_record(leader_pid, "agent", "fixture", token)
                .ok_or("could not capture the fixture process")?;
        let members = wait_for_member_pids(&members_file, 2)?;
        leader.wait()?;
        assert!(!alive(leader_pid));
        let tokened = members[0];
        let other = members[1];
        assert!(alive(tokened) && alive(other));
        let mut unrelated = std::process::Command::new("sleep")
            .arg("30")
            .env(agent_harness::PROCESS_TOKEN_ENV, token)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let unrelated_pid = libc::pid_t::try_from(unrelated.id())?;

        drop(ProcessLease::create(&directory, &record.id)?);
        stop(&leases, &record)?;

        assert!(!alive(tokened), "a member of this launch should be stopped");
        assert!(alive(other), "a member of another launch must survive");
        assert!(alive(unrelated_pid), "an unrelated process must survive");

        unsafe {
            libc::kill(other, libc::SIGKILL);
            libc::kill(unrelated_pid, libc::SIGKILL);
        }
        unrelated.wait()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn stop_refuses_a_leaderless_group_without_a_launch_token()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::process::CommandExt as _;
        if crate::pty_terminal::boot_id().is_none() {
            eprintln!("Skipping process stop check: the OS boot identifier is unavailable");
            return Ok(());
        }
        let root = tempfile::tempdir()?;
        let directory = root.path().join("leases");
        let leases = ProcessLeases::new(directory.clone())?;
        let member_file = root.path().join("member");
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args([
                "-c",
                "sleep 30 & echo $! > \"$1\"; sleep 0.4; exit 0",
                "fixture",
            ])
            .arg(&member_file)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut leader = command.spawn()?;
        let leader_pid = libc::pid_t::try_from(leader.id())?;
        let record =
            crate::pty_terminal::capture_process_record(leader_pid, "agent", "fixture", "")
                .ok_or("could not capture the fixture process")?;
        let members = wait_for_member_pids(&member_file, 1)?;
        leader.wait()?;
        let member = members[0];
        assert!(alive(member));

        drop(ProcessLease::create(&directory, &record.id)?);
        let error = stop(&leases, &record)
            .err()
            .ok_or("stopped a process group without a launch token")?;
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(alive(member), "an unverified member must survive a refusal");

        unsafe { libc::kill(member, libc::SIGKILL) };
        Ok(())
    }

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
            token: String::new(),
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
