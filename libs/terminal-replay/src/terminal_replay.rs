use terminal_renderer::{Screen, TerminalFrame};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreebuffScreenWarning {
    Disconnected,
    LauncherRestart,
}

#[derive(Default)]
pub struct FreebuffWarningTracker {
    warning: Option<FreebuffScreenWarning>,
}

impl FreebuffWarningTracker {
    pub fn observe(&mut self, frame: &TerminalFrame) -> agent_harness::HarnessResult<()> {
        if let Some(warning) = freebuff_screen_warning(frame)? {
            self.warning = Some(warning);
        }
        Ok(())
    }

    pub fn warning(&self) -> Option<FreebuffScreenWarning> {
        self.warning
    }
}

/// These observations can invalidate an interaction; their absence never
/// establishes readiness, compatibility, or a successful reconnection.
pub fn freebuff_screen_warning(
    frame: &TerminalFrame,
) -> agent_harness::HarnessResult<Option<FreebuffScreenWarning>> {
    use agent_harness::HarnessError;
    if frame.columns == 0
        || frame.rows == 0
        || frame.columns.checked_mul(frame.rows) != Some(frame.cells.len())
    {
        return Err(HarnessError::Protocol(
            "invalid Freebuff terminal frame".into(),
        ));
    }
    for cells in frame.cells.chunks(frame.columns) {
        let row: String = cells.iter().map(|cell| cell.text.as_str()).collect();
        let row = row.trim();
        if row == "Can't reach Freebuff's servers. Retrying… Try another network." {
            return Ok(Some(FreebuffScreenWarning::Disconnected));
        }
        if row.starts_with("Update available: ") && row.contains(" → ")
            || row.starts_with("Update failed; restarting freebuff ")
        {
            return Ok(Some(FreebuffScreenWarning::LauncherRestart));
        }
    }
    Ok(None)
}

#[allow(dead_code)]
pub struct RecordedScreenReplayer {
    verifier: Screen,
    replay: Screen,
    previous: TerminalFrame,
    failed: bool,
}

#[allow(dead_code)]
impl RecordedScreenReplayer {
    pub fn new(columns: u16, rows: u16) -> agent_harness::HarnessResult<Self> {
        shepherd_core::terminal::validate_size(columns, rows).map_err(|error| {
            agent_harness::HarnessError::Protocol(format!("invalid terminal size: {error}"))
        })?;
        let verifier = Screen::with_scrollback(usize::from(columns), usize::from(rows), 0);
        let replay = Screen::with_scrollback(usize::from(columns), usize::from(rows), 0);
        let previous = replay.frame();
        Ok(Self {
            verifier,
            replay,
            previous,
            failed: false,
        })
    }

    pub fn apply(
        &mut self,
        update: &terminal_renderer::RecordedScreenUpdate,
        mut visit: impl FnMut(&TerminalFrame) -> agent_harness::HarnessResult<()>,
    ) -> agent_harness::HarnessResult<()> {
        use agent_harness::HarnessError;
        use terminal_renderer::RecordedScreenUpdate;
        if self.failed {
            return Err(HarnessError::Protocol(
                "terminal screen replay has failed".into(),
            ));
        }
        self.failed = true;
        match update {
            RecordedScreenUpdate::Output { bytes, state } => {
                self.verifier.process(bytes);
                Self::verify_frame(&self.verifier, state)?;
                // A read may scroll earlier output away before its final frame.
                for byte in bytes {
                    self.replay.process(std::slice::from_ref(byte));
                    let frame = self.replay.frame();
                    if frame != self.previous {
                        visit(&frame)?;
                        self.previous = frame;
                    }
                }
                Self::verify_frame(&self.replay, state)?;
            }
            RecordedScreenUpdate::Resize {
                columns,
                rows,
                state,
            } => {
                shepherd_core::terminal::validate_size(*columns, *rows).map_err(|error| {
                    HarnessError::Protocol(format!("invalid terminal size: {error}"))
                })?;
                self.verifier
                    .resize(usize::from(*columns), usize::from(*rows));
                Self::verify_frame(&self.verifier, state)?;
                self.replay
                    .resize(usize::from(*columns), usize::from(*rows));
                let frame = self.replay.frame();
                if frame != self.previous {
                    visit(&frame)?;
                    self.previous = frame;
                }
                Self::verify_frame(&self.replay, state)?;
            }
        }
        self.failed = false;
        Ok(())
    }

