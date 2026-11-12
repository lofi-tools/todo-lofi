//! The `portable-pty` implementation of the core's terminal boundary.
//!
//! One reader thread and one waiter thread per PTY; both report through the same
//! channel, so the shell sees output and the exit in order.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_harness::freebuff_capture::{CaptureEvent, CaptureSession};
use agent_harness::{HarnessError, HarnessResult};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use shepherd_core::terminal::{
    ProcessRecord, TerminalError, TerminalEvent, TerminalId, TerminalService, TerminalSpec,
    TerminalWrite, validate_size,
};
use tokio::sync::{mpsc, oneshot};

use terminal_renderer::{RecordedScreenUpdate, Screen};

const MAX_WRITE_BYTES: usize = 64 * 1024;

#[cfg(unix)]
fn recording_poll_handles(
    master: &dyn MasterPty,
) -> Result<(std::os::fd::OwnedFd, std::os::fd::OwnedFd), TerminalError> {
    use std::os::fd::FromRawFd;
    let fd = master.as_raw_fd().ok_or_else(|| {
        TerminalError::Backend("recorded terminal has no pollable descriptor".into())
    })?;
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(TerminalError::Backend(format!(
            "could not set recorded terminal nonblocking: {}",
            std::io::Error::last_os_error()
        )));
    }
    let duplicate = || {
        let duplicated = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
        if duplicated < 0 {
            Err(TerminalError::Backend(format!(
                "could not duplicate terminal descriptor: {}",
                std::io::Error::last_os_error()
            )))
        } else {
            // The successful duplication returns a new owned descriptor.
            Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(duplicated) })
        }
    };
    Ok((duplicate()?, duplicate()?))
}

#[cfg(unix)]
fn wait_recorded_pty(fd: &std::os::fd::OwnedFd, events: libc::c_short) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let mut descriptor = libc::pollfd {
        fd: fd.as_raw_fd(),
        events,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut descriptor, 1, 100) };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn write_recorded_pty(
    writer: &mut dyn Write,
    fd: &std::os::fd::OwnedFd,
    recording: &TerminalRecording,
    bytes: &[u8],
) -> HarnessResult<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        let written =
            recording
                .capture
                .try_apply_and_record(|| match writer.write(&bytes[offset..]) {
                    Ok(0) => Err(HarnessError::Protocol(
                        "terminal write made no progress".into(),
                    )),
                    Ok(count) => Ok(Some((
                        count,
                        CaptureEvent::Input {
                            bytes: bytes[offset..offset + count].to_vec(),
                        },
                    ))),
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) =>
                    {
                        Ok(None)
                    }
                    Err(error) => Err(HarnessError::Protocol(format!(
                        "terminal write failed: {error}"
                    ))),
                })?;
        if let Some(written) = written {
            offset += written;
        } else {
            wait_recorded_pty(fd, libc::POLLOUT).map_err(|error| {
                HarnessError::Protocol(format!("terminal write polling failed: {error}"))
            })?;
        }
    }
    Ok(())
}

struct WriteRequest {
    bytes: Vec<u8>,
    completion: Option<oneshot::Sender<Result<(), TerminalError>>>,
}

struct Session {
    writer: mpsc::Sender<WriteRequest>,
    closing: bool,
    /// The waiting thread owns the child, so closing goes through a cloned killer.
    killer: Box<dyn ChildKiller + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
    #[cfg(unix)]
    session_id: Option<libc::pid_t>,
    process: Option<ProcessRecord>,
    recording: Option<Arc<TerminalRecording>>,
}

pub struct TerminalRecording {
    capture: Arc<CaptureSession>,
    screen: Mutex<Screen>,
    errors: mpsc::UnboundedSender<String>,
    failure_signal: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    screen_observer: Option<mpsc::Sender<RecordedScreenUpdate>>,
    #[allow(dead_code)]
    initial_size: (u16, u16),
}

impl TerminalRecording {
    #[allow(dead_code)]
    pub fn new(
        capture: CaptureSession,
        columns: u16,
        rows: u16,
        errors: mpsc::UnboundedSender<String>,
    ) -> Result<Self, TerminalError> {
        Self::from_shared(Arc::new(capture), columns, rows, errors)
    }

    #[allow(dead_code)]
    pub fn from_shared(
        capture: Arc<CaptureSession>,
        columns: u16,
        rows: u16,
        errors: mpsc::UnboundedSender<String>,
    ) -> Result<Self, TerminalError> {
        validate_size(columns, rows)?;
        let screen = Screen::with_scrollback(usize::from(columns), usize::from(rows), 0);
        let recording = Self {
            capture,
            screen: Mutex::new(screen),
            errors,
            failure_signal: Mutex::new(None),
            screen_observer: None,
            initial_size: (columns, rows),
        };
        recording
            .capture
            .record_and_apply(CaptureEvent::Resize { columns, rows }, || {
                Ok(((), Vec::new()))
            })
            .map_err(|error| TerminalError::Backend(error.to_string()))?;
        Ok(recording)
    }

    #[allow(dead_code)]
    pub fn with_screen_observer(mut self, observer: mpsc::Sender<RecordedScreenUpdate>) -> Self {
        self.screen_observer = Some(observer);
        self
    }

    fn screen_state(&self, screen: &Screen) -> HarnessResult<serde_json::Value> {
        serde_json::to_value(screen.frame()).map_err(|error| {
            HarnessError::Protocol(format!(
                "could not encode terminal recording screen: {error}"
            ))
        })
    }

    fn publish_screen(&self, update: RecordedScreenUpdate) -> HarnessResult<()> {
        if let Some(observer) = &self.screen_observer {
            observer.try_send(update).map_err(|error| {
                HarnessError::Protocol(format!("terminal screen observer unavailable: {error}"))
            })?;
        }
        Ok(())
    }

    fn output(&self, bytes: &[u8]) -> HarnessResult<()> {
        self.capture
            .record_apply_and_publish(
                CaptureEvent::Output {
                    bytes: bytes.to_vec(),
                },
                || {
                    let mut screen = self.screen.lock().map_err(|_| {
                        HarnessError::Protocol("terminal recording screen lock is poisoned".into())
                    })?;
                    screen.process(bytes);
                    let state = self.screen_state(&screen)?;
                    Ok((state.clone(), vec![CaptureEvent::Screen { state }]))
                },
                |state| {
                    self.publish_screen(RecordedScreenUpdate::Output {
                        bytes: bytes.to_vec(),
                        state: state.clone(),
                    })
                },
            )
            .map(|_| ())
    }

    fn report_error(&self, error: &impl std::fmt::Display) {
        match self.failure_signal.lock() {
            Ok(signal) => {
                if let Some(signal) = signal.as_ref()
                    && signal.send(()).is_err()
                {
                    tracing::error!(%error, "terminal recording failure monitor is unavailable");
                }
            }
            Err(failure) => {
                tracing::error!(%failure, "terminal recording failure signal is poisoned")
            }
        }
        if self.errors.send(error.to_string()).is_err() {
            tracing::error!(%error, "terminal recording failed after its error receiver closed");
        }
    }

    fn invalidate(&self, reason: &str) {
        if let Err(error) = self.capture.fail() {
            self.report_error(&error);
        }
        self.report_error(&reason);
    }
}

#[cfg(unix)]
struct TerminalProcessGroup {
    group: libc::pid_t,
    session: libc::pid_t,
    members: Vec<ProcessIdentity>,
}

#[cfg(unix)]
impl TerminalProcessGroup {
    fn for_session(
        session: libc::pid_t,
        processes: impl IntoIterator<Item = libc::pid_t>,
    ) -> Vec<Self> {
        let own_group = unsafe { libc::getpgrp() };
        let mut groups = BTreeMap::<libc::pid_t, Vec<ProcessIdentity>>::new();
        for process in processes {
            if process <= 1 || unsafe { libc::getsid(process) } != session {
                continue;
            }
            let group = unsafe { libc::getpgid(process) };
            if group > 1
                && group != own_group
                && let Some(identity) = ProcessIdentity::new(process)
            {
                groups.entry(group).or_default().push(identity);
            }
        }
        groups
            .into_iter()
            .map(|(group, members)| Self {
                group,
                session,
                members,
            })
            .collect()
    }

    fn still_owned(&self) -> bool {
        self.group > 1
            && self.group != unsafe { libc::getpgrp() }
            && self.members.iter().any(|process| {
                process.is_in_session(self.session)
                    && unsafe { libc::getpgid(process.pid) == self.group }
            })
    }

    fn signal(&self, signal: libc::c_int) -> std::io::Result<bool> {
        if !self.still_owned() {
            return Ok(false);
        }
        if unsafe { libc::killpg(self.group, signal) } == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(false)
        } else {
            Err(error)
        }
    }

    fn finish_hangup(
        mut groups: Vec<Self>,
        signal_group: &mut impl FnMut(&Self, libc::c_int) -> std::io::Result<bool>,
    ) -> std::io::Result<()> {
        let deadline = std::time::Instant::now() + Duration::from_millis(200);
        groups.retain(Self::still_owned);
        while !groups.is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            groups.retain(Self::still_owned);
        }
        for group in groups {
            signal_group(&group, libc::SIGKILL)?;
        }
        Ok(())
    }
}

#[cfg(unix)]
struct ProcessIdentity {
    pid: libc::pid_t,
    started_at: u64,
}

#[cfg(unix)]
impl ProcessIdentity {
    fn new(pid: libc::pid_t) -> Option<Self> {
        Some(Self {
            pid,
            started_at: process_start_time(pid)?,
        })
    }

    fn is_in_session(&self, session: libc::pid_t) -> bool {
        (unsafe { libc::getsid(self.pid) == session })
            && process_start_time(self.pid) == Some(self.started_at)
    }
}

