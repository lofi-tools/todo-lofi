#![allow(clippy::too_many_lines)]

use shep_core::harness::{HarnessSession, ProjectApprovals};
use shep_core::session::SessionRecord;
use shep_core::store::Store;
use shep_core::test_support::{TestDatabase, TestDirectory};
use shep_core::*;
use shep_harness_core::fake_harness::FakeHarness;
use shep_harness_core::{
    ApprovalId, ApprovalRequest, ItemId, ProjectApprovalPolicy, SpawnSpec, ToolCallId,
};
use shep_store::TursoStore;
use std::sync::Arc;

#[tokio::test]
async fn terminal_prompt_admission_persists_once_and_rolls_back_on_failure()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::new()?;
    let store = TursoStore::open(&database.uri()).await?;
    let project = Project::new(database.path().join("project"));
    store.save_project(&project).await?;
    let mut task = Task::draft(project.id.clone(), "task", database.path());
    task.state = shep_core::TaskState::Working;
    store.save_task(&task).await?;
    let record = SessionRecord::started(task.id.clone(), "fixture", None);
    let mut session = HarnessSession::new(Box::new(FakeHarness::new(Vec::new())));
    session.start(SpawnSpec::default()).await?;
    session.open_session(None).await?;
    let failed = session
        .persist_terminal_prompt(
            &store,
            &record,
            &task,
            ItemId::new("prompt-1"),
            "hello".into(),
        )
        .await;
    assert!(failed.is_err());
    assert!(session.transcript().is_empty());
    store.save_session(&record).await?;
    session
        .persist_terminal_prompt(
            &store,
            &record,
            &task,
            ItemId::new("prompt-1"),
            "hello".into(),
        )
        .await?;
    assert_eq!(store.load_transcript(&task.id).await?.len(), 1);
    assert!(
        session
            .persist_terminal_prompt(
                &store,
                &record,
                &task,
                ItemId::new("prompt-1"),
                "hello".into()
            )
            .await
            .is_err()
    );
    assert_eq!(session.transcript().len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_project_grant_survives_reopen_and_reset_is_scoped()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::new()?;
    let project = Project::new(database.path().join("first"));
    let other = Project::new(database.path().join("other"));
    let spec = SpawnSpec {
        program: "/bin/fixture".into(),
        ..SpawnSpec::default()
    };
    let request = ApprovalRequest {
        approval_id: ApprovalId::new("approval-1"),
        tool_call_id: ToolCallId::new("tool-1"),
        title: "Check status".into(),
        arguments: serde_json::json!({"command": "git status"}),
        paths: Vec::new(),
        patch: None,
        choices: None,
        stream_permit: None,
    };
    let key = "exact ACP input";
    {
        let store: Arc<dyn Store> = Arc::new(TursoStore::open(&database.uri()).await?);
        store.save_project(&project).await?;
        store.save_project(&other).await?;
        let policy =
            ProjectApprovals::new(store.clone(), project.id.clone(), Agent::Claude, &spec)?;
        let other_policy = ProjectApprovals::new(store, other.id.clone(), Agent::Claude, &spec)?;
        policy.remember(key, &request).await?;
        policy.remember(key, &request).await?;
        assert!(!other_policy.allows(key).await?);
        other_policy.remember(key, &request).await?;
    }

    let store: Arc<dyn Store> = Arc::new(TursoStore::open(&database.uri()).await?);
    let policy = ProjectApprovals::new(store.clone(), project.id.clone(), Agent::Claude, &spec)?;
    let other_policy =
        ProjectApprovals::new(store.clone(), other.id.clone(), Agent::Claude, &spec)?;
    let changed_spec = SpawnSpec {
        args: vec!["--other-profile".into()],
        ..spec.clone()
    };
    let changed = ProjectApprovals::new(
        store.clone(),
        project.id.clone(),
        Agent::Claude,
        &changed_spec,
    )?;
    let other_agent =
        ProjectApprovals::new(store.clone(), project.id.clone(), Agent::OpenCode, &spec)?;
    assert!(policy.allows(key).await?);
    assert!(!changed.allows(key).await?);
    assert!(!other_agent.allows(key).await?);
    assert!(!policy.allows("different ACP input").await?);
    let listed = store.project_approvals(&project.id).await?;
    assert_eq!(listed.len(), 1);
    let grant = listed.first().ok_or("missing persisted grant")?;
    assert_eq!(grant.arguments, request.arguments);
    store.reset_project_approval(&project.id, &grant.id).await?;
    store.reset_project_approval(&project.id, &grant.id).await?;
    assert!(store.project_approvals(&project.id).await?.is_empty());
    assert!(!policy.allows(key).await?);
    assert!(other_policy.allows(key).await?);
    Ok(())
}