    fn verify_frame(
        screen: &Screen,
        expected: &serde_json::Value,
    ) -> agent_harness::HarnessResult<()> {
        let actual = serde_json::to_value(screen.frame()).map_err(|error| {
            agent_harness::HarnessError::Protocol(format!(
                "could not encode terminal screen: {error}"
            ))
        })?;
        if &actual != expected {
            return Err(agent_harness::HarnessError::Protocol(
                "captured terminal screen does not match replayed output".into(),
            ));
        }
        Ok(())
    }
}

#[allow(dead_code)]
pub fn verify_recorded_screens(path: &std::path::Path) -> agent_harness::HarnessResult<()> {
    visit_verified_screens(path, |_, _| Ok(()))
}

/// Replay a complete capture in order. Every recorded screen frame is
/// reconstructed and verified before `visit` sees it, and worktree effects are
/// applied only after all frames and the visitor succeed.
#[cfg(unix)]
#[allow(dead_code)]
pub fn replay_freebuff_capture(
    capture_path: &std::path::Path,
    worktree: &std::path::Path,
    visit: impl FnMut(u64, TerminalFrame) -> agent_harness::HarnessResult<()>,
) -> agent_harness::HarnessResult<()> {
    visit_verified_screens(capture_path, visit)?;
    shepherd_core::harness::replay_worktree_effects(worktree, capture_path).map_err(|error| {
        agent_harness::HarnessError::Protocol(format!(
            "could not replay captured worktree effects: {error}"
        ))
    })
}

/// The visitor must defer external effects until replay succeeds.
#[allow(dead_code)]
pub fn visit_verified_screens(
    path: &std::path::Path,
    visit: impl FnMut(u64, TerminalFrame) -> agent_harness::HarnessResult<()>,
) -> agent_harness::HarnessResult<()> {
    verify_screens(path, true, visit)
}

#[allow(dead_code)]
pub fn verify_unfinished_screens(path: &std::path::Path) -> agent_harness::HarnessResult<()> {
    visit_unfinished_screens(path, |_, _| Ok(()))
}

/// Inspect verified frames from an incomplete capture without applying effects.
pub fn visit_unfinished_screens(
    path: &std::path::Path,
    visit: impl FnMut(u64, TerminalFrame) -> agent_harness::HarnessResult<()>,
) -> agent_harness::HarnessResult<()> {
    verify_screens(path, false, visit)
}