#[cfg(unix)]
pub(crate) fn capture_process_record(
    pid: libc::pid_t,
    kind: &str,
    owner_id: &str,
    token: &str,
) -> Option<ProcessRecord> {
    if pid <= 1 {
        return None;
    }
    let identity = ProcessIdentity::new(pid)?;
    let group = unsafe { libc::getpgid(pid) };
    let session = unsafe { libc::getsid(pid) };
    if group <= 1 || session <= 1 {
        return None;
    }
    let boot_id = boot_id()?;
    if !identity.is_in_session(session) || unsafe { libc::getpgid(pid) } != group {
        return None;
    }
    Some(ProcessRecord {
        id: uuid::Uuid::new_v4().to_string(),
        kind: kind.to_owned(),
        owner_id: owner_id.to_owned(),
        pid: u32::try_from(pid).ok()?,
        started_at: identity.started_at,
        process_group: u32::try_from(group).ok()?,
        session_id: u32::try_from(session).ok()?,
        boot_id,
        token: token.to_owned(),
    })
}

/// Whether a same-user process still carries `needle` in its environment.
///
/// macOS only exposes another process's environment through `KERN_PROCARGS2`,
/// and `ps -E` prints nothing for the same process, so the launch token can be
/// read this way but not through `ps`. A failing read is an error rather than a
/// negative answer, because the caller must refuse to signal an unverified PID.
#[cfg(unix)]
pub(crate) fn process_environment_contains(
    pid: libc::pid_t,
    needle: &[u8],
) -> std::io::Result<bool> {
    const MAX_ENVIRONMENT_BYTES: usize = 4 * 1024 * 1024;
    let environment = read_process_environment(pid, MAX_ENVIRONMENT_BYTES)?;
    Ok(environment
        .split(|byte| *byte == 0)
        .any(|entry| entry == needle))
}

#[cfg(target_os = "macos")]
fn read_process_environment(pid: libc::pid_t, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    if size == 0 {
        return Err(std::io::Error::other("process environment is empty"));
    }
    if size > limit {
        return Err(std::io::Error::other("process environment is too large"));
    }
    let mut buffer = vec![0u8; size];
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    buffer.truncate(size.min(limit));
    Ok(buffer)
}

#[cfg(target_os = "linux")]
fn read_process_environment(pid: libc::pid_t, limit: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let file = std::fs::File::open(format!("/proc/{pid}/environ"))?;
    let mut buffer = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut buffer)?;
    if buffer.len() > limit {
        return Err(std::io::Error::other("process environment is too large"));
    }
    Ok(buffer)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn read_process_environment(_pid: libc::pid_t, _limit: usize) -> std::io::Result<Vec<u8>> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "reading another process's environment is unavailable on this platform",
    ))
}

#[cfg(target_os = "macos")]
pub(crate) fn process_start_time(pid: libc::pid_t) -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read != size {
        return None;
    }
    let info = unsafe { info.assume_init() };
    Some(info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
}

#[cfg(target_os = "linux")]
pub(crate) fn process_start_time(pid: libc::pid_t) -> Option<u64> {
    let stat = std::fs::read(format!("/proc/{pid}/stat")).ok()?;
    // The command name can contain spaces or parentheses. Fields after it start at 3.
    let fields = stat.rsplit(|&byte| byte == b')').next()?;
    std::str::from_utf8(fields)
        .ok()?
        .split_ascii_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
pub(crate) fn process_start_time(_: libc::pid_t) -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
pub(crate) fn boot_id() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(target_os = "macos")]
pub(crate) fn boot_id() -> Option<String> {
    let mut value = std::mem::MaybeUninit::<libc::timeval>::uninit();
    let mut size = std::mem::size_of::<libc::timeval>();
    let result = unsafe {
        libc::sysctlbyname(
            c"kern.boottime".as_ptr(),
            value.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || size != std::mem::size_of::<libc::timeval>() {
        return None;
    }
    let value = unsafe { value.assume_init() };
    Some(format!("{}:{}", value.tv_sec, value.tv_usec))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn boot_id() -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
fn list_processes() -> std::io::Result<Vec<libc::pid_t>> {
    let estimate = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if estimate <= 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut capacity = estimate as usize + 64;
    loop {
        let bytes = libc::c_int::try_from(capacity * std::mem::size_of::<libc::pid_t>())
            .map_err(std::io::Error::other)?;
        let mut processes = vec![0; capacity];
        let count = unsafe { libc::proc_listallpids(processes.as_mut_ptr().cast(), bytes) };
        if count <= 0 {
            return Err(std::io::Error::last_os_error());
        }
        if (count as usize) < capacity {
            processes.truncate(count as usize);
            return Ok(processes);
        }
        capacity *= 2;
    }
}

#[cfg(target_os = "linux")]
fn list_processes() -> std::io::Result<Vec<libc::pid_t>> {
    let mut processes = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        if let Some(process) = entry?
            .file_name()
            .to_str()
            .and_then(|name| name.parse().ok())
        {
            processes.push(process);
        }
    }
    Ok(processes)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn list_processes() -> std::io::Result<Vec<libc::pid_t>> {
    Ok(Vec::new())
}

#[cfg(unix)]
pub(crate) fn session_members(record: &ProcessRecord) -> std::io::Result<Vec<libc::pid_t>> {
    let current_boot =
        boot_id().ok_or_else(|| std::io::Error::other("could not identify the current boot"))?;
    if current_boot != record.boot_id {
        return Ok(Vec::new());
    }
    let group = libc::pid_t::try_from(record.process_group).map_err(std::io::Error::other)?;
    let session = libc::pid_t::try_from(record.session_id).map_err(std::io::Error::other)?;
    let own_group = unsafe { libc::getpgrp() };
    let own_pid = unsafe { libc::getpid() };
    if group <= 1 || session <= 1 || group == own_group {
        return Err(std::io::Error::other("invalid recorded process identity"));
    }
    let mut members = Vec::new();
    for pid in list_processes()? {
        if pid <= 1 || pid == own_pid || unsafe { libc::getsid(pid) } != session {
            continue;
        }
        if unsafe { libc::getpgid(pid) } == group {
            members.push(pid);
        }
    }
    Ok(members)
}

#[cfg(unix)]
pub(crate) fn process_group_has_other_members(record: &ProcessRecord) -> std::io::Result<bool> {
    let current_boot =
        boot_id().ok_or_else(|| std::io::Error::other("could not identify the current boot"))?;
    if current_boot != record.boot_id {
        return Ok(false);
    }
    let group = libc::pid_t::try_from(record.process_group).map_err(std::io::Error::other)?;
    let session = libc::pid_t::try_from(record.session_id).map_err(std::io::Error::other)?;
    let leader = libc::pid_t::try_from(record.pid).map_err(std::io::Error::other)?;
    if group <= 1 || session <= 1 || leader <= 1 {
        return Err(std::io::Error::other("invalid recorded process identity"));
    }
    for pid in list_processes()? {
        if pid <= 1 || pid == leader {
            continue;
        }
        let observed_group = unsafe { libc::getpgid(pid) };
        if observed_group < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                continue;
            }
            return Err(error);
        }
        if observed_group != group {
            continue;
        }
        let observed_session = unsafe { libc::getsid(pid) };
        if observed_session < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                continue;
            }
            return Err(error);
        }
        if observed_session == session {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(unix)]
fn terminal_processes() -> Vec<libc::pid_t> {
    match list_processes() {
        Ok(processes) => processes,
        Err(error) => {
            tracing::warn!(%error, "could not enumerate background terminal jobs");
            Vec::new()
        }
    }
}

impl Session {
    #[cfg(unix)]
    fn process_groups(&self, processes: &[libc::pid_t]) -> Vec<TerminalProcessGroup> {
        let Some(session) = self.session_id else {
            return Vec::new();
        };
        // The shell and foreground leader are still available if the process
        // table could not be read. Other members can outlive their group leader.
        let fallback = [Some(session), self.master.process_group_leader()];
        TerminalProcessGroup::for_session(
            session,
            processes
                .iter()
                .copied()
                .chain(fallback.into_iter().flatten()),
        )
    }

    fn hangup(
        &mut self,
        #[cfg(unix)] groups: &[TerminalProcessGroup],
        #[cfg(unix)] signal_group: &mut impl FnMut(
            &TerminalProcessGroup,
            libc::c_int,
        ) -> std::io::Result<bool>,
    ) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            let mut shell_signalled = false;
            for group in groups {
                if signal_group(group, libc::SIGHUP)? && group.group == group.session {
                    shell_signalled = true;
                }
            }
            if shell_signalled
                || self
                    .session_id
                    .is_some_and(|process| unsafe { libc::getsid(process) != process })
            {
                return Ok(());
            }
        }
        match self.killer.kill() {
            #[cfg(unix)]
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(()),
            result => result,
        }
    }
}

#[derive(Default)]
struct TerminalSessions {
    live: BTreeMap<String, Session>,
    #[cfg(unix)]
    exited: BTreeMap<libc::pid_t, ExitedSession>,
}

#[cfg(unix)]
struct ExitedSession {
    terminal_id: String,
    members: Vec<ProcessIdentity>,
    token: String,
}

#[cfg(unix)]
impl ExitedSession {
    fn process_groups(
        &self,
        session: libc::pid_t,
        processes: &[libc::pid_t],
    ) -> Vec<TerminalProcessGroup> {
        let known_members: Vec<_> = self
            .members
            .iter()
            .filter(|member| member.is_in_session(session))
            .map(|member| member.pid)
            .collect();
        // A recycled session ID alone cannot establish ownership after exit.
        if known_members.is_empty() {
            return Vec::new();
        }
        TerminalProcessGroup::for_session(session, processes.iter().copied().chain(known_members))
    }
}

