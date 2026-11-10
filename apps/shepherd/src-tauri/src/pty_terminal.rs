//! The `portable-pty` implementation of the core's terminal boundary.
//!
//! One reader thread and one waiter thread per PTY; both report through the same
//! channel, so the shell sees output and the exit in order.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use shepherd_core::terminal::{
    ProcessRecord, TerminalError, TerminalEvent, TerminalId, TerminalService, TerminalSpec,
    TerminalWrite, validate_size,
};
use tokio::sync::{mpsc, oneshot};

const MAX_WRITE_BYTES: usize = 64 * 1024;

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

#[derive(Default)]
pub struct PtyTerminals {
    sessions: Arc<Mutex<TerminalSessions>>,
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
        if let Err(error) = self.terminate_all() {
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

impl TerminalService for PtyTerminals {
    fn spawn(
        &mut self,
        spec: TerminalSpec,
        events: mpsc::Sender<TerminalEvent>,
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

        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| TerminalError::Backend(format!("spawn failed: {error}")))?;
        drop(pair.slave);

        let (reader_finished, reader_status) = std::sync::mpsc::sync_channel(1);
        let output_events = events.clone();
        let sending = Arc::new(AtomicBool::new(false));
        let reader_sending = Arc::clone(&sending);
        std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        reader_sending.store(true, Ordering::Release);
                        let sent = output_events
                            .blocking_send(TerminalEvent::Output(buffer[..read].to_vec()));
                        reader_sending.store(false, Ordering::Release);
                        if sent.is_err() {
                            break;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => {
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
        std::thread::spawn(move || {
            while let Some(request) = writes.blocking_recv() {
                let result = writer
                    .write_all(&request.bytes)
                    .and_then(|()| writer.flush())
                    .map_err(|error| TerminalError::Backend(format!("write failed: {error}")));
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
            },
        );

        let sessions = Arc::clone(&self.sessions);
        let id = terminal_id.as_str().to_string();
        std::thread::spawn(move || {
            let (exit_code, signal) = match child.wait() {
                Ok(status) => (
                    Some(status.exit_code() as i32),
                    status.signal().map(str::to_owned),
                ),
                Err(error) => {
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
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        // A full queue still has output ahead of the exit. Only time out
                        // a reader waiting for descendants that inherited the slave.
                        if !sending.load(Ordering::Acquire) || events.is_closed() {
                            tracing::debug!("terminal reader did not reach EOF before exit");
                            break;
                        }
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
        session
            .master
            .resize(Self::size(columns, rows))
            .map_err(|error| TerminalError::Backend(format!("resize failed: {error}")))
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

    use super::*;

    #[tokio::test]
    async fn fake_cli_pty_stream_can_be_recorded_with_screen_states()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::terminal_screen::Screen;
        use agent_harness::freebuff_capture::{
            CaptureEvent, CaptureSession, CaptureWriter, read_capture,
        };

        let directory = tempfile::tempdir()?;
        let session = CaptureSession::new(CaptureWriter::create(directory.path(), "fake-pty")?);
        let mut screen = Screen::new(80, 24);
        let mut engine = PtyTerminals::new();
        let (sender, mut receiver) = mpsc::channel(16);
        let terminal_id = engine.spawn(
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
        )?;
        let mut output = Vec::new();
        let mut submitted = false;
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(event) = receiver.recv().await {
                match event {
                    TerminalEvent::Output(bytes) => {
                        session
                            .record_and_apply(
                                CaptureEvent::Output {
                                    bytes: bytes.clone(),
                                },
                                || {
                                    screen.process(&bytes);
                                    let state =
                                        serde_json::to_value(screen.frame()).map_err(|error| {
                                            agent_harness::HarnessError::Protocol(error.to_string())
                                        })?;
                                    Ok(((), vec![CaptureEvent::Screen { state }]))
                                },
                            )
                            .map_err(|error| error.to_string())?;
                        output.extend(bytes);
                        if !submitted && output.windows(6).any(|bytes| bytes == b"ready>") {
                            let input = b"hello\n".to_vec();
                            let completion = session
                                .record_and_apply(
                                    CaptureEvent::Input {
                                        bytes: input.clone(),
                                    },
                                    || {
                                        let completion =
                                            engine.write(&terminal_id, input).map_err(|error| {
                                                agent_harness::HarnessError::Protocol(
                                                    error.to_string(),
                                                )
                                            })?;
                                        Ok((completion, Vec::new()))
                                    },
                                )
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
        assert!(output.windows(11).any(|bytes| bytes == b"reply:hello"));
        let records = read_capture(
            &directory
                .path()
                .join(".cache/shepherd-live/freebuff/fake-pty.jsonl"),
        )?;
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