/// Inspect intermediate frames from an incomplete capture without applying effects.
pub fn visit_unfinished_screen_changes(
    path: &std::path::Path,
    visit: impl FnMut(u64, &TerminalFrame) -> agent_harness::HarnessResult<()>,
) -> agent_harness::HarnessResult<()> {
    use agent_harness::freebuff_capture::{CaptureEvent, visit_capture};
    use agent_harness::{HarnessError, HarnessResult};
    use terminal_renderer::RecordedScreenUpdate;

    enum Pending {
        Output(Vec<u8>),
        Resize(u16, u16),
    }

    let mut replayer = None::<RecordedScreenReplayer>;
    let mut pending = None::<Pending>;
    let mut visit = visit;
    visit_capture(path, |record| -> HarnessResult<()> {
        if pending.is_some() && !matches!(&record.event, CaptureEvent::Screen { .. }) {
            return Err(HarnessError::Protocol(
                "terminal capture is missing a screen frame".into(),
            ));
        }
        match record.event {
            CaptureEvent::Resize { columns, rows } if replayer.is_none() => {
                replayer = Some(RecordedScreenReplayer::new(columns, rows)?);
            }
            CaptureEvent::Resize { columns, rows } => {
                pending = Some(Pending::Resize(columns, rows));
            }
            CaptureEvent::Output { bytes } if replayer.is_some() => {
                pending = Some(Pending::Output(bytes));
            }
            CaptureEvent::Output { .. } => {
                return Err(HarnessError::Protocol(
                    "terminal output has no initial size".into(),
                ));
            }
            CaptureEvent::Screen { state } => {
                let update = match pending.take() {
                    Some(Pending::Output(bytes)) => RecordedScreenUpdate::Output { bytes, state },
                    Some(Pending::Resize(columns, rows)) => RecordedScreenUpdate::Resize {
                        columns,
                        rows,
                        state,
                    },
                    None => {
                        return Err(HarnessError::Protocol(
                            "unexpected terminal screen frame".into(),
                        ));
                    }
                };
                replayer
                    .as_mut()
                    .ok_or_else(|| {
                        HarnessError::Protocol("terminal screen has no initial size".into())
                    })?
                    .apply(&update, |frame| visit(record.elapsed_micros, frame))?;
            }
            CaptureEvent::Complete => {
                return Err(HarnessError::Protocol(
                    "terminal capture was already completed".into(),
                ));
            }
            _ => {}
        }
        Ok(())
    })?;
    if pending.is_some() {
        return Err(HarnessError::Protocol(
            "terminal capture is missing a screen frame".into(),
        ));
    }
    if replayer.is_none() {
        return Err(HarnessError::Protocol(
            "terminal capture has no initial size".into(),
        ));
    }
    Ok(())
}