impl TerminalSessions {
    fn release(&mut self, id: &str) {
        let session = self.live.remove(id);
        #[cfg(unix)]
        if let Some(session) = &session
            && !session.closing
            && let Some(session_id) = session.session_id
            && unsafe { libc::getsid(session_id) != session_id }
        {
            self.exited.retain(|&session, exited| {
                exited
                    .members
                    .iter()
                    .any(|member| member.is_in_session(session))
            });
            let members: Vec<_> = session
                .process_groups(&terminal_processes())
                .into_iter()
                .flat_map(|group| group.members)
                .filter(|member| unsafe { libc::getsid(member.pid) == session_id })
                .collect();
            if !members.is_empty() {
                self.exited.insert(
                    session_id,
                    ExitedSession {
                        terminal_id: id.into(),
                        token: session
                            .process
                            .as_ref()
                            .map(|process| process.token.clone())
                            .unwrap_or_default(),
                        members,
                    },
                );
            }
        }
        drop(session);
    }
}

pub struct PtyTerminals {
    sessions: Arc<Mutex<TerminalSessions>>,
    terminate_on_drop: bool,
}

impl Default for PtyTerminals {
    fn default() -> Self {
        Self {
            sessions: Arc::new(Mutex::new(TerminalSessions::default())),
            terminate_on_drop: true,
        }
    }
}

impl PtyTerminals {
    pub fn new() -> Self {
        Self::default()
    }

    fn size(columns: u16, rows: u16) -> PtySize {
        PtySize {
            rows,
            cols: columns,
            pixel_width: 0,
            pixel_height: 0,
        }
    }

    pub fn terminate_all(&mut self) -> Result<(), TerminalError> {
        self.terminate_all_with_scan(
            #[cfg(unix)]
            list_processes,
        )
    }

    fn terminate_all_with_scan(
        &mut self,
        #[cfg(unix)] mut scan: impl FnMut() -> std::io::Result<Vec<libc::pid_t>>,
    ) -> Result<(), TerminalError> {
        let terminal_ids: std::collections::BTreeSet<String> = {
            let sessions = self.sessions()?;
            #[cfg(unix)]
            {
                sessions
                    .live
                    .keys()
                    .cloned()
                    .chain(
                        sessions
                            .exited
                            .values()
                            .map(|session| session.terminal_id.clone()),
                    )
                    .collect()
            }
            #[cfg(not(unix))]
            {
                sessions.live.keys().cloned().collect()
            }
        };
        for id in terminal_ids {
            self.close_with_scan(
                &TerminalId::new(id),
                #[cfg(unix)]
                &mut scan,
                #[cfg(unix)]
                TerminalProcessGroup::signal,
            )?;
        }
        Ok(())
    }

    pub fn write_reply(
        &self,
        terminal_id: &TerminalId,
        bytes: Vec<u8>,
    ) -> Result<(), TerminalError> {
        self.queue_write(
            terminal_id,
            WriteRequest {
                bytes,
                completion: None,
            },
        )
    }

    fn queue_write(
        &self,
        terminal_id: &TerminalId,
        request: WriteRequest,
    ) -> Result<(), TerminalError> {
        if request.bytes.len() > MAX_WRITE_BYTES {
            return Err(TerminalError::Backend(format!(
                "terminal input exceeds {MAX_WRITE_BYTES} bytes per request"
            )));
        }
        let sessions = self.sessions()?;
        let session = sessions
            .live
            .get(terminal_id.as_str())
            .filter(|session| !session.closing)
            .ok_or_else(|| {
                TerminalError::Backend(format!("no live terminal {}", terminal_id.as_str()))
            })?;
        session
            .writer
            .try_send(request)
            .map_err(|error| TerminalError::Backend(format!("terminal input queue: {error}")))
    }

    fn sessions(&self) -> Result<std::sync::MutexGuard<'_, TerminalSessions>, TerminalError> {
        self.sessions
            .lock()
            .map_err(|_| TerminalError::Backend("the terminal table is poisoned".to_string()))
    }

    pub fn process_record(
        &self,
        terminal_id: &TerminalId,
    ) -> Result<Option<ProcessRecord>, TerminalError> {
        Ok(self
            .sessions()?
            .live
            .get(terminal_id.as_str())
            .and_then(|session| session.process.clone()))
    }

    pub fn lingering_process_records(
        &self,
        terminal_id: &TerminalId,
    ) -> Result<Vec<ProcessRecord>, TerminalError> {
        #[cfg(unix)]
        {
            let sessions = self.sessions()?;
            if !sessions
                .exited
                .values()
                .any(|session| session.terminal_id == terminal_id.as_str())
            {
                return Ok(Vec::new());
            }
            let boot_id = boot_id().ok_or_else(|| {
                TerminalError::Backend("could not identify the current boot".into())
            })?;
            let mut records = Vec::new();
            for (&session_id, session) in &sessions.exited {
                if session.terminal_id != terminal_id.as_str() {
                    continue;
                }
                for member in &session.members {
                    if !member.is_in_session(session_id) {
                        continue;
                    }
                    let group = unsafe { libc::getpgid(member.pid) };
                    if group <= 1 {
                        continue;
                    }
                    let (Ok(pid), Ok(process_group)) =
                        (u32::try_from(member.pid), u32::try_from(group))
                    else {
                        continue;
                    };
                    records.push(ProcessRecord {
                        id: uuid::Uuid::new_v4().to_string(),
                        kind: "terminal".into(),
                        owner_id: terminal_id.as_str().to_owned(),
                        pid,
                        started_at: member.started_at,
                        process_group,
                        session_id: u32::try_from(session_id)
                            .map_err(|error| TerminalError::Backend(error.to_string()))?,
                        boot_id: boot_id.clone(),
                        token: session.token.clone(),
                    });
                }
            }
            Ok(records)
        }
        #[cfg(not(unix))]
        {
            let _ = terminal_id;
            Ok(Vec::new())
        }
    }
}

impl Drop for PtyTerminals {
    fn drop(&mut self) {
        if self.terminate_on_drop
            && let Err(error) = self.terminate_all()
        {
            tracing::error!(%error, "could not stop terminal sessions during drop");
        }
    }
}

impl PtyTerminals {
    fn close_with_scan(
        &mut self,
        terminal_id: &TerminalId,
        #[cfg(unix)] scan: impl FnOnce() -> std::io::Result<Vec<libc::pid_t>>,
        #[cfg(unix)] mut signal_group: impl FnMut(
            &TerminalProcessGroup,
            libc::c_int,
        ) -> std::io::Result<bool>,
    ) -> Result<(), TerminalError> {
        let mut sessions = self.sessions()?;
        let key = terminal_id.as_str();
        #[cfg(unix)]
        let mut groups = Vec::new();
        #[cfg(unix)]
        let processes = if sessions.live.contains_key(key)
            || sessions
                .exited
                .values()
                .any(|session| session.terminal_id == key)
        {
            scan().map_err(|error| {
                TerminalError::Backend(format!("could not enumerate terminal processes: {error}"))
            })?
        } else {
            Vec::new()
        };
        // The waiter owns removal so a failed signal can be retried.
        if let Some(session) = sessions
            .live
            .get_mut(key)
            .filter(|session| !session.closing)
        {
            #[cfg(unix)]
            groups.extend(session.process_groups(&processes));
            session
                .hangup(
                    #[cfg(unix)]
                    &groups,
                    #[cfg(unix)]
                    &mut signal_group,
                )
                .map_err(|error| TerminalError::Backend(format!("kill failed: {error}")))?;
        }
        #[cfg(unix)]
        if sessions
            .exited
            .values()
            .any(|session| session.terminal_id == key)
        {
            for (&session, exited) in sessions
                .exited
                .iter()
                .filter(|(_, exited)| exited.terminal_id == key)
            {
                let orphan_groups = exited.process_groups(session, &processes);
                for group in &orphan_groups {
                    signal_group(group, libc::SIGHUP)
                        .map_err(|error| TerminalError::Backend(format!("kill failed: {error}")))?;
                }
                groups.extend(orphan_groups);
            }
        }
        drop(sessions);
        #[cfg(unix)]
        TerminalProcessGroup::finish_hangup(groups, &mut signal_group)
            .map_err(|error| TerminalError::Backend(format!("kill failed: {error}")))?;
        let mut sessions = self.sessions()?;
        if let Some(session) = sessions.live.get_mut(key) {
            session.closing = true;
        }
        #[cfg(unix)]
        sessions
            .exited
            .retain(|_, exited| exited.terminal_id != key);
        Ok(())
    }
}

impl PtyTerminals {
    #[allow(dead_code)]
    pub fn spawn_recorded(
        &mut self,
        spec: TerminalSpec,
        events: mpsc::Sender<TerminalEvent>,
        recording: Arc<TerminalRecording>,
    ) -> Result<TerminalId, TerminalError> {
        if recording.initial_size != (spec.columns, spec.rows) {
            return Err(TerminalError::Backend(
                "terminal recording size does not match the terminal".into(),
            ));
        }
        self.spawn_internal(spec, events, Some(recording))
    }

    fn spawn_internal(
        &mut self,
        spec: TerminalSpec,
        events: mpsc::Sender<TerminalEvent>,
        recording: Option<Arc<TerminalRecording>>,
    ) -> Result<TerminalId, TerminalError> {
        validate_size(spec.columns, spec.rows)?;
        let terminal_id = spec.id;
        if self.sessions()?.live.contains_key(terminal_id.as_str()) {
            return Err(TerminalError::Backend("terminal is already running".into()));
        }
        let metadata = std::fs::metadata(&spec.cwd).map_err(|error| {
            TerminalError::Backend(format!(
                "cannot access terminal working directory {}: {error}",
                spec.cwd.display()
            ))
        })?;
        if !metadata.is_dir() {
            return Err(TerminalError::Backend(format!(
                "terminal working directory is not a directory: {}",
                spec.cwd.display()
            )));
        }
        let system = native_pty_system();
        let pair = system
            .openpty(Self::size(spec.columns, spec.rows))
            .map_err(|error| TerminalError::Backend(format!("openpty failed: {error}")))?;

        let mut command = CommandBuilder::new(&spec.command);
        command.args(&spec.args);
        command.cwd(&spec.cwd);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command.env("SHEPHERD_TERMINAL_ID", terminal_id.as_str());
        let token = uuid::Uuid::new_v4().to_string();
        command.env(agent_harness::PROCESS_TOKEN_ENV, &token);
        for (key, value) in &spec.environment {
            command.env(key, value);
        }

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| TerminalError::Backend(format!("reader failed: {error}")))?;
        let mut writer = pair
            .master
            .take_writer()
            .map_err(|error| TerminalError::Backend(format!("writer failed: {error}")))?;

