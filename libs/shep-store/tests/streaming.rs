#![allow(clippy::too_many_lines)]

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use shep_harness_core::{
    AgentHarness, ApprovalId, Capabilities, Decision, DeltaKind, HarnessEvent, HarnessResult,
    ItemId, PromptInput, SessionApproval, SessionId, SpawnSpec,
};
use shep_harness_core::{Entry, TurnFinishReason};
use tokio::sync::mpsc;

use shep_core::harness::HarnessFactory;
use shep_core::harness::HarnessSession;
use shep_core::store::Store;
use shep_core::test_support::{TestDatabase, TestRepository};
use shep_core::{
    Agent, Command, CommandResult, CoreError, Event, Project, ProjectId, Task, TaskState,
};
use shep_core::{Core, TaskId};
use shep_store::TursoStore;
use shep_store::test_support::{LateHarness, LateHarnessFactory};

#[tokio::test]
async fn agent_event_wakeups_cover_multiple_sessions_and_buffered_events()
-> Result<(), Box<dyn std::error::Error>> {
    let mut core = Core::new();
    let mut senders = Vec::new();
    let mut ids = Vec::new();
    for index in 0..3 {
        let (sender, events) = mpsc::unbounded_channel();
        let id = TaskId::new(format!("task-{index}"));
        core.sessions.insert(
            id.clone(),
            HarnessSession::new(Box::new(LateHarness {
                events,
                approvals: Vec::new(),
                prompts: None,
                mode: None,
            })),
        );
        senders.push(sender);
        ids.push(id);
    }
    let [first, middle, last] = senders.as_slice() else {
        return Err("expected three senders".into());
    };
    let [first_id, middle_id, last_id] = ids.as_slice() else {
        return Err("expected three sessions".into());
    };
    let notify = Arc::new(tokio::sync::Notify::new());
    core.arm_agent_events(&notify);
    middle.send(HarnessEvent::Delta {
        item_id: ItemId::new("middle"),
        kind: DeltaKind::Message,
        text: "first".into(),
        stream_permit: None,
    })?;
    tokio::time::timeout(Duration::from_secs(1), notify.notified()).await?;
    let events = core
        .sessions
        .get_mut(middle_id)
        .ok_or("missing session")?
        .pump();
    assert_eq!(events.len(), 1);
    first.send(HarnessEvent::Delta {
        item_id: ItemId::new("first"),
        kind: DeltaKind::Message,
        text: "buffered".into(),
        stream_permit: None,
    })?;
    core.arm_agent_events(&notify);
    tokio::time::timeout(Duration::from_secs(1), notify.notified()).await?;
    assert_eq!(
        core.sessions
            .get_mut(first_id)
            .ok_or("missing session")?
            .pump()
            .len(),
        1
    );
    core.arm_agent_events(&notify);
    last.send(HarnessEvent::Delta {
        item_id: ItemId::new("last"),
        kind: DeltaKind::Message,
        text: "last".into(),
        stream_permit: None,
    })?;
    tokio::time::timeout(Duration::from_secs(1), notify.notified()).await?;
    assert_eq!(
        core.sessions
            .get_mut(last_id)
            .ok_or("missing session")?
            .pump()
            .len(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn queued_stream_events_hold_their_capacity_until_folded()
-> Result<(), Box<dyn std::error::Error>> {
    let capacity = Arc::new(tokio::sync::Semaphore::new(1));
    let permit = capacity.clone().acquire_owned().await?;
    let (sender, events) = mpsc::unbounded_channel();
    let id = TaskId::new("capacity");
    let mut core = Core::new();
    core.sessions.insert(
        id.clone(),
        HarnessSession::new(Box::new(LateHarness {
            events,
            approvals: Vec::new(),
            prompts: None,
            mode: None,
        })),
    );
    sender.send(HarnessEvent::Delta {
        item_id: ItemId::new("held"),
        kind: DeltaKind::Message,
        text: "held".into(),
        stream_permit: Some(Arc::new(permit)),
    })?;
    assert_eq!(capacity.available_permits(), 0);
    let mut drained = core.sessions.get_mut(&id).ok_or("missing session")?.pump();
    assert_eq!(drained.len(), 1);
    assert_eq!(
        capacity.available_permits(),
        0,
        "a drained event must keep its capacity until it is folded"
    );
    drained.clear();
    assert_eq!(
        capacity.available_permits(),
        1,
        "folding an event must release its capacity"
    );
    Ok(())
}

#[tokio::test]
async fn background_batches_are_fair_and_preserve_all_agent_events()
-> Result<(), Box<dyn std::error::Error>> {
    let mut core = Core::new();
    let (busy_sender, busy_events) = mpsc::unbounded_channel();
    let (other_sender, other_events) = mpsc::unbounded_channel();
    let busy = TaskId::new("busy");
    let other = TaskId::new("other");
    for (id, events) in [(busy.clone(), busy_events), (other.clone(), other_events)] {
        core.sessions.insert(
            id,
            HarnessSession::new(Box::new(LateHarness {
                events,
                approvals: Vec::new(),
                prompts: None,
                mode: None,
            })),
        );
    }
    let count = shep_core::harness::MAX_EVENTS_PER_PUMP * 2 + 1;
    for index in 0..count {
        busy_sender.send(HarnessEvent::Delta {
            item_id: ItemId::new("busy"),
            kind: DeltaKind::Message,
            text: format!("{index},"),
            stream_permit: None,
        })?;
    }
    other_sender.send(HarnessEvent::Delta {
        item_id: ItemId::new("other"),
        kind: DeltaKind::Message,
        text: "other".into(),
        stream_permit: None,
    })?;
    let notify = Arc::new(tokio::sync::Notify::new());
    core.arm_agent_events(&notify);
    tokio::time::timeout(Duration::from_secs(1), notify.notified()).await?;
    let first = core
        .sessions
        .get_mut(&busy)
        .ok_or("missing busy session")?
        .pump();
    assert_eq!(first.len(), shep_core::harness::MAX_EVENTS_PER_PUMP);
    assert_eq!(
        core.sessions
            .get_mut(&other)
            .ok_or("missing other session")?
            .pump()
            .len(),
        1
    );
    let mut received = first;
    loop {
        core.arm_agent_events(&notify);
        if received.len() == count {
            break;
        }
        tokio::time::timeout(Duration::from_secs(1), notify.notified()).await?;
        received.extend(
            core.sessions
                .get_mut(&busy)
                .ok_or("missing busy session")?
                .pump(),
        );
    }
    let chunks: Vec<_> = received
        .into_iter()
        .filter_map(|event| match event {
            HarnessEvent::Delta { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(
        chunks,
        (0..count)
            .map(|index| format!("{index},"))
            .collect::<Vec<_>>()
    );
    Ok(())
}

#[tokio::test]
async fn live_grants_are_listed_and_reset_by_project_and_task()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = TestRepository::new()?;
    let project = Project::new(directory.path().join("project"));
    let other_project = Project::new(directory.path().join("other"));
    let mut core = Core::new();
    core.projects.insert(project.id.clone(), project.clone());
    core.projects
        .insert(other_project.id.clone(), other_project.clone());
    let approval = SessionApproval {
        id: "grant".into(),
        title: "Run tests".into(),
        arguments: serde_json::json!({"command": "cargo test"}),
        paths: Vec::new(),
    };
    let mut launch = |project: &Project, title: &str| {
        let mut task = Task::draft(project.id.clone(), title, &project.path);
        task.agent = Agent::Claude;
        let task_id = task.id.clone();
        let (sender, events) = mpsc::unbounded_channel();
        core.tasks.insert(task_id.clone(), task);
        core.sessions.insert(
            task_id.clone(),
            HarnessSession::new(Box::new(LateHarness {
                events,
                approvals: vec![approval.clone()],
                prompts: None,
                mode: None,
            })),
        );
        (task_id, sender)
    };
    let (first, _first_events) = launch(&project, "First chat");
    let (second, second_events) = launch(&project, "Second chat");
    let (other, _other_events) = launch(&other_project, "Other project chat");

    let grants = core.project_approvals(&project.id).await?;
    assert_eq!(grants.sessions.len(), 2);
    assert!(
        grants
            .sessions
            .iter()
            .any(|grant| grant.task_id == first.as_str() && grant.task_title == "First chat")
    );
    assert!(matches!(
        core.reset_approval_grant(&project.id, "grant", Some(&other))
            .await,
        Err(CoreError::TaskNotFound { .. })
    ));
    core.reset_approval_grant(&project.id, "grant", Some(&first))
        .await?;
    let grants = core.project_approvals(&project.id).await?;
    assert_eq!(
        grants.sessions.first().map(|grant| grant.task_id.as_str()),
        Some(second.as_str())
    );
    assert_eq!(grants.sessions.len(), 1);
    assert_eq!(
        core.project_approvals(&other_project.id)
            .await?
            .sessions
            .len(),
        1
    );
    core.drain_events();

    second_events.send(HarnessEvent::Exited {
        code: Some(0),
        stderr: String::new(),
    })?;
    core.pump().await?;
    assert!(
        core.project_approvals(&project.id)
            .await?
            .sessions
            .is_empty()
    );
    assert!(core.drain_events().iter().any(|event| matches!(
        event, Event::ProjectApprovalsChanged { project_id } if *project_id == project.id
    )));
    Ok(())
}

#[tokio::test]
async fn pumping_folds_a_streaming_turn_into_the_workspace()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let (sender, receiver) = mpsc::unbounded_channel();
    let (prompts, mut sent) = mpsc::unbounded_channel();
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&database.uri()).await?),
        Box::new(LateHarnessFactory {
            receiver: Mutex::new(Some(receiver)),
            prompts,
        }),
    );

    let CommandResult::ProjectList(projects) = core
        .execute(Command::AddProject {
            path: repository.path().to_path_buf(),
        })
        .await?
    else {
        return Err("expected a project list".into());
    };
    let project = projects.projects.first().ok_or("the project is listed")?;
    let CommandResult::TaskWorkspace(draft) = core
        .execute(Command::CreateDraft {
            agent: Agent::Codex,
            project_id: ProjectId::new(project.id.clone()),
            title: "Stream a turn".to_string(),
        })
        .await?
    else {
        return Err("expected a draft".into());
    };
    let task_id = TaskId::new(draft.task.id.clone());
    let prompt = r#"Open @file("README.md")"#;
    let CommandResult::TaskWorkspace(workspace) = core
        .execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput {
                mode: Some("plan".into()),
                ..PromptInput::text(prompt)
            },
        })
        .await?
    else {
        return Err("expected a task workspace".into());
    };
    assert_eq!(workspace.task.state, TaskState::Working);
    assert_eq!(workspace.transcript.len(), 1);
    let (mode, input) = sent.try_recv()?;
    assert_eq!(mode.as_deref(), Some("plan"));
    assert_eq!(input.text, prompt);
    let [file] = input.attachments.as_slice() else {
        return Err("expected one referenced file".into());
    };
    let worktree = workspace
        .task
        .worktree_path
        .as_deref()
        .ok_or("missing worktree")?;
    assert_eq!(
        file.path,
        std::path::Path::new(worktree)
            .canonicalize()?
            .join("README.md")
    );
    assert!(file.metadata.is_none());
    core.drain_events();

    sender.send(HarnessEvent::Delta {
        item_id: ItemId::new("item-1"),
        kind: DeltaKind::Message,
        text: "streamed".to_string(),
        stream_permit: None,
    })?;
    assert!(core.pump().await?);
    let store = TursoStore::open(&database.uri()).await?;
    let persisted = store.load_transcript(&task_id).await?;
    assert_eq!(
        persisted.iter().map(Entry::summary).collect::<Vec<_>>(),
        [format!("you: {prompt}")]
    );
    sender.send(HarnessEvent::TurnFinished {
        reason: TurnFinishReason::Completed,
    })?;

    assert!(core.pump().await?);

    let workspace = core.task_workspace(&task_id)?;
    assert_eq!(workspace.task.state, TaskState::Review);
    assert_eq!(workspace.transcript.len(), 2);
    assert_eq!(workspace.transcript[1].summary(), "agent: streamed");
    assert_eq!(store.load_transcript(&task_id).await?, workspace.transcript);
    let rejected = core
        .execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput {
                mode: Some("unavailable".into()),
                ..PromptInput::text("Do not send this prompt")
            },
        })
        .await;
    assert!(matches!(
        rejected,
        Err(CoreError::Harness(
            shep_harness_core::HarnessError::ModeUnavailable
        ))
    ));
    assert!(sent.try_recv().is_err());
    assert_eq!(core.task_workspace(&task_id)?.task.state, TaskState::Ready);
    assert_eq!(store.load_transcript(&task_id).await?, workspace.transcript);
    let invalid = core
        .execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text(r#"Read @file("../README.md")"#),
        })
        .await;
    assert!(matches!(invalid, Err(CoreError::FileMention { .. })));
    assert!(sent.try_recv().is_err());
    assert!(
        core.drain_events()
            .iter()
            .any(|event| matches!(event, Event::TaskWorkspaceChanged(_))),
        "the pump must publish the changed workspace"
    );

    core.execute(Command::SendPrompt {
        task_id: task_id.clone(),
        input: PromptInput::text("Cancel this turn"),
    })
    .await?;
    let CommandResult::TaskWorkspace(cancelling) = core
        .execute(Command::CancelTask {
            task_id: task_id.clone(),
        })
        .await?
    else {
        return Err("expected a task workspace".into());
    };
    assert!(cancelling.cancelling);
    assert_eq!(cancelling.task.state, TaskState::Working);
    assert!(
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("Too soon"),
        })
        .await
        .is_err()
    );

    sender.send(HarnessEvent::TurnFinished {
        reason: TurnFinishReason::Cancelled,
    })?;
    assert!(core.pump().await?);
    let finished = core.task_workspace(&task_id)?;
    assert!(!finished.cancelling);
    assert_eq!(finished.task.state, TaskState::Ready);
    assert_eq!(finished.task.worktree_path, cancelling.task.worktree_path);

    core.execute(Command::SendPrompt {
        task_id: task_id.clone(),
        input: PromptInput::text("Cancel before the process exits"),
    })
    .await?;
    core.execute(Command::CancelTask {
        task_id: task_id.clone(),
    })
    .await?;
    sender.send(HarnessEvent::Exited {
        code: Some(0),
        stderr: String::new(),
    })?;
    assert!(core.pump().await?);
    let exited = core.task_workspace(&task_id)?;
    assert!(!exited.cancelling);
    assert_eq!(exited.task.state, TaskState::Failed);
    Ok(())
}