fn verify_screens(
    path: &std::path::Path,
    complete: bool,
    mut visit: impl FnMut(u64, TerminalFrame) -> agent_harness::HarnessResult<()>,
) -> agent_harness::HarnessResult<()> {
    use agent_harness::HarnessError;
    use agent_harness::freebuff_capture::{CaptureEvent, visit_capture, visit_complete_capture};

    let mut screen: Option<Screen> = None;
    let mut awaiting_screen = false;
    let mut verify = |record: agent_harness::freebuff_capture::CaptureRecord| {
        if awaiting_screen && !matches!(&record.event, CaptureEvent::Screen { .. }) {
            return Err(HarnessError::Protocol(
                "terminal capture is missing a screen frame".into(),
            ));
        }
        match record.event {
            CaptureEvent::Resize { columns, rows } => {
                shepherd_core::terminal::validate_size(columns, rows).map_err(|error| {
                    HarnessError::Protocol(format!("invalid captured terminal size: {error}"))
                })?;
                if let Some(screen) = &mut screen {
                    screen.resize(usize::from(columns), usize::from(rows));
                    awaiting_screen = true;
                } else {
                    screen = Some(Screen::with_scrollback(
                        usize::from(columns),
                        usize::from(rows),
                        0,
                    ));
                }
            }
            CaptureEvent::Output { bytes } => {
                screen
                    .as_mut()
                    .ok_or_else(|| {
                        HarnessError::Protocol("terminal output precedes its initial size".into())
                    })?
                    .process(&bytes);
                awaiting_screen = true;
            }
            CaptureEvent::Screen { state } => {
                if !awaiting_screen {
                    return Err(HarnessError::Protocol(
                        "unexpected terminal screen frame".into(),
                    ));
                }
                let frame = screen
                    .as_ref()
                    .ok_or_else(|| {
                        HarnessError::Protocol("terminal screen has no initial size".into())
                    })?
                    .frame();
                let expected = serde_json::to_value(&frame).map_err(|error| {
                    HarnessError::Protocol(format!(
                        "could not encode replayed terminal screen: {error}"
                    ))
                })?;
                if state != expected {
                    return Err(HarnessError::Protocol(
                        "captured terminal screen does not match replayed output".into(),
                    ));
                }
                awaiting_screen = false;
                visit(record.elapsed_micros, frame)?;
            }
            CaptureEvent::Complete if !complete => {
                return Err(HarnessError::Protocol(
                    "terminal capture was already completed".into(),
                ));
            }
            _ => {}
        }
        Ok(())
    };
    if complete {
        visit_complete_capture(path, &mut verify)?;
    } else {
        visit_capture(path, &mut verify)?;
    }
    if awaiting_screen {
        return Err(HarnessError::Protocol(
            "terminal capture is missing a screen frame".into(),
        ));
    }
    if screen.is_none() {
        return Err(HarnessError::Protocol(
            "terminal capture has no initial size".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_harness::freebuff_capture::{
        CaptureEvent, CaptureRecord, CaptureWriter, visit_capture,
    };

    #[test]
    fn freebuff_warnings_come_from_rendered_rows_and_never_imply_readiness()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut screen = Screen::with_scrollback(100, 3, 0);
        let mut warnings = FreebuffWarningTracker::default();
        screen.process(b"Can't reach Freebuff's servers. Retrying");
        screen.process("… Try another network.".as_bytes());
        assert_eq!(
            freebuff_screen_warning(&screen.frame())?,
            Some(FreebuffScreenWarning::Disconnected)
        );
        warnings.observe(&screen.frame())?;
        screen.process(b"\x1b[2J\x1b[HReconnected");
        assert_eq!(freebuff_screen_warning(&screen.frame())?, None);
        warnings.observe(&screen.frame())?;
        assert_eq!(
            warnings.warning(),
            Some(FreebuffScreenWarning::Disconnected)
        );
        screen.process(b"\x1b[2J\x1b[HUpdate available: 0.2.36 ");
        screen.process("→ 0.2.37".as_bytes());
        assert_eq!(
            freebuff_screen_warning(&screen.frame())?,
            Some(FreebuffScreenWarning::LauncherRestart)
        );
        warnings.observe(&screen.frame())?;
        assert_eq!(
            warnings.warning(),
            Some(FreebuffScreenWarning::LauncherRestart)
        );
        Ok(())
    }

    #[test]
    fn unfinished_capture_exposes_warnings_erased_within_one_read()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let mut writer = CaptureWriter::create(directory.path(), "transient-warning")?;
        let bytes = "Can't reach Freebuff's servers. Retrying… Try another network.\x1b[2J\x1b[HReconnected".as_bytes().to_vec();
        let mut screen = Screen::with_scrollback(100, 3, 0);
        screen.process(&bytes);
        for (elapsed_micros, event) in [
            CaptureEvent::Resize {
                columns: 100,
                rows: 3,
            },
            CaptureEvent::Output { bytes },
            CaptureEvent::Screen {
                state: serde_json::to_value(screen.frame())?,
            },
        ]
        .into_iter()
        .enumerate()
        {
            writer.append(&CaptureRecord {
                elapsed_micros: elapsed_micros as u64,
                event,
            })?;
        }
        let path = directory
            .path()
            .join(".cache/shepherd-live/freebuff/transient-warning.jsonl");
        assert_eq!(freebuff_screen_warning(&screen.frame())?, None);
        let mut warnings = FreebuffWarningTracker::default();
        visit_unfinished_screen_changes(&path, |_, frame| warnings.observe(frame))?;
        assert_eq!(
            warnings.warning(),
            Some(FreebuffScreenWarning::Disconnected)
        );
        writer.append(&CaptureRecord {
            elapsed_micros: 100,
            event: CaptureEvent::Complete,
        })?;
        assert!(visit_unfinished_screen_changes(&path, |_, _| Ok(())).is_err());
        Ok(())
    }

    #[test]
    fn recorded_read_replays_text_that_scrolls_out_before_its_final_frame()
    -> Result<(), Box<dyn std::error::Error>> {
        use terminal_renderer::RecordedScreenUpdate;
        let bytes = b"first\r\nsecond\r\nthird".to_vec();
        let mut source = Screen::with_scrollback(10, 2, 0);
        source.process(&bytes);
        let state = serde_json::to_value(source.frame())?;
        let mut replay = RecordedScreenReplayer::new(10, 2)?;
        let mut saw_first = false;
        replay.apply(
            &RecordedScreenUpdate::Output {
                bytes: bytes.clone(),
                state: state.clone(),
            },
            |frame| {
                saw_first |= row(frame, 0).contains("first");
                Ok(())
            },
        )?;
        assert!(saw_first);
        assert!(!row(&source.frame(), 0).contains("first"));

        let mut rejected = RecordedScreenReplayer::new(10, 2)?;
        let mut invalid = state;
        invalid["columns"] = serde_json::json!(9);
        let mut called = false;
        assert!(
            rejected
                .apply(
                    &RecordedScreenUpdate::Output {
                        bytes,
                        state: invalid
                    },
                    |_| {
                        called = true;
                        Ok(())
                    }
                )
                .is_err()
        );
        assert!(!called);
        Ok(())
    }

    #[test]
    fn recorded_screens_must_match_replayed_terminal_output()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        for (name, tampered) in [("matching", false), ("tampered", true)] {
            let mut writer = CaptureWriter::create(directory.path(), name)?;
            let mut screen = Screen::with_scrollback(10, 2, 0);
            let mut events = vec![CaptureEvent::Resize {
                columns: 10,
                rows: 2,
            }];
            for bytes in [b"hello".as_slice(), b"\rworld".as_slice()] {
                screen.process(bytes);
                events.push(CaptureEvent::Output {
                    bytes: bytes.to_vec(),
                });
                let mut state = serde_json::to_value(screen.frame())?;
                if tampered {
                    state["cursorColumn"] = serde_json::json!(0);
                }
                events.push(CaptureEvent::Screen { state });
            }
            for (elapsed_micros, event) in events.into_iter().enumerate() {
                writer.append(&CaptureRecord {
                    elapsed_micros: elapsed_micros as u64,
                    event,
                })?;
            }
            let path = directory
                .path()
                .join(format!(".cache/shepherd-live/freebuff/{name}.jsonl"));
            assert_eq!(verify_unfinished_screens(&path).is_err(), tampered);
            writer.append(&CaptureRecord {
                elapsed_micros: 100,
                event: CaptureEvent::Complete,
            })?;
            assert_eq!(verify_recorded_screens(&path).is_err(), tampered);
            assert!(verify_unfinished_screens(&path).is_err());
            if !tampered {
                let mut replayed = Vec::new();
                visit_verified_screens(&path, |elapsed_micros, frame| {
                    replayed.push((elapsed_micros, row(&frame, 0)));
                    Ok(())
                })?;
                let [(first_time, first), (second_time, second)] = replayed.as_slice() else {
                    return Err("expected two replayed frames".into());
                };
                assert_eq!((*first_time, first.trim_end()), (2, "hello"));
                assert_eq!((*second_time, second.trim_end()), (4, "world"));
            }
        }
        Ok(())
    }

    #[test]
    fn freebuff_capture_replays_split_terminal_output_and_resize()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let mut writer = CaptureWriter::create(directory.path(), "terminal-replay")?;
        for (elapsed_micros, event) in [
            (
                0,
                CaptureEvent::Output {
                    bytes: b"hello\x1b[".to_vec(),
                },
            ),
            (
                1,
                CaptureEvent::Output {
                    bytes: b"2K\rworld".to_vec(),
                },
            ),
            (
                2,
                CaptureEvent::Resize {
                    columns: 12,
                    rows: 3,
                },
            ),
            (
                3,
                CaptureEvent::Output {
                    bytes: b"!".to_vec(),
                },
            ),
        ] {
            writer.append(&CaptureRecord {
                elapsed_micros,
                event,
            })?;
        }

        let path = directory
            .path()
            .join(".cache/shepherd-live/freebuff/terminal-replay.jsonl");
        let mut screen = Screen::new(10, 2);
        visit_capture(&path, |record| {
            match record.event {
                CaptureEvent::Output { bytes } => screen.process(&bytes),
                CaptureEvent::Resize { columns, rows } => {
                    screen.resize(usize::from(columns), usize::from(rows));
                }
                _ => {}
            }
            Ok(())
        })?;
        let frame = screen.frame();
        assert_eq!((frame.columns, frame.rows), (12, 3));
        assert_eq!(row(&frame, 0).trim_end(), "world!");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn verified_replay_streams_screens_before_applying_worktree_effects()
    -> Result<(), Box<dyn std::error::Error>> {
        use agent_harness::freebuff_capture::{CaptureEvent, read_complete_capture};
        use shepherd_core::harness::FreebuffLiveCapture;
        use shepherd_core::test_support::TestRepository;

        fn record(
            cache: &std::path::Path,
            repository: &TestRepository,
            scenario: &str,
            tamper: bool,
        ) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
            let capture =
                FreebuffLiveCapture::begin(cache, repository.path().to_path_buf(), scenario)?;
            capture.session().record_and_apply(
                CaptureEvent::Resize {
                    columns: 10,
                    rows: 2,
                },
                || Ok(((), Vec::new())),
            )?;
            let bytes = b"hello\r\nworld".to_vec();
            let mut screen = Screen::with_scrollback(10, 2, 0);
            screen.process(&bytes);
            let mut state = serde_json::to_value(screen.frame())?;
            if tamper {
                state["columns"] = serde_json::json!(9);
            }
            capture
                .session()
                .record_and_apply(CaptureEvent::Output { bytes }, || {
                    Ok(((), vec![CaptureEvent::Screen { state }]))
                })?;
            repository.write_file("README.md", "replayed\n")?;
            capture.finish_after_drain()?;
            Ok(cache.join(format!(".cache/shepherd-live/freebuff/{scenario}.jsonl")))
        }

        let repository = TestRepository::new()?;
        let cache = tempfile::tempdir()?;
        let path = record(cache.path(), &repository, "terminal-replay-v1", false)?;
        let records = read_complete_capture(&path)?;
        assert!(
            records
                .iter()
                .any(|record| matches!(record.event, CaptureEvent::Screen { .. }))
        );

        repository.write_file("README.md", "initial\n")?;
        let mut frames = Vec::new();
        replay_freebuff_capture(&path, repository.path(), |elapsed_micros, frame| {
            frames.push((elapsed_micros, row(&frame, 1).trim_end().to_string()));
            Ok(())
        })?;
        assert_eq!(frames.len(), 1, "one recorded screen is replayed once");
        assert_eq!(frames.first().map(|(_, row)| row.as_str()), Some("world"));
        assert_eq!(
            std::fs::read_to_string(repository.path().join("README.md"))?,
            "replayed\n"
        );

        repository.write_file("README.md", "initial\n")?;
        let tampered = record(
            cache.path(),
            &repository,
            "terminal-replay-v1-tampered",
            true,
        )?;
        repository.write_file("README.md", "initial\n")?;
        let mut visited = false;
        assert!(
            replay_freebuff_capture(&tampered, repository.path(), |_, _| {
                visited = true;
                Ok(())
            })
            .is_err()
        );
        assert!(!visited, "a tampered screen must not reach the consumer");
        assert_eq!(
            std::fs::read_to_string(repository.path().join("README.md"))?,
            "initial\n",
            "worktree effects must not be applied before screen verification"
        );

        let stopped = record(
            cache.path(),
            &repository,
            "terminal-replay-v1-stopped",
            false,
        )?;
        repository.write_file("README.md", "initial\n")?;
        assert!(
            replay_freebuff_capture(&stopped, repository.path(), |_, _| {
                Err(agent_harness::HarnessError::Protocol(
                    "consumer stopped".into(),
                ))
            })
            .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(repository.path().join("README.md"))?,
            "initial\n",
            "a consumer error must defer worktree effects"
        );
        Ok(())
    }

    fn row(frame: &TerminalFrame, row: usize) -> String {
        let start = row * frame.columns;
        frame.cells[start..start + frame.columns]
            .iter()
            .map(|cell| cell.text.as_str())
            .collect()
    }
}