        #[cfg(unix)]
        let (reader_poll, writer_poll) = if recording.is_some() {
            let (reader, writer) = recording_poll_handles(&*pair.master)?;
            (Some(reader), Some(writer))
        } else {
            (None, None)
        };
        #[cfg(not(unix))]
        if recording.is_some() {
            return Err(TerminalError::Backend(
                "recorded terminals require nonblocking PTY support".into(),
            ));
        }

        let failure_receiver = if let Some(recording) = &recording {
            let (sender, receiver) = std::sync::mpsc::channel();
            let mut signal = recording.failure_signal.lock().map_err(|_| {
                TerminalError::Backend("terminal recording failure signal is poisoned".into())
            })?;
            if signal.is_some() {
                return Err(TerminalError::Backend(
                    "terminal recording is already attached".into(),
                ));
            }
            *signal = Some(sender);
            Some(receiver)
        } else {
            None
        };

        let mut child = match pair.slave.spawn_command(command) {
            Ok(child) => child,
            Err(error) => {
                let mut message = format!("spawn failed: {error}");
                if let Some(recording) = &recording {
                    match recording.failure_signal.lock() {
                        Ok(mut signal) => {
                            signal.take();
                        }
                        Err(failure) => {
                            message.push_str(&format!("; recording signal is poisoned: {failure}"));
                        }
                    }
                    if let Err(failure) = recording.capture.fail() {
                        message.push_str(&format!("; could not invalidate capture: {failure}"));
                    }
                }
                return Err(TerminalError::Backend(message));
            }
        };
        drop(pair.slave);

        let (reader_finished, reader_status) = std::sync::mpsc::sync_channel(1);
        let output_events = events.clone();
        let sending = Arc::new(AtomicBool::new(false));
        let reader_sending = Arc::clone(&sending);
        let reader_recording = recording.clone();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        if let Some(recording) = &reader_recording
                            && let Err(error) = recording.output(&buffer[..read])
                        {
                            recording.report_error(&error);
                            break;
                        }
                        reader_sending.store(true, Ordering::Release);
                        let sent = output_events
                            .blocking_send(TerminalEvent::Output(buffer[..read].to_vec()));
                        reader_sending.store(false, Ordering::Release);
                        if sent.is_err() {
                            if let Some(recording) = &reader_recording {
                                recording.invalidate(
                                    "terminal output receiver closed before capture drained",
                                );
                            }
                            break;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    #[cfg(unix)]
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && reader_poll.is_some() =>
                    {
                        if let Some(fd) = &reader_poll
                            && let Err(error) = wait_recorded_pty(fd, libc::POLLIN)
                        {
                            if let Some(recording) = &reader_recording {
                                if let Err(failure) = recording.capture.fail() {
                                    recording.report_error(&failure);
                                }
                                recording.report_error(&error);
                            }
                            break;
                        }
                    }
                    Err(error) => {
                        if let Some(recording) = &reader_recording {
                            #[cfg(target_os = "linux")]
                            let normal_hangup = error.raw_os_error() == Some(libc::EIO);
                            #[cfg(not(target_os = "linux"))]
                            let normal_hangup = false;
                            if !normal_hangup {
                                if let Err(failure) = recording.capture.fail() {
                                    recording.report_error(&failure);
                                }
                                recording.report_error(&error);
                            }
                        }
                        tracing::debug!(%error, "terminal reader stopped");
                        break;
                    }
                }
            }
            if reader_finished.send(()).is_err() {
                tracing::debug!("terminal waiter stopped");
            }
        });

        let (input, mut writes) = mpsc::channel::<WriteRequest>(32);
        let writer_recording = recording.clone();
        std::thread::spawn(move || {
            while let Some(request) = writes.blocking_recv() {
                let result = if let Some(recording) = &writer_recording {
                    #[cfg(unix)]
                    let outcome = match &writer_poll {
                        Some(fd) => write_recorded_pty(&mut *writer, fd, recording, &request.bytes),
                        None => Err(HarnessError::Protocol(
                            "recorded terminal has no poll descriptor".into(),
                        )),
                    };
                    #[cfg(not(unix))]
                    let outcome: HarnessResult<()> = Err(HarnessError::Protocol(
                        "recorded terminals require nonblocking PTY support".into(),
                    ));
                    outcome.map_err(|error| {
                        if let Err(failure) = recording.capture.fail() {
                            recording.report_error(&failure);
                        }
                        recording.report_error(&error);
                        TerminalError::Backend(error.to_string())
                    })
                } else {
                    writer
                        .write_all(&request.bytes)
                        .and_then(|()| writer.flush())
                        .map_err(|error| TerminalError::Backend(format!("write failed: {error}")))
                };
                match request.completion {
                    Some(completion) => {
                        if let Err(Err(error)) = completion.send(result) {
                            tracing::debug!(%error, "terminal input requester disconnected");
                        }
                    }
                    None => {
                        if let Err(error) = result {
                            tracing::debug!(%error, "could not write terminal reply");
                        }
                    }
                }
            }
        });

        let killer = child.clone_killer();
        #[cfg(unix)]
        let session_id = child
            .process_id()
            .and_then(|pid| libc::pid_t::try_from(pid).ok());
        #[cfg(unix)]
        let process = session_id
            .and_then(|pid| capture_process_record(pid, "terminal", terminal_id.as_str(), &token));
        #[cfg(not(unix))]
        let process = None;
        let waiter_recording = recording.clone();
        let monitor_errors = recording.as_ref().map(|recording| recording.errors.clone());
        self.sessions()?.live.insert(
            terminal_id.as_str().to_string(),
            Session {
                writer: input,
                closing: false,
                killer,
                master: pair.master,
                #[cfg(unix)]
                session_id,
                process,
                recording,
            },
        );

        if let Some(failure_receiver) = failure_receiver {
            let sessions = Arc::downgrade(&self.sessions);
            let terminal_id = terminal_id.clone();
            std::thread::spawn(move || {
                if failure_receiver.recv().is_ok() {
                    if let Some(sessions) = sessions.upgrade() {
                        let mut terminal = PtyTerminals {
                            sessions,
                            terminate_on_drop: false,
                        };
                        if let Err(error) = terminal.close(&terminal_id) {
                            tracing::error!(%error, terminal_id = terminal_id.as_str(), "could not stop terminal after recording failure");
                            if let Some(sender) = &monitor_errors
                                && sender
                                    .send(format!(
                                        "could not stop terminal after recording failure: {error}"
                                    ))
                                    .is_err()
                            {
                                tracing::error!(%error, "terminal recording error receiver closed");
                            }
                        }
                    }
                }
            });
        }

        let sessions = Arc::clone(&self.sessions);
        let id = terminal_id.as_str().to_string();
        std::thread::spawn(move || {
            let (exit_code, signal) = match child.wait() {
                Ok(status) => (
                    Some(status.exit_code() as i32),
                    status.signal().map(str::to_owned),
                ),
                Err(error) => {
                    if let Some(recording) = &waiter_recording {
                        recording.invalidate("terminal process wait failed during capture");
                    }
                    tracing::debug!(%error, "waiting for the terminal child failed");
                    (None, None)
                }
            };
            match sessions.lock() {
                Ok(mut sessions) => {
                    sessions.release(&id);
                }
                Err(error) => tracing::error!(%error, "could not release terminal descriptors"),
            }
            loop {
                match reader_status.recv_timeout(Duration::from_millis(150)) {
                    Ok(()) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        if let Some(recording) = &waiter_recording {
                            recording.invalidate(
                                "terminal reader stopped without confirming capture drain",
                            );
                        }
                        break;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        // A full queue still has output ahead of the exit. Only time out
                        // a reader waiting for descendants that inherited the slave.
                        if !sending.load(Ordering::Acquire) || events.is_closed() {
                            if let Some(recording) = &waiter_recording {
                                recording.invalidate(
                                    "terminal output did not drain before capture exit",
                                );
                            }
                            tracing::debug!("terminal reader did not reach EOF before exit");
                            break;
                        }
                    }
                }
            }
            if let Some(recording) = &waiter_recording {
                match recording.failure_signal.lock() {
                    Ok(mut signal) => {
                        signal.take();
                    }
                    Err(error) => {
                        tracing::error!(%error, "terminal recording failure signal is poisoned")
                    }
                }
            }
            if events
                .blocking_send(TerminalEvent::Exited { exit_code, signal })
                .is_err()
            {
                tracing::debug!("terminal event receiver closed");
            }
        });

        Ok(terminal_id)
    }
}

impl TerminalService for PtyTerminals {
    fn spawn(
        &mut self,
        spec: TerminalSpec,
        events: mpsc::Sender<TerminalEvent>,
    ) -> Result<TerminalId, TerminalError> {
        self.spawn_internal(spec, events, None)
    }

    fn write(
        &mut self,
        terminal_id: &TerminalId,
        bytes: Vec<u8>,
    ) -> Result<TerminalWrite, TerminalError> {
        let (completion, result) = oneshot::channel();
        self.queue_write(
            terminal_id,
            WriteRequest {
                bytes,
                completion: Some(completion),
            },
        )?;
        Ok(result)
    }

    fn resize(
        &mut self,
        terminal_id: &TerminalId,
        columns: u16,
        rows: u16,
    ) -> Result<(), TerminalError> {
        validate_size(columns, rows)?;
        let sessions = self.sessions()?;
        let session = sessions
            .live
            .get(terminal_id.as_str())
            .filter(|session| !session.closing)
            .ok_or_else(|| {
                TerminalError::Backend(format!("no live terminal {}", terminal_id.as_str()))
            })?;
        if let Some(recording) = &session.recording {
            recording
                .capture
                .record_apply_and_publish(
                    CaptureEvent::Resize { columns, rows },
                    || {
                        session
                            .master
                            .resize(Self::size(columns, rows))
                            .map_err(|error| {
                                HarnessError::Protocol(format!("resize failed: {error}"))
                            })?;
                        let mut screen = recording.screen.lock().map_err(|_| {
                            HarnessError::Protocol(
                                "terminal recording screen lock is poisoned".into(),
                            )
                        })?;
                        screen.resize(usize::from(columns), usize::from(rows));
                        let state = recording.screen_state(&screen)?;
                        Ok((state.clone(), vec![CaptureEvent::Screen { state }]))
                    },
                    |state| {
                        recording.publish_screen(RecordedScreenUpdate::Resize {
                            columns,
                            rows,
                            state: state.clone(),
                        })
                    },
                )
                .map(|_| ())
                .map_err(|error| {
                    recording.report_error(&error);
                    TerminalError::Backend(error.to_string())
                })
        } else {
            session
                .master
                .resize(Self::size(columns, rows))
                .map_err(|error| TerminalError::Backend(format!("resize failed: {error}")))
        }
    }

    fn close(&mut self, terminal_id: &TerminalId) -> Result<(), TerminalError> {
        self.close_with_scan(
            terminal_id,
            #[cfg(unix)]
            list_processes,
            #[cfg(unix)]
            TerminalProcessGroup::signal,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    #[cfg(all(unix, feature = "git"))]
    use terminal_replay::verify_recorded_screens;
    #[cfg(all(unix, any(feature = "git", feature = "live-freebuff")))]
    use terminal_replay::verify_unfinished_screens;

    use super::*;

    #[cfg(all(unix, any(feature = "git", feature = "live-freebuff")))]
    struct CaptureRunnerConfig {
        workspace: std::path::PathBuf,
        worktree: std::path::PathBuf,
        markers: std::path::PathBuf,
        executable: std::path::PathBuf,
        args: Vec<String>,
        metadata: std::path::PathBuf,
        prompt: Vec<u8>,
        scenario: String,
    }

    #[cfg(all(unix, any(feature = "git", feature = "live-freebuff")))]
    async fn wait_for_capture_session_exit(
        record: &ProcessRecord,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let session = libc::pid_t::try_from(record.session_id)?;
        if session <= 1 {
            return Err("invalid recorded terminal session".into());
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let mut alive = false;
            for pid in list_processes()? {
                if pid <= 1 {
                    continue;
                }
                let observed = unsafe { libc::getsid(pid) };
                if observed == session {
                    alive = true;
                    break;
                }
                if observed < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ESRCH) {
                        return Err(error.into());
                    }
                }
            }
            if !alive {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("terminal session still has running processes after shutdown".into());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[cfg(all(unix, any(feature = "git", feature = "live-freebuff")))]
    async fn run_capture(config: CaptureRunnerConfig) -> Result<(), Box<dyn std::error::Error>> {
        use agent_harness::freebuff_capture::CaptureEvent;
        use shepherd_core::harness::FreebuffLiveCapture;
        use std::io::Write as _;
        fn runtime(
            session: &agent_harness::freebuff_capture::CaptureSession,
            directory: &std::path::Path,
            executable: &std::path::Path,
        ) -> Result<(), Box<dyn std::error::Error>> {
            let metadata = agent_harness::freebuff::read_runtime_metadata(directory)?;
            let launcher_version = agent_harness::freebuff::read_launcher_version(executable)?;
            session.record_and_apply(
                CaptureEvent::Runtime {
                    launcher_version,
                    runtime_version: metadata.as_ref().map(|metadata| metadata.version.clone()),
                    target: metadata.map(|metadata| metadata.target),
                },
                || Ok(((), Vec::new())),
            )?;
            Ok(())
        }

        let CaptureRunnerConfig {
            workspace,
            worktree,
            markers,
            executable,
            args,
            metadata,
            prompt,
            scenario,
        } = config;
        let worktree = worktree.canonicalize()?;
        let markers = markers.canonicalize()?;
        if !executable.is_absolute() || markers.starts_with(&worktree) {
            return Err("use an absolute executable and markers outside the worktree".into());
        }
        let send_marker = markers.join("send");
        let entered_marker = markers.join("entered");
        let submit_marker = markers.join("submit");
        let stop_marker = markers.join("stop");
        if [&send_marker, &entered_marker, &submit_marker, &stop_marker]
            .iter()
            .any(|marker| marker.exists())
        {
            return Err("capture marker files must not exist before launch".into());
        }
        if prompt.is_empty() || prompt.len() >= MAX_WRITE_BYTES {
            return Err("approved prompt must contain 1 to 65535 bytes".into());
        }
        std::str::from_utf8(&prompt)?;
        let capture = FreebuffLiveCapture::begin(&workspace, worktree.clone(), &scenario)?;
        let session = capture.session();
        runtime(&session, &metadata, &executable)?;
        let renderer_dependency = include_str!("../../../../libs/terminal-renderer/Cargo.toml")
            .lines()
            .find(|line| line.starts_with("alacritty_terminal = "))
            .ok_or("terminal renderer dependency is missing from the manifest")?;
        session.record_and_apply(
            CaptureEvent::Terminal {
                term: "xterm-256color".into(),
                color_term: "truecolor".into(),
                renderer: format!(
                    "shepherd {} {renderer_dependency}",
                    env!("CARGO_PKG_VERSION"),
                ),
            },
            || Ok(((), Vec::new())),
        )?;
        let (error_sender, mut errors) = mpsc::unbounded_channel();
        let recording = Arc::new(TerminalRecording::from_shared(
            session.clone(),
            100,
            30,
            error_sender,
        )?);
        let mut engine = PtyTerminals::new();
        let (sender, mut output) = mpsc::channel(32);
        let terminal_id = engine.spawn_recorded(
            TerminalSpec {
                id: TerminalId::new("approved-freebuff-capture"),
                columns: 100,
                rows: 30,
                cwd: worktree,
                command: executable
                    .to_str()
                    .ok_or("executable path is not UTF-8")?
                    .into(),
                args,
                environment: BTreeMap::new(),
            },
            sender,
            recording,
        )?;
        let process = engine
            .process_record(&terminal_id)?
            .ok_or("could not establish terminal process identity for capture")?;
        eprintln!(
            "Inspect the terminal output. When the composer is ready, create send to enter the approved prompt. After entered appears, inspect the composer and create submit to press Enter. Create stop only after the response is complete."
        );
        let mut entering = false;
        let mut entered = false;
        let mut submitted = false;
        let mut stopping = false;
        let mut errors_open = true;
        let mut pending_write: Option<TerminalWrite> = None;
        let deadline = tokio::time::sleep(Duration::from_secs(600));
        tokio::pin!(deadline);
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        loop {
            tokio::select! {
                _ = &mut deadline => return Err("capture timed out".into()),
                error = errors.recv(), if errors_open => match error {
                    Some(error) => return Err(error.into()),
                    None => errors_open = false,
                },
                result = async {
                    match pending_write.as_mut() {
                        Some(write) => write.await.map_err(|error| error.to_string()),
                        None => Err("missing terminal input completion".into()),
                    }
                }, if pending_write.is_some() => {
                    result??;
                    pending_write = None;
                    if entering && !entered {
                        entered = true;
                        std::fs::write(&entered_marker, b"")?;
                    }
                },
                event = output.recv() => match event {
                    Some(TerminalEvent::Output(bytes)) => {
                        let mut stdout = std::io::stdout().lock();
                        stdout.write_all(&bytes)?;
                        stdout.flush()?;
                    }
                    Some(TerminalEvent::Exited { exit_code, signal }) => {
                        session.record_and_apply(
                            CaptureEvent::Exit { exit_code, signal, operator_stopped: stopping },
                            || Ok(((), Vec::new())),
                        )?;
                        if !submitted || !stopping || pending_write.is_some() { return Err("terminal exited before the operator confirmed completion".into()); }
                        if let Ok(error) = errors.try_recv() { return Err(error.into()); }
                        break;
                    }
                    None => return Err("terminal output closed without an exit event".into()),
                },
                _ = tick.tick() => {
                    if !submitted && stop_marker.exists() {
                        return Err("stop marker appeared before submission".into());
                    }
                    if !entered && submit_marker.exists() {
                        return Err("submit marker appeared before prompt entry completed".into());
                    }
                    if !entering && send_marker.exists() {
                        pending_write = Some(engine.write(&terminal_id, prompt.clone())?);
                        entering = true;
                    }
                    if entered && !submitted && pending_write.is_none() && submit_marker.exists() {
                        pending_write = Some(engine.write(&terminal_id, vec![b'\r'])?);
                        submitted = true;
                    }
                    if submitted && pending_write.is_none() && !stopping && stop_marker.exists() {
                        engine.close(&terminal_id)?;
                        stopping = true;
                    }
                }
            }
        }
        wait_for_capture_session_exit(&process).await?;
        eprintln!(
            "Worktree effects may be incomplete: processes that escaped the terminal session are not detected."
        );
        runtime(&session, &metadata, &executable)?;
        let recorded_capture_path = workspace
            .join(".cache/shepherd-live/freebuff")
            .join(format!("{scenario}.jsonl"));
        verify_unfinished_screens(&recorded_capture_path)?;
        capture.finish_after_drain()?;
        Ok(())
    }

    #[cfg(all(feature = "live-freebuff", unix))]
    #[tokio::test]
    #[ignore = "requires an explicitly approved Freebuff prompt and disposable worktree"]
    async fn approved_freebuff_capture() -> Result<(), Box<dyn std::error::Error>> {
        fn required(name: &str) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
            std::env::var_os(name)
                .map(std::path::PathBuf::from)
                .ok_or_else(|| format!("missing {name}").into())
        }
        let prompt = std::fs::read(required("SHEPHERD_FREEBUFF_PROMPT_FILE")?)?;
        run_capture(CaptureRunnerConfig {
            workspace: required("SHEPHERD_FREEBUFF_WORKSPACE")?,
            worktree: required("SHEPHERD_FREEBUFF_WORKTREE")?,
            markers: required("SHEPHERD_FREEBUFF_MARKERS")?,
            executable: required("SHEPHERD_FREEBUFF_EXECUTABLE")?,
            args: Vec::new(),
            metadata: required("SHEPHERD_FREEBUFF_METADATA_DIR")?,
            prompt,
            scenario: std::env::var("SHEPHERD_FREEBUFF_SCENARIO")?,
        })
        .await
    }

    #[cfg(all(feature = "live-freebuff", unix))]
    #[test]
    #[ignore = "requires a complete private capture and a clean disposable worktree"]
    fn replay_approved_freebuff_capture() -> Result<(), Box<dyn std::error::Error>> {
        let capture = std::env::var_os("SHEPHERD_FREEBUFF_CAPTURE")
            .map(std::path::PathBuf::from)
            .ok_or("missing SHEPHERD_FREEBUFF_CAPTURE")?;
        let worktree = std::env::var_os("SHEPHERD_FREEBUFF_WORKTREE")
            .map(std::path::PathBuf::from)
            .ok_or("missing SHEPHERD_FREEBUFF_WORKTREE")?;
        let mut frames = 0usize;
        let mut warnings = terminal_replay::FreebuffWarningTracker::default();
        terminal_replay::replay_freebuff_capture_changes(&capture, &worktree, |_, frame| {
            frames += 1;
            warnings.observe(frame)?;
            Ok(())
        })?;
        if frames == 0 {
            return Err("capture contains no screen frames".into());
        }
        if let Some(warning) = warnings.warning() {
            eprintln!("Freebuff screen warning observed: {warning:?}");
        }
        eprintln!(
            "Recorded worktree effects may be incomplete: escaped processes may not have been detected."
        );
        Ok(())
    }

    #[cfg(all(feature = "live-freebuff", unix))]
    #[test]
    #[ignore = "requires a private incomplete capture"]
    fn inspect_unfinished_freebuff_capture() -> Result<(), Box<dyn std::error::Error>> {
        let capture = std::env::var_os("SHEPHERD_FREEBUFF_CAPTURE")
            .map(std::path::PathBuf::from)
            .ok_or("missing SHEPHERD_FREEBUFF_CAPTURE")?;
        let mut frames = 0usize;
        let mut warnings = terminal_replay::FreebuffWarningTracker::default();
        terminal_replay::visit_unfinished_screen_changes(&capture, |_, frame| {
            frames += 1;
            warnings.observe(frame)
        })?;
        if frames == 0 {
            return Err("capture contains no screen frames".into());
        }
        eprintln!(
            "Verified {frames} incomplete-capture screen changes; warning: {:?}",
            warnings.warning()
        );
        Ok(())
    }

    #[cfg(all(unix, feature = "git"))]
    #[tokio::test]
    async fn capture_runner_records_fake_cli_and_worktree_effects()
    -> Result<(), Box<dyn std::error::Error>> {
        use agent_harness::freebuff_capture::{CaptureEvent, read_complete_capture};
        use shepherd_core::test_support::TestRepository;

        async fn wait_for(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
            tokio::time::timeout(Duration::from_secs(10), async {
                while !path.exists() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await?;
            Ok(())
        }

        let repository = TestRepository::new()?;
        repository.write_file(".gitignore", ".freebuff/\n")?;
        for arguments in [
            ["add", ".gitignore"].as_slice(),
            ["commit", "-m", "ignore"].as_slice(),
        ] {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(repository.path())
                .args(arguments)
                .stdout(std::process::Stdio::null())
                .status()?;
            assert!(status.success());
        }
        let workspace = tempfile::tempdir()?;
        let markers = tempfile::tempdir()?;
        std::fs::write(
            markers.path().join("freebuff-metadata.json"),
            br#"{"version":"fixture","target":"local"}"#,
        )?;
        let marker_path = markers.path().to_path_buf();
        let config = CaptureRunnerConfig {
            workspace: workspace.path().to_path_buf(),
            worktree: repository.path().to_path_buf(),
            markers: marker_path.clone(),
            executable: "/bin/sh".into(),
            args: vec!["-c".into(), "printf ready > \"$1/ready\"; IFS= read -r prompt; printf 'fake:%s\\n' \"$prompt\"; printf 'changed\\n' > README.md; mkdir .freebuff; printf 'fixture-id\\n' > .freebuff/project-id; printf done > \"$1/replied\"; exec sleep 30".into(), "fixture".into(), marker_path.to_str().ok_or("non-UTF-8 marker path")?.into()],
            metadata: marker_path.clone(),
            prompt: b"inspect README".to_vec(),
            scenario: "fake-runner".into(),
        };
        let operator = async {
            wait_for(&marker_path.join("ready")).await?;
            std::fs::write(marker_path.join("send"), b"")?;
            wait_for(&marker_path.join("entered")).await?;
            std::fs::write(marker_path.join("submit"), b"")?;
            wait_for(&marker_path.join("replied")).await?;
            std::fs::write(marker_path.join("stop"), b"")?;
            Ok::<(), Box<dyn std::error::Error>>(())
        };
        let (result, operator_result) = tokio::time::timeout(Duration::from_secs(15), async {
            tokio::join!(run_capture(config), operator)
        })
        .await?;
        operator_result?;
        result?;
        let completed_capture_path = workspace
            .path()
            .join(".cache/shepherd-live/freebuff/fake-runner.jsonl");
        verify_recorded_screens(&completed_capture_path)?;
        let records = read_complete_capture(&completed_capture_path)?;
        let input: Vec<u8> = records
            .iter()
            .filter_map(|record| match &record.event {
                CaptureEvent::Input { bytes } => Some(bytes.as_slice()),
                _ => None,
            })
            .flatten()
            .copied()
            .collect();
        let output: Vec<u8> = records
            .iter()
            .filter_map(|record| match &record.event {
                CaptureEvent::Output { bytes } => Some(bytes.as_slice()),
                _ => None,
            })
            .flatten()
            .copied()
            .collect();
        assert_eq!(input, b"inspect README\r");
        let input_events: Vec<&[u8]> = records
            .iter()
            .filter_map(|record| match &record.event {
                CaptureEvent::Input { bytes } => Some(bytes.as_slice()),
                _ => None,
            })
            .collect();
        assert_eq!(
            input_events,
            [b"inspect README".as_slice(), b"\r".as_slice()]
        );
        assert!(
            output
                .windows(b"fake:inspect README".len())
                .any(|window| window == b"fake:inspect README")
        );
        assert!(
            records
                .iter()
                .any(|record| matches!(record.event, CaptureEvent::Screen { .. }))
        );
        assert!(records.iter().any(|record| matches!(&record.event, CaptureEvent::Terminal { term, .. } if term == "xterm-256color")));
        assert!(records.iter().any(|record| matches!(
            record.event,
            CaptureEvent::Exit {
                operator_stopped: true,
                ..
            }
        )));
        assert!(records.iter().any(|record| matches!(&record.event, CaptureEvent::WorktreeFile { path, contents, .. } if path == std::path::Path::new("README.md") && contents == b"changed\n")));
        assert!(records.iter().any(|record| matches!(&record.event, CaptureEvent::WorktreeFile { path, contents, .. } if path == std::path::Path::new(".freebuff/project-id") && contents == b"fixture-id\n")));
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn undrained_recorded_terminal_cannot_finish_capture()
    -> Result<(), Box<dyn std::error::Error>> {
        use agent_harness::freebuff_capture::{CaptureSession, CaptureWriter};
        let directory = tempfile::tempdir()?;
        let capture = CaptureSession::new(CaptureWriter::create(directory.path(), "undrained")?);
        let (error_sender, mut errors) = mpsc::unbounded_channel();
        let recording = Arc::new(TerminalRecording::new(capture, 80, 24, error_sender)?);
        let mut engine = PtyTerminals::new();
        let (sender, mut receiver) = mpsc::channel(8);
        engine.spawn_recorded(
            TerminalSpec {
                id: TerminalId::new("undrained"),
                columns: 80,
                rows: 24,
                cwd: directory.path().to_path_buf(),
                command: "/bin/sh".into(),
                args: vec!["-c".into(), "printf 'undrained output'; exit 0".into()],
                environment: BTreeMap::new(),
            },
            sender,
            recording.clone(),
        )?;
        // macOS closes the PTY as soon as the shell exits, so a background
        // descendant cannot keep the slave open. Closing the receiver while the
        // child still writes exercises the same undrained-capture failure.
        drop(receiver);
        let error = tokio::time::timeout(Duration::from_secs(5), errors.recv())
            .await?
            .ok_or("missing drain error")?;
        assert!(error.contains("drain"));
        assert!(recording.capture.finish().is_err());
        Ok(())
    }

    #[test]
    fn a_full_screen_observer_fails_the_recording_without_dropping_frames()
    -> Result<(), Box<dyn std::error::Error>> {
        use agent_harness::freebuff_capture::{CaptureSession, CaptureWriter};
        let directory = tempfile::tempdir()?;
        let capture =
            CaptureSession::new(CaptureWriter::create(directory.path(), "observer-full")?);
        let (errors, _receiver) = mpsc::unbounded_channel();
        let (observer, mut frames) = mpsc::channel(1);
        let recording =
            TerminalRecording::new(capture, 80, 24, errors)?.with_screen_observer(observer);
        recording.output(b"one")?;
        assert!(recording.output(b"two").is_err());
        assert!(frames.try_recv().is_ok());
        assert!(recording.output(b"three").is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[cfg(unix)]
    #[test]
    fn failed_recorded_spawn_invalidates_capture() -> Result<(), Box<dyn std::error::Error>> {
        use agent_harness::freebuff_capture::{CaptureSession, CaptureWriter};
        let directory = tempfile::tempdir()?;
        let capture =
            CaptureSession::new(CaptureWriter::create(directory.path(), "spawn-failure")?);
        let (error_sender, _errors) = mpsc::unbounded_channel();
        let recording = Arc::new(TerminalRecording::new(capture, 80, 24, error_sender)?);
        let mut engine = PtyTerminals::new();
        let (sender, _receiver) = mpsc::channel(8);
        let result = engine.spawn_recorded(
            TerminalSpec {
                id: TerminalId::new("failed-spawn"),
                columns: 80,
                rows: 24,
                cwd: directory.path().to_path_buf(),
                command: directory
                    .path()
                    .join("missing-executable")
                    .display()
                    .to_string(),
                args: Vec::new(),
                environment: BTreeMap::new(),
            },
            sender,
            recording.clone(),
        );
        assert!(result.is_err());
        assert!(
            recording
                .failure_signal
                .lock()
                .map_err(|error| error.to_string())?
                .is_none()
        );
        assert!(recording.capture.finish().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn recording_failure_stops_the_terminal_process() -> Result<(), Box<dyn std::error::Error>>
    {
        use agent_harness::freebuff_capture::{CaptureSession, CaptureWriter};
        let directory = tempfile::tempdir()?;
        let capture =
            CaptureSession::new(CaptureWriter::create(directory.path(), "stop-on-failure")?);
        let (error_sender, mut errors) = mpsc::unbounded_channel();
        let recording = Arc::new(TerminalRecording::new(capture, 80, 24, error_sender)?);
        recording.capture.fail()?;
        let mut engine = PtyTerminals::new();
        let (sender, mut receiver) = mpsc::channel(8);
        engine.spawn_recorded(
            TerminalSpec {
                id: TerminalId::new("recording-failure"),
                columns: 80,
                rows: 24,
                cwd: directory.path().to_path_buf(),
                command: "/bin/sh".into(),
                args: vec!["-c".into(), "printf trigger; exec sleep 30".into()],
                environment: BTreeMap::new(),
            },
            sender,
            recording,
        )?;
        let message = tokio::time::timeout(Duration::from_secs(10), errors.recv())
            .await?
            .ok_or("no recording error")?;
        assert!(message.contains("failed"));
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(event) = receiver.recv().await {
                if matches!(event, TerminalEvent::Exited { .. }) {
                    return Ok::<(), String>(());
                }
            }
            Err("terminal exited without an exit event".into())
        })
        .await??;
        Ok(())
    }

    #[tokio::test]
    async fn recorded_terminal_drains_output_while_input_waits_for_the_child()
    -> Result<(), Box<dyn std::error::Error>> {
        use agent_harness::freebuff_capture::{CaptureSession, CaptureWriter};
        let directory = tempfile::tempdir()?;
        let capture = CaptureSession::new(CaptureWriter::create(directory.path(), "backpressure")?);
        let (error_sender, mut errors) = mpsc::unbounded_channel();
        let recording = Arc::new(TerminalRecording::new(capture, 20, 3, error_sender)?);
        let mut engine = PtyTerminals::new();
        let (sender, mut receiver) = mpsc::channel(32);
        let id = engine.spawn_recorded(TerminalSpec {
            id: TerminalId::new("backpressure"), columns: 20, rows: 3,
            cwd: directory.path().to_path_buf(), command: "/bin/sh".into(),
            args: vec!["-c".into(), "stty raw -echo; printf READY; head -c 131072 /dev/zero; head -c 65536 >/dev/null".into()],
            environment: BTreeMap::new(),
        }, sender, recording)?;
        let mut first = Vec::new();
        while first.len() < 5 {
            let event = tokio::time::timeout(Duration::from_secs(10), receiver.recv())
                .await?
                .ok_or("terminal exited before readiness")?;
            let TerminalEvent::Output(bytes) = event else {
                return Err("terminal exited before readiness".into());
            };
            first.extend(bytes);
        }
        assert!(first.starts_with(b"READY"));
        let write = engine.write(&id, vec![b'x'; MAX_WRITE_BYTES])?;
        let reader = async {
            let mut count = first.len();
            while let Some(event) = receiver.recv().await {
                match event {
                    TerminalEvent::Output(bytes) => count += bytes.len(),
                    TerminalEvent::Exited { exit_code, .. } => return (count, exit_code),
                }
            }
            (count, None)
        };
        let (write_result, (count, exit_code)) =
            tokio::time::timeout(Duration::from_secs(15), async {
                tokio::join!(write, reader)
            })
            .await?;
        write_result??;
        assert_eq!(count, 131077);
        assert_eq!(exit_code, Some(0));
        assert!(errors.try_recv().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn fake_cli_pty_stream_can_be_recorded_with_screen_states()
    -> Result<(), Box<dyn std::error::Error>> {
        use agent_harness::freebuff_capture::{
            CaptureEvent, CaptureSession, CaptureWriter, read_capture,
        };
        let directory = tempfile::tempdir()?;
        let session = CaptureSession::new(CaptureWriter::create(directory.path(), "fake-pty")?);
        let (error_sender, mut errors) = mpsc::unbounded_channel();
        let (screen_sender, mut screens) = mpsc::channel(32);
        let recording = Arc::new(
            TerminalRecording::new(session, 80, 24, error_sender)?
                .with_screen_observer(screen_sender),
        );
        let mut engine = PtyTerminals::new();
        let (sender, mut receiver) = mpsc::channel(16);
        let terminal_id = engine.spawn_recorded(
            TerminalSpec {
                id: TerminalId::new("fake-freebuff"),
                columns: 80,
                rows: 24,
                cwd: directory.path().to_path_buf(),
                command: "/bin/sh".into(),
                args: vec![
                    "-c".into(),
                    "printf 'ready>'; IFS= read -r line; printf 'reply:%s\\n' \"$line\"".into(),
                ],
                environment: BTreeMap::new(),
            },
            sender,
            recording,
        )?;
        let mut output = Vec::new();
        let mut submitted = false;
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(event) = receiver.recv().await {
                match event {
                    TerminalEvent::Output(bytes) => {
                        output.extend(bytes);
                        if !submitted && output.windows(6).any(|bytes| bytes == b"ready>") {
                            engine
                                .resize(&terminal_id, 90, 24)
                                .map_err(|error| error.to_string())?;
                            let completion = engine
                                .write(&terminal_id, b"hello\n".to_vec())
                                .map_err(|error| error.to_string())?;
                            completion
                                .await
                                .map_err(|error| error.to_string())?
                                .map_err(|error| error.to_string())?;
                            submitted = true;
                        }
                    }
                    TerminalEvent::Exited { exit_code, .. } => {
                        if exit_code != Some(0) {
                            return Err("fake CLI failed".to_string());
                        }
                        return Ok::<(), String>(());
                    }
                }
            }
            Err("fake CLI output ended before exit".into())
        })
        .await??;
        assert!(errors.try_recv().is_err());
        assert!(output.windows(11).any(|bytes| bytes == b"reply:hello"));
        let records = read_capture(
            &directory
                .path()
                .join(".cache/shepherd-live/freebuff/fake-pty.jsonl"),
        )?;
        let expected_updates: Vec<_> = records
            .windows(2)
            .filter_map(|records| {
                let [first, second] = records else {
                    return None;
                };
                match (&first.event, &second.event) {
                    (CaptureEvent::Output { bytes }, CaptureEvent::Screen { state }) => {
                        Some(RecordedScreenUpdate::Output {
                            bytes: bytes.clone(),
                            state: state.clone(),
                        })
                    }
                    (CaptureEvent::Resize { columns, rows }, CaptureEvent::Screen { state }) => {
                        Some(RecordedScreenUpdate::Resize {
                            columns: *columns,
                            rows: *rows,
                            state: state.clone(),
                        })
                    }
                    _ => None,
                }
            })
            .collect();
        let mut observed_updates = Vec::new();
        while let Ok(update) = screens.try_recv() {
            observed_updates.push(update);
        }
        assert!(!observed_updates.is_empty());
        assert_eq!(observed_updates, expected_updates);
        assert!(
            records
                .iter()
                .any(|record| matches!(record.event, CaptureEvent::Input { .. }))
        );
        assert!(
            records
                .iter()
                .any(|record| matches!(record.event, CaptureEvent::Screen { .. }))
        );
        assert!(
            records
                .iter()
                .any(|record| matches!(record.event, CaptureEvent::Resize { columns: 90, .. }))
        );
        Ok(())
    }

    #[test]
    fn refuses_unavailable_terminal_working_directories() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("file");
        std::fs::write(&file, "not a directory")?;
        let missing = directory.path().join("missing");
        let mut engine = PtyTerminals::new();

        for (cwd, expected) in [(&missing, "cannot access"), (&file, "not a directory")] {
            let (sender, _receiver) = mpsc::channel(1);
            let result = engine.spawn(
                TerminalSpec {
                    id: TerminalId::new("invalid-cwd"),
                    columns: 80,
                    rows: 24,
                    cwd: cwd.to_path_buf(),
                    command: "/bin/true".to_string(),
                    args: Vec::new(),
                    environment: BTreeMap::new(),
                },
                sender,
            );
            match result {
                Err(TerminalError::Backend(message)) => {
                    assert!(message.contains(expected), "{message}");
                    assert!(message.contains(&cwd.display().to_string()), "{message}");
                }
                other => {
                    return Err(format!("expected a working directory error, got {other:?}").into());
                }
            }
        }
        Ok(())
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn process_group_inspection_rejects_invalid_and_old_boot_records()
    -> Result<(), Box<dyn std::error::Error>> {
        let pid = libc::pid_t::try_from(std::process::id())?;
        let mut record =
            capture_process_record(pid, "agent", "task", "token").ok_or("identity unavailable")?;
        let current_boot = record.boot_id.clone();
        record.boot_id = format!("different-{current_boot}");
        assert!(!process_group_has_other_members(&record)?);
        record.boot_id = current_boot;
        record.process_group = 1;
        assert!(process_group_has_other_members(&record).is_err());
        Ok(())
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn captured_process_record_includes_verified_identity() -> Result<(), Box<dyn std::error::Error>>
    {
        let pid = libc::pid_t::try_from(std::process::id())?;
        let record =
            capture_process_record(pid, "agent", "task", "token").ok_or("identity unavailable")?;
        assert_eq!(record.token, "token");
        assert_eq!(record.pid, u32::try_from(pid)?);
        assert_eq!(record.kind, "agent");
        assert_eq!(record.owner_id, "task");
        assert_eq!(Some(record.started_at), process_start_time(pid));
        assert_eq!(
            record.process_group,
            u32::try_from(unsafe { libc::getpgid(pid) })?
        );
        assert_eq!(
            record.session_id,
            u32::try_from(unsafe { libc::getsid(pid) })?
        );
        assert_eq!(Some(record.boot_id), boot_id());
        assert!(capture_process_record(1, "agent", "task", "token").is_none());
        Ok(())
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn process_identity_rejects_a_reused_pid() -> Result<(), Box<dyn std::error::Error>> {
        let pid = libc::pid_t::try_from(std::process::id())?;
        let identity = ProcessIdentity::new(pid).ok_or("could not read process start time")?;
        let session = unsafe { libc::getsid(pid) };
        assert!(session > 0);
        assert!(identity.is_in_session(session));
        let reused = ProcessIdentity {
            pid,
            started_at: identity.started_at.wrapping_add(1),
        };
        assert!(!reused.is_in_session(session));
        Ok(())
    }

    #[test]
    fn a_pty_reports_its_output_and_exit_code() {
        let mut engine = PtyTerminals::new();
        let (sender, mut receiver) = mpsc::channel(8);
        engine
            .spawn(
                TerminalSpec {
                    id: TerminalId::new("test-terminal"),
                    columns: 80,
                    rows: 24,
                    cwd: std::env::temp_dir(),
                    command: "/bin/sh".to_string(),
                    args: vec!["-c".to_string(), "printf hello".to_string()],
                    environment: BTreeMap::new(),
                },
                sender,
            )
            .expect("the PTY should spawn");

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut output = String::new();
        let mut exit_code = None;
        while Instant::now() < deadline && exit_code.is_none() {
            match receiver.blocking_recv() {
                Some(TerminalEvent::Output(bytes)) => {
                    output.push_str(&String::from_utf8_lossy(&bytes));
                }
                Some(TerminalEvent::Exited {
                    exit_code: code, ..
                }) => exit_code = Some(code),
                None => break,
            }
        }

        assert!(output.contains("hello"), "output was {output:?}");
        assert_eq!(exit_code, Some(Some(0)));
    }

    #[tokio::test]
    async fn a_slow_reader_gets_all_output_before_exit() -> Result<(), Box<dyn std::error::Error>> {
        let mut engine = PtyTerminals::new();
        let (sender, mut receiver) = mpsc::channel(1);
        engine.spawn(
            TerminalSpec {
                id: TerminalId::new("burst-terminal"),
                columns: 80,
                rows: 24,
                cwd: std::env::temp_dir(),
                command: "/bin/sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    "head -c 1048576 /dev/zero; printf tail".to_string(),
                ],
                environment: BTreeMap::new(),
            },
            sender,
        )?;

        tokio::time::sleep(Duration::from_millis(200)).await;
        let (output, status) = tokio::time::timeout(Duration::from_secs(10), async {
            let mut output = Vec::new();
            while let Some(event) = receiver.recv().await {
                match event {
                    TerminalEvent::Output(bytes) => output.extend(bytes),
                    TerminalEvent::Exited { exit_code, .. } => return (output, exit_code),
                }
            }
            (output, None)
        })
        .await?;
        assert_eq!(output.len(), 1_048_580);
        assert!(output.ends_with(b"tail"));
        assert_eq!(status, Some(0));
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn closing_a_live_terminal_stops_a_job_that_ignores_hangup()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut engine = PtyTerminals::new();
        let (sender, mut receiver) = mpsc::channel(8);
        let terminal_id = engine.spawn(
            TerminalSpec {
                id: TerminalId::new("test-terminal"),
                columns: 80,
                rows: 24,
                cwd: std::env::temp_dir(),
                command: "/bin/sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    "trap '' HUP; printf ready; sleep 10".to_string(),
                ],
                environment: BTreeMap::new(),
            },
            sender,
        )?;
        let ready = tokio::time::timeout(Duration::from_secs(3), receiver.recv()).await?;
        assert!(matches!(ready, Some(TerminalEvent::Output(bytes)) if bytes == b"ready"));

        let session_id = engine
            .sessions()?
            .live
            .get(terminal_id.as_str())
            .and_then(|session| session.session_id)
            .ok_or("missing terminal session")?;
        let mut groups = TerminalProcessGroup::for_session(session_id, terminal_processes());
        assert!(!groups.is_empty());
        assert!(groups.iter().any(TerminalProcessGroup::still_owned));
        for group in &mut groups {
            for member in &mut group.members {
                member.started_at = member.started_at.wrapping_add(1);
            }
            assert!(!group.still_owned());
        }

        let failure = engine.close_with_scan(
            &terminal_id,
            || Err(std::io::Error::other("fixture process scan failed")),
            TerminalProcessGroup::signal,
        );
        assert!(failure.is_err());
        assert!(
            engine
                .sessions()?
                .live
                .get(terminal_id.as_str())
                .is_some_and(|session| !session.closing)
        );
        let failure = engine.close_with_scan(&terminal_id, list_processes, |_, _| {
            Err(std::io::Error::other("fixture signal failure"))
        });
        assert!(failure.is_err());
        assert!(
            engine
                .sessions()?
                .live
                .get(terminal_id.as_str())
                .is_some_and(|session| !session.closing)
        );
        engine.close(&terminal_id)?;
        assert!(
            engine.write(&terminal_id, b"x".to_vec()).is_err(),
            "a closed terminal keeps no writer"
        );
        let stopped = tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(event) = receiver.recv().await {
                if matches!(event, TerminalEvent::Exited { .. }) {
                    return true;
                }
            }
            false
        })
        .await?;
        assert!(stopped, "the terminal should report its exit");
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn closing_a_terminal_stops_a_separate_background_process_group()
    -> Result<(), Box<dyn std::error::Error>> {
        check_background_job_shutdown(false).await
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn quitting_stops_a_background_job_after_its_shell_exits()
    -> Result<(), Box<dyn std::error::Error>> {
        check_background_job_shutdown(true).await
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    async fn check_background_job_shutdown(
        exit_before_quit: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut engine = PtyTerminals::new();
        let (sender, mut receiver) = mpsc::channel(8);
        let terminal_id = engine.spawn(
            TerminalSpec {
                id: TerminalId::new("background-terminal"),
                columns: 80,
                rows: 24,
                cwd: std::env::temp_dir(),
                command: "/bin/sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    r#"python3 -u -c 'import os, signal, time; os.setpgid(0, 0); signal.signal(signal.SIGHUP, signal.SIG_IGN); print(f"background:{os.getpid()}"); time.sleep(10)' & IFS= read -r line; exit 23"#.into(),
                ],
                environment: BTreeMap::new(),
            },
            sender,
        )?;
        let process = tokio::time::timeout(Duration::from_secs(3), async {
            let mut output = String::new();
            while let Some(TerminalEvent::Output(bytes)) = receiver.recv().await {
                output.push_str(&String::from_utf8_lossy(&bytes));
                if let Some(process) = output.lines().find_map(|line| {
                    line.strip_prefix("background:")?
                        .parse::<libc::pid_t>()
                        .ok()
                }) {
                    return Some(process);
                }
            }
            None
        })
        .await?
        .ok_or("the background job did not start")?;
        let session = unsafe { libc::getsid(process) };
        let group = unsafe { libc::getpgid(process) };
        assert!(
            session > 1 && group > 1 && session != group,
            "session {session}, group {group}"
        );

        if exit_before_quit {
            engine.write(&terminal_id, b"leave\r".to_vec())?.await??;
            let exit_code = tokio::time::timeout(Duration::from_secs(3), async {
                while let Some(event) = receiver.recv().await {
                    if let TerminalEvent::Exited { exit_code, .. } = event {
                        return exit_code;
                    }
                }
                None
            })
            .await?;
            assert_eq!(exit_code, Some(23));
            assert_eq!(unsafe { libc::getsid(process) }, session);
            let records = engine.lingering_process_records(&terminal_id)?;
            assert!(
                records
                    .iter()
                    .any(|record| i64::from(record.pid) == i64::from(process)
                        && i64::from(record.process_group) == i64::from(group))
            );
            assert!(
                engine
                    .terminate_all_with_scan(|| Err(std::io::Error::other("fixture scan failed")))
                    .is_err()
            );
            assert!(!engine.lingering_process_records(&terminal_id)?.is_empty());
            let failure = engine.close_with_scan(&terminal_id, list_processes, |group, signal| {
                if signal == libc::SIGKILL {
                    Err(std::io::Error::other("fixture kill failure"))
                } else {
                    group.signal(signal)
                }
            });
            assert!(failure.is_err());
            assert!(!engine.lingering_process_records(&terminal_id)?.is_empty());
            engine.terminate_all()?;
        } else {
            engine.close(&terminal_id)?;
        }
        let stopped = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let status = std::process::Command::new("ps")
                    .args(["-p", &process.to_string(), "-o", "stat="])
                    .output()?;
                let state = String::from_utf8(status.stdout)?;
                if state.trim().is_empty() || state.trim_start().starts_with('Z') {
                    return Ok::<(), Box<dyn std::error::Error>>(());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        if !matches!(&stopped, Ok(Ok(())))
            && unsafe { libc::getsid(process) == session && libc::getpgid(process) == group }
            && unsafe { libc::kill(process, libc::SIGKILL) } == -1
        {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error.into());
            }
        }
        stopped??;
        Ok(())
    }
}
