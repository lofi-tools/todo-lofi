#![allow(clippy::too_many_lines)]

use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use shep_harness_core::fake_harness::FakeHarness;
use shep_harness_core::fake_harness::{FakeStep, FakeTurn};
use shep_harness_core::{AgentHarness, Capabilities, HarnessError, HarnessResult, SpawnSpec};
use shep_harness_core::{ApprovalId, Decision, Entry};

use shep_core::store::{
    MemoryStore, POOL_CATALOG_CONTRACT_VERSION, PoolAvailabilityState, PoolCatalogRecord,
    PoolCooldownClearRequest, PoolQuotaCorrectionRequest, PoolQuotaEntry, PoolQuotaSource,
    PoolSlot, PoolSlotId,
};
use shep_core::store::{PoolReorderRequest, PoolSlotEnabledRequest};
use shep_core::*;
use shep_store::TursoStore;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use shep_core::git::{GitError, GitService};
use shep_core::harness::*;
use shep_core::store::*;
use shep_core::terminal::*;
use shep_core::test_support::TestRepository;
use shep_core::test_support::{FakeHarnessFactory, TestCore};
use shep_core::test_support::{TestDatabase, TestDirectory};
use shep_harness_core::{FailureKind, HarnessEvent, PromptInput, SessionId, TurnFinishReason};
use shep_store::test_support::LateHarnessFactory;
use tokio::sync::mpsc;

async fn add_project(
    core: &mut Core,
    path: PathBuf,
) -> Result<ProjectId, Box<dyn std::error::Error>> {
    let result = core.execute(Command::AddProject { path }).await?;
    let CommandResult::ProjectList(projects) = result else {
        return Err("unexpected result".into());
    };
    let project = projects
        .projects
        .last()
        .ok_or("the project list should contain the added project")?;
    Ok(ProjectId::new(project.id.clone()))
}

#[tokio::test]
async fn pool_controls_delegate_to_the_store_and_repeat_safely()
-> Result<(), Box<dyn std::error::Error>> {
    let mut core = Core::new();
    let mut first = PoolSlot::new(Agent::Codex, "acme", "Acme", "first", "First");
    first.id = PoolSlotId::new("core-first");
    first.created_at = 100;
    first.updated_at = 100;
    let mut second = PoolSlot::new(Agent::Claude, "other", "Other", "second", "Second");
    second.id = PoolSlotId::new("core-second");
    second.priority = 1;
    second.created_at = 200;
    second.updated_at = 200;
    for slot in [&first, &second] {
        core.store.save_pool_slot(slot).await?;
        let mut entry = PoolQuotaEntry::new(slot.id.clone(), PoolQuotaSource::Provider, 100);
        entry.used = Some(0);
        entry.limit = Some(10);
        entry.limit_unit = Some("requests".into());
        entry.reset_at = Some(1_000);
        core.store.append_pool_quota_entry(&entry).await?;
    }

    let status = core.pool_status(150).await?;
    assert_eq!(status.slots.len(), 2);
    assert_eq!(status.slots[0].slot.id, first.id);
    assert_eq!(
        status.slots[0].availability.state,
        PoolAvailabilityState::Available
    );

    let reorder = PoolReorderRequest {
        slot_ids: vec![second.id.clone(), first.id.clone()],
        now: 200,
    };
    let outcome = core.reorder_pool_slots(&reorder).await?;
    assert_eq!(outcome.slots[0].id, second.id);
    assert_eq!(
        core.reorder_pool_slots(&reorder).await?.slots[0].id,
        second.id
    );

    let disabled = core
        .set_pool_slot_enabled(&PoolSlotEnabledRequest {
            slot_id: second.id.clone(),
            enabled: false,
            now: 300,
        })
        .await?;
    assert!(!disabled.slot.enabled);
    assert!(
        !core
            .set_pool_slot_enabled(&PoolSlotEnabledRequest {
                slot_id: second.id.clone(),
                enabled: false,
                now: 310,
            })
            .await?
            .slot
            .enabled
    );
    let status = core.pool_status(320).await?;
    assert_eq!(
        status.slots[0].availability.state,
        PoolAvailabilityState::Disabled
    );

    let correction = PoolQuotaCorrectionRequest {
        slot_id: first.id.clone(),
        limit: Some(20),
        limit_unit: Some("requests".into()),
        reset_at: Some(5_000),
        now: 400,
        window_key: None,
    };
    let corrected = core.correct_pool_quota(&correction).await?;
    assert!(corrected.applied);
    assert_eq!(
        corrected.latest_quota.as_ref().and_then(|entry| entry.used),
        Some(0)
    );
    assert_eq!(
        corrected.latest_quota.as_ref().map(|entry| entry.source),
        Some(PoolQuotaSource::User)
    );
    assert_eq!(
        corrected.availability.state,
        PoolAvailabilityState::Available
    );
    let repeated = core
        .correct_pool_quota(&PoolQuotaCorrectionRequest {
            now: 410,
            ..correction.clone()
        })
        .await?;
    assert!(!repeated.applied);
    assert_eq!(
        repeated
            .latest_quota
            .as_ref()
            .map(|entry| entry.id.as_str()),
        corrected
            .latest_quota
            .as_ref()
            .map(|entry| entry.id.as_str())
    );

    let mut cooling = PoolQuotaEntry::new(first.id.clone(), PoolQuotaSource::Provider, 500);
    cooling.used = Some(5);
    cooling.limit = Some(20);
    cooling.limit_unit = Some("requests".into());
    cooling.reset_at = Some(5_000);
    cooling.cooldown_until = Some(2_000);
    core.store.append_pool_quota_entry(&cooling).await?;
    let status = core.pool_status(600).await?;
    let first_status = status
        .slots
        .iter()
        .find(|status| status.slot.id == first.id)
        .ok_or("corrected slot status missing")?;
    assert_eq!(
        first_status.availability.state,
        PoolAvailabilityState::CoolingDown
    );

    let cleared = core
        .clear_pool_slot_cooldown(&PoolCooldownClearRequest::new(first.id.clone(), 600))
        .await?;
    assert!(cleared.applied);
    assert_eq!(
        cleared
            .latest_quota
            .as_ref()
            .and_then(|entry| entry.cooldown_until),
        None
    );
    assert_eq!(cleared.availability.state, PoolAvailabilityState::Available);
    assert!(
        !core
            .clear_pool_slot_cooldown(&PoolCooldownClearRequest::new(first.id.clone(), 610))
            .await?
            .applied
    );

    assert!(matches!(
        core.correct_pool_quota(&PoolQuotaCorrectionRequest {
            slot_id: first.id.clone(),
            limit: Some(30),
            limit_unit: Some("requests".into()),
            reset_at: Some(5_000),
            now: 600,
            window_key: None,
        })
        .await,
        Err(CoreError::Store(StoreError::InvalidRow(_)))
    ));
    assert!(matches!(
        core.correct_pool_quota(&PoolQuotaCorrectionRequest::new(
            PoolSlotId::new("missing"),
            700
        ))
        .await,
        Err(CoreError::Store(StoreError::PoolSlotNotFound { .. }))
    ));
    assert!(matches!(
        core.clear_pool_slot_cooldown(&PoolCooldownClearRequest::new(
            PoolSlotId::new("missing"),
            700
        ))
        .await,
        Err(CoreError::Store(StoreError::PoolSlotNotFound { .. }))
    ));
    assert!(matches!(
        core.set_pool_slot_enabled(&PoolSlotEnabledRequest {
            slot_id: PoolSlotId::new("missing"),
            enabled: true,
            now: 320,
        })
        .await,
        Err(CoreError::Store(StoreError::PoolSlotNotFound { .. }))
    ));
    Ok(())
}

#[tokio::test]
async fn pool_force_covers_one_turn_and_does_not_persist() -> Result<(), Box<dyn std::error::Error>>
{
    let database = TestDatabase::new()?;
    let uri = database.uri();
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(UnavailableHarnessFactory),
    );
    let mut slot = PoolSlot::new(Agent::Codex, "acme", "Acme", "model-a", "Model A");
    slot.id = PoolSlotId::new("force-slot");
    slot.created_at = 100;
    slot.updated_at = 100;
    core.store.save_pool_slot(&slot).await?;
    let mut entry = PoolQuotaEntry::new(slot.id.clone(), PoolQuotaSource::Provider, 100);
    entry.used = Some(1);
    entry.limit = Some(10);
    entry.limit_unit = Some("requests".into());
    core.store.append_pool_quota_entry(&entry).await?;

    let forced = core
        .force_pool_slot(&PoolForceRequest {
            slot_id: slot.id.clone(),
            now: 200,
        })
        .await?;
    assert!(forced.applied);
    assert_eq!(forced.forced_slot.as_ref(), Some(&slot.id));
    assert_eq!(
        core.pool_status(210).await?.forced_slot.as_ref(),
        Some(&slot.id)
    );

    let repeated = core
        .force_pool_slot(&PoolForceRequest {
            slot_id: slot.id.clone(),
            now: 220,
        })
        .await?;
    assert!(!repeated.applied);
    assert_eq!(repeated.reason, "slot is already forced for the next turn");

    let request = core.pool_selection_request("turn-1", 230);
    assert_eq!(request.request_id, "turn-1");
    assert_eq!(request.override_slot.as_ref(), Some(&slot.id));
    assert_eq!(core.pool_status(240).await?.forced_slot, None);
    assert_eq!(
        core.pool_selection_request("turn-2", 250).override_slot,
        None
    );

    assert!(
        core.force_pool_slot(&PoolForceRequest {
            slot_id: slot.id.clone(),
            now: 260,
        })
        .await?
        .applied
    );
    let mut cooling = PoolQuotaEntry::new(slot.id.clone(), PoolQuotaSource::Provider, 270);
    cooling.used = Some(2);
    cooling.limit = Some(10);
    cooling.limit_unit = Some("requests".into());
    cooling.cooldown_until = Some(10_000);
    core.store.append_pool_quota_entry(&cooling).await?;
    let rejected = core
        .force_pool_slot(&PoolForceRequest {
            slot_id: slot.id.clone(),
            now: 280,
        })
        .await?;
    assert!(!rejected.applied);
    assert!(
        rejected
            .reason
            .starts_with("cannot force a cooling-down slot")
    );
    assert_eq!(rejected.forced_slot.as_ref(), Some(&slot.id));
    assert!(matches!(
        core.force_pool_slot(&PoolForceRequest {
            slot_id: PoolSlotId::new("missing"),
            now: 290,
        })
        .await,
        Err(CoreError::Store(StoreError::PoolSlotNotFound { .. }))
    ));

    let mut reopened = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(UnavailableHarnessFactory),
    );
    assert_eq!(reopened.pool_status(300).await?.forced_slot, None);

    let cleared = core.clear_pool_force();
    assert!(cleared.applied);
    assert_eq!(cleared.forced_slot, None);
    assert!(!core.clear_pool_force().applied);
    Ok(())
}

#[tokio::test]
async fn adding_a_pool_slot_appends_it_and_rejects_a_duplicate()
-> Result<(), Box<dyn std::error::Error>> {
    let mut core = Core::with_services(
        Box::new(MemoryStore::new()),
        Box::new(UnavailableHarnessFactory),
    );
    let first = core
        .add_pool_slot(
            Agent::Claude,
            "anthropic",
            "Anthropic",
            "claude-opus",
            "Claude Opus",
        )
        .await?;
    assert_eq!(first.priority, 0);
    assert_eq!(first.agent, Agent::Claude);

    let second = core
        .add_pool_slot(Agent::Codex, "openai", "OpenAI", "gpt-codex", "GPT Codex")
        .await?;
    assert_eq!(second.priority, 1);
    assert_eq!(core.pool_status(100).await?.slots.len(), 2);

    let duplicate = core
        .add_pool_slot(
            Agent::Claude,
            "anthropic",
            "Anthropic",
            "claude-opus",
            "Claude Opus",
        )
        .await;
    assert!(matches!(
        duplicate,
        Err(CoreError::Store(StoreError::InvalidRow(_)))
    ));
    Ok(())
}

fn catalog_record(provider: &str, model: Option<&str>, allowance: u64) -> PoolCatalogRecord {
    PoolCatalogRecord {
        provider_id: provider.to_string(),
        model_id: model.map(str::to_string),
        plan_id: Some("plan-1".to_string()),
        offer_id: format!("offer-{provider}"),
        allowance: Some(allowance),
        allowance_unit: Some("requests".to_string()),
        reset_period: Some("monthly".to_string()),
        usage_model: "subscription".to_string(),
        capacity: "known".to_string(),
        snapshot_id: "snap-1".to_string(),
        schema_version: 3,
        stale: false,
        source_url: "https://example.test/offer".to_string(),
        conditions: None,
    }
}

fn catalog_advisory(records: Vec<PoolCatalogRecord>) -> PoolCatalogAdvisory {
    PoolCatalogAdvisory {
        contract_version: POOL_CATALOG_CONTRACT_VERSION,
        snapshot_id: "snap-1".to_string(),
        schema_version: 3,
        generated_at: "2026-11-01T00:00:00Z".to_string(),
        stale: false,
        records,
    }
}

#[tokio::test]
async fn pool_catalog_seeds_unknown_limits_without_overriding_local_state()
-> Result<(), Box<dyn std::error::Error>> {
    let database = TestDatabase::new()?;
    let uri = database.uri();
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(UnavailableHarnessFactory),
    );
    let mut unknown = PoolSlot::new(Agent::Codex, "acme", "Acme", "model-a", "Model A");
    unknown.id = PoolSlotId::new("catalog-unknown");
    unknown.created_at = 100;
    unknown.updated_at = 100;
    let mut local = PoolSlot::new(Agent::Claude, "acme", "Acme", "model-b", "Model B");
    local.id = PoolSlotId::new("catalog-local");
    local.priority = 1;
    local.created_at = 200;
    local.updated_at = 200;
    let mut missing = PoolSlot::new(Agent::Codex, "other", "Other", "model-c", "Model C");
    missing.id = PoolSlotId::new("catalog-missing");
    missing.priority = 2;
    missing.created_at = 300;
    missing.updated_at = 300;
    for slot in [&unknown, &local, &missing] {
        core.store.save_pool_slot(slot).await?;
    }
    let mut used = PoolQuotaEntry::new(unknown.id.clone(), PoolQuotaSource::Provider, 100);
    used.used = Some(3);
    core.store.append_pool_quota_entry(&used).await?;
    let mut local_limit = PoolQuotaEntry::new(local.id.clone(), PoolQuotaSource::Provider, 100);
    local_limit.used = Some(2);
    local_limit.limit = Some(5);
    local_limit.limit_unit = Some("requests".into());
    core.store.append_pool_quota_entry(&local_limit).await?;

    let outcome = core
        .apply_pool_catalog(
            &catalog_advisory(vec![
                catalog_record("acme", Some("model-a"), 50),
                catalog_record("acme", Some("model-b"), 80),
            ]),
            200,
        )
        .await?;
    assert_eq!(outcome.snapshot_id, "snap-1");
    assert_eq!((outcome.applied, outcome.skipped), (1, 2));
    let seeded = outcome
        .seeds
        .iter()
        .find(|seed| seed.slot_id == unknown.id)
        .ok_or("missing seeded slot result")?;
    assert!(seeded.applied);
    assert_eq!(seeded.reason, "catalog seeded 50 requests");
    assert!(
        !outcome
            .seeds
            .iter()
            .find(|seed| seed.slot_id == local.id)
            .ok_or("missing local slot result")?
            .applied
    );
    assert_eq!(
        outcome
            .seeds
            .iter()
            .find(|seed| seed.slot_id == local.id)
            .map(|seed| seed.reason.clone()),
        Some("local limit wins over the catalog".to_string())
    );

    let status = core.pool_status(210).await?;
    let seeded_slot = status
        .slots
        .iter()
        .find(|entry| entry.slot.id == unknown.id)
        .ok_or("missing seeded status")?;
    assert_eq!(seeded_slot.slot.limit, Some(50));
    assert_eq!(seeded_slot.slot.used, Some(3));
    assert_eq!(
        seeded_slot.latest_quota.as_ref().map(|entry| entry.source),
        Some(PoolQuotaSource::Catalog)
    );
    let local_slot = status
        .slots
        .iter()
        .find(|entry| entry.slot.id == local.id)
        .ok_or("missing local status")?;
    assert_eq!(
        local_slot
            .latest_quota
            .as_ref()
            .and_then(|entry| entry.limit),
        Some(5)
    );

    let repeated = core
        .apply_pool_catalog(
            &catalog_advisory(vec![catalog_record("acme", Some("model-a"), 50)]),
            220,
        )
        .await?;
    assert_eq!(
        repeated
            .seeds
            .iter()
            .find(|seed| seed.slot_id == unknown.id)
            .map(|seed| seed.reason.clone()),
        Some("catalog seeded 50 requests".to_string())
    );
    let same_time = core
        .apply_pool_catalog(
            &catalog_advisory(vec![catalog_record("acme", Some("model-a"), 60)]),
            220,
        )
        .await?;
    assert_eq!(
        same_time
            .seeds
            .iter()
            .find(|seed| seed.slot_id == unknown.id)
            .map(|seed| seed.reason.clone()),
        Some("catalog snapshot is not newer than the latest observation".to_string())
    );

    let mut stale = catalog_advisory(vec![catalog_record("acme", Some("model-a"), 70)]);
    stale.stale = true;
    let stale_outcome = core.apply_pool_catalog(&stale, 240).await?;
    assert_eq!(stale_outcome.applied, 0);
    assert!(
        stale_outcome
            .seeds
            .iter()
            .all(|seed| seed.reason == "catalog snapshot is stale")
    );

    let mut wrong = catalog_advisory(Vec::new());
    wrong.contract_version = 1;
    assert!(matches!(
        core.apply_pool_catalog(&wrong, 250).await,
        Err(CoreError::Store(StoreError::InvalidRow(_)))
    ));

    let mut reopened = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(UnavailableHarnessFactory),
    );
    let status = reopened.pool_status(260).await?;
    assert_eq!(
        status
            .slots
            .iter()
            .find(|entry| entry.slot.id == unknown.id)
            .and_then(|entry| entry.slot.limit),
        Some(50)
    );
    Ok(())
}

#[tokio::test]
async fn pool_windows_roll_from_the_last_observed_use() -> Result<(), Box<dyn std::error::Error>> {
    let mut core = Core::new();
    let mut codex = PoolSlot::new(Agent::Codex, "acme", "Acme", "model-a", "Model A");
    codex.id = PoolSlotId::new("window-codex");
    codex.last_used_at = Some(1_000);
    codex.created_at = 100;
    codex.updated_at = 100;
    let mut claude = PoolSlot::new(Agent::Claude, "zed", "Zed", "model-b", "Model B");
    claude.id = PoolSlotId::new("window-claude");
    claude.priority = 1;
    claude.last_used_at = Some(1_000);
    claude.created_at = 200;
    claude.updated_at = 200;
    let mut unused = PoolSlot::new(Agent::Codex, "blank", "Blank", "model-c", "Model C");
    unused.id = PoolSlotId::new("window-unused");
    unused.priority = 2;
    unused.created_at = 300;
    unused.updated_at = 300;
    for slot in [&codex, &claude, &unused] {
        core.store.save_pool_slot(slot).await?;
    }

    let status = core.pool_status(2_000).await?;
    let codex_status = status
        .slots
        .iter()
        .find(|entry| entry.slot.id == codex.id)
        .ok_or("missing codex slot")?;
    assert_eq!(codex_status.reset_windows.len(), 2);
    assert_eq!(codex_status.reset_windows[0].key, "five-hour");
    assert_eq!(
        codex_status.reset_windows[0].reset_at,
        Some(1_000 + 5 * 3_600_000)
    );
    assert_eq!(codex_status.reset_windows[1].key, "weekly");
    assert_eq!(
        codex_status.reset_windows[1].reset_at,
        Some(1_000 + 7 * 86_400_000)
    );
    let claude_status = status
        .slots
        .iter()
        .find(|entry| entry.slot.id == claude.id)
        .ok_or("missing claude slot")?;
    assert_eq!(claude_status.reset_windows.len(), 2);
    assert!(
        claude_status
            .reset_windows
            .iter()
            .all(|window| window.reset_at.is_none())
    );

    let mut weekly = catalog_record("acme", Some("model-a"), 50);
    weekly.reset_period = Some("week".into());
    let outcome = core
        .apply_pool_catalog(&catalog_advisory(vec![weekly]), 2_000)
        .await?;
    assert_eq!(outcome.applied, 1);
    let status = core.pool_status(2_100).await?;
    let seeded = status
        .slots
        .iter()
        .find(|entry| entry.slot.id == codex.id)
        .ok_or("missing seeded slot")?;
    assert_eq!(
        seeded
            .latest_quota
            .as_ref()
            .and_then(|entry| entry.reset_at),
        Some(1_000 + 7 * 86_400_000)
    );

    let mut monthly = catalog_record("blank", Some("model-c"), 60);
    monthly.reset_period = Some("month".into());
    let outcome = core
        .apply_pool_catalog(&catalog_advisory(vec![monthly]), 2_200)
        .await?;
    assert_eq!(outcome.applied, 1);
    let status = core.pool_status(2_300).await?;
    let unused_status = status
        .slots
        .iter()
        .find(|entry| entry.slot.id == unused.id)
        .ok_or("missing unused slot")?;
    assert_eq!(
        unused_status
            .latest_quota
            .as_ref()
            .and_then(|entry| entry.reset_at),
        None
    );
    assert!(
        unused_status
            .reset_windows
            .iter()
            .all(|window| window.reset_at.is_none())
    );
    Ok(())
}

#[tokio::test]
async fn pool_catalog_seeds_the_in_memory_store_and_skips_ambiguous_records()
-> Result<(), Box<dyn std::error::Error>> {
    let mut core = Core::new();
    let mut slot = PoolSlot::new(Agent::Codex, "acme", "Acme", "model-a", "Model A");
    slot.id = PoolSlotId::new("memory-catalog");
    slot.created_at = 100;
    slot.updated_at = 100;
    core.store.save_pool_slot(&slot).await?;

    let ambiguous = core
        .apply_pool_catalog(
            &catalog_advisory(vec![
                catalog_record("acme", None, 10),
                catalog_record("acme", Some("model-a"), 20),
            ]),
            200,
        )
        .await?;
    assert_eq!(ambiguous.applied, 0);
    assert_eq!(
        ambiguous.seeds.first().map(|seed| seed.reason.clone()),
        Some("catalog has 2 records for this slot; no single advisory".to_string())
    );

    let outcome = core
        .apply_pool_catalog(
            &catalog_advisory(vec![catalog_record("acme", Some("model-a"), 20)]),
            210,
        )
        .await?;
    assert_eq!(outcome.applied, 1);
    let status = core.pool_status(220).await?;
    assert_eq!(status.slots[0].slot.limit, Some(20));
    assert_eq!(
        status.slots[0]
            .latest_quota
            .as_ref()
            .map(|entry| entry.source),
        Some(PoolQuotaSource::Catalog)
    );
    Ok(())
}

#[tokio::test]
async fn a_turn_selects_a_pool_slot_and_records_it() -> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let mut core = Core::with_services(
        Box::new(MemoryStore::new()),
        Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
            FakeStep::Message("done".into()),
        ])]])),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            project_id,
            title: "Pool turn".into(),
            agent: Agent::Codex,
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id.clone());
    let mut slot = PoolSlot::new(Agent::Codex, "acme", "Acme", "model-a", "Model A");
    slot.id = PoolSlotId::new("turn-slot");
    core.store.save_pool_slot(&slot).await?;
    let mut entry = PoolQuotaEntry::new(slot.id.clone(), PoolQuotaSource::Provider, 100);
    entry.used = Some(0);
    entry.limit = Some(10);
    entry.limit_unit = Some("requests".into());
    core.store.append_pool_quota_entry(&entry).await?;

    let workspace = workspace_of(
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("start"),
        })
        .await?,
    );
    let rows = summaries(&workspace);
    assert!(
        rows.iter()
            .any(|row| row.contains("pool: Acme · Model A (codex)")),
        "rows: {rows:?}"
    );
    let selections = core.store.load_pool_selections().await?;
    let selection = selections
        .iter()
        .find(|selection| selection.request_id == task_id.as_str())
        .ok_or("missing pool selection")?;
    assert_eq!(selection.slot_id.as_ref(), Some(&slot.id));
    Ok(())
}

#[tokio::test]
async fn a_turn_refuses_to_start_when_no_slot_can_serve_it()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let mut core = Core::with_services(
        Box::new(MemoryStore::new()),
        Box::new(FakeHarnessFactory::new(Vec::new())),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            project_id,
            title: "Blocked turn".into(),
            agent: Agent::Codex,
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id.clone());
    let mut disabled = PoolSlot::new(Agent::Codex, "acme", "Acme", "model-a", "Model A");
    disabled.id = PoolSlotId::new("disabled-slot");
    disabled.enabled = false;
    core.store.save_pool_slot(&disabled).await?;
    let mut other = PoolSlot::new(Agent::Claude, "other", "Other", "model-b", "Model B");
    other.id = PoolSlotId::new("other-slot");
    other.priority = 1;
    core.store.save_pool_slot(&other).await?;

    let error = core
        .execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("start"),
        })
        .await
        .err()
        .ok_or("the turn must not start")?;
    let CoreError::PoolUnavailable { reason } = error else {
        return Err(format!("unexpected error: {error}").into());
    };
    assert!(reason.contains("no eligible pool slot"), "reason: {reason}");
    assert!(reason.contains("disabled"), "reason: {reason}");
    assert!(reason.contains("claude"), "reason: {reason}");
    assert_eq!(
        core.tasks.get(&task_id).map(|task| task.state),
        Some(TaskState::Ready)
    );
    assert!(!core.sessions.contains_key(&task_id));
    Ok(())
}

struct FallingBackFactory {
    inner: FakeHarnessFactory,
    failed: Agent,
    fallback: Agent,
}

impl HarnessFactory for FallingBackFactory {
    fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
        self.inner.create()
    }

    fn launch_for_task(
        &self,
        _task_id: &TaskId,
        agent: Agent,
        cwd: PathBuf,
    ) -> Result<(Box<dyn AgentHarness>, shep_harness_core::SpawnSpec), CoreError> {
        if agent == self.failed {
            return Err(CoreError::HarnessMissing {
                report: format!("{} is not installed", agent.as_str()),
            });
        }
        Ok((self.inner.create()?, self.inner.spawn_spec(cwd)))
    }

    fn fallback_agents(&self, _agent: Agent) -> Vec<Agent> {
        vec![self.fallback]
    }
}

#[tokio::test]
async fn a_conversation_falls_back_to_an_available_agent() -> Result<(), Box<dyn std::error::Error>>
{
    let repository = TestRepository::new()?;
    let mut core = Core::with_services(
        Box::new(MemoryStore::new()),
        Box::new(FallingBackFactory {
            inner: FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![FakeStep::Message(
                "fallback answer".into(),
            )])]]),
            failed: Agent::Codex,
            fallback: Agent::OpenCode,
        }),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            agent: Agent::Codex,
            project_id,
            title: "Fallback".into(),
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id.clone());
    let workspace = workspace_of(
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("continue"),
        })
        .await?,
    );
    assert_eq!(workspace.task.agent, Agent::OpenCode);
    assert!(
        workspace.transcript.iter().any(|entry| entry
            .summary()
            .contains("Codex is unavailable; continuing with OpenCode")),
        "rows: {:?}",
        summaries(&workspace)
    );
    Ok(())
}

#[test]
fn a_missing_model_falls_back_to_the_adapters_current_model() {
    let models = shep_harness_core::AgentModels {
        current: Some("acme/default".into()),
        options: vec![shep_harness_core::AgentModel {
            id: "acme/default".into(),
            name: "Default".into(),
            description: None,
        }],
    };
    assert_eq!(
        Core::model_fallback("acme/gone", &models),
        Some(Some("acme/default".into()))
    );
    assert_eq!(Core::model_fallback("acme/default", &models), None);
    assert_eq!(
        Core::model_fallback("acme/gone", &shep_harness_core::AgentModels::default()),
        None
    );
}

struct NamedAgentFactory;

impl HarnessFactory for NamedAgentFactory {
    fn create(&self) -> Result<Box<dyn shep_harness_core::AgentHarness>, CoreError> {
        Err(CoreError::HarnessUnavailable)
    }

    fn launch(
        &self,
        agent: Agent,
        cwd: PathBuf,
    ) -> Result<
        (
            Box<dyn shep_harness_core::AgentHarness>,
            shep_harness_core::SpawnSpec,
        ),
        CoreError,
    > {
        let name = agent.as_str();
        let turn = FakeTurn::new(vec![
            FakeStep::Message(format!("{name} started")),
            FakeStep::approval("shared-approval", "Approve tool"),
            FakeStep::WriteFile {
                path: "agent-result.txt".into(),
                contents: name.into(),
            },
            FakeStep::Message(format!("{name} finished")),
        ]);
        Ok((
            Box::new(shep_harness_core::fake_harness::FakeHarness::one_turn(turn)),
            self.spawn_spec(cwd),
        ))
    }
}

fn repository_at(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(path)?;
    let git = |args: &[&str]| -> Result<(), Box<dyn std::error::Error>> {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .status()?;
        if !status.success() {
            return Err(std::io::Error::other(format!("git {args:?} failed")).into());
        }
        Ok(())
    };
    git(&["init", "-b", "main"])?;
    git(&["config", "user.name", "Shepherd Test"])?;
    git(&["config", "user.email", "shepherd@example.com"])?;
    git(&["config", "commit.gpgsign", "false"])?;
    std::fs::write(path.join("README.md"), "initial\n")?;
    git(&["add", "README.md"])?;
    git(&["commit", "-m", "initial"])?;
    Ok(())
}

async fn prompt_task(
    core: &mut Core,
    project: &ProjectId,
    title: &str,
    agent: Agent,
) -> Result<(TaskId, PathBuf), Box<dyn std::error::Error>> {
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            project_id: project.clone(),
            title: title.to_string(),
            agent,
        })
        .await?,
    );
    let id = TaskId::new(draft.task.id);
    let waiting = workspace_of(
        core.execute(Command::SendPrompt {
            task_id: id.clone(),
            input: PromptInput::text("fixture prompt"),
        })
        .await?,
    );
    let path = PathBuf::from(waiting.task.worktree_path.ok_or("missing worktree")?);
    Ok((id, path))
}

fn workspace_of(result: CommandResult) -> TaskWorkspace {
    match result {
        CommandResult::TaskWorkspace(workspace) => workspace,
        other => panic!("expected a task workspace, got {other:?}"),
    }
}

fn summaries(workspace: &TaskWorkspace) -> Vec<String> {
    workspace.transcript.iter().map(Entry::summary).collect()
}

#[tokio::test]
async fn saturated_event_queue_reports_a_resync() {
    let mut core = Core::new();
    for index in 0..MAX_PENDING_EVENTS + 5 {
        core.emit(Event::TaskNeedsInput {
            task_id: format!("task-{index}"),
        });
    }
    let events = core.drain_events();
    assert_eq!(events.len(), MAX_PENDING_EVENTS + 1);
    assert!(matches!(events.last(), Some(Event::ResyncRequired)));
    assert!(core.drain_events().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn launch_environment_reaches_process_without_persisting_secret()
-> Result<(), Box<dyn std::error::Error>> {
    use shep_harness_core::fake_harness::FakeHarness;
    use shep_harness_core::{AgentHarness, Capabilities, HarnessResult, SpawnSpec};

    struct Factory;
    impl HarnessFactory for Factory {
        fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
            Ok(Box::new(InspectingHarness(FakeHarness::one_turn(
                FakeTurn::new(vec![FakeStep::Message("done".into())]),
            ))))
        }
        fn environment_for_project(
            &self,
            _: Agent,
            _: &Path,
        ) -> Result<BTreeMap<String, String>, CoreError> {
            Ok(BTreeMap::from([(
                "SHEPHERD_TEST_TOKEN".into(),
                "!cat token".into(),
            )]))
        }
    }
    struct InspectingHarness(FakeHarness);
    #[async_trait::async_trait]
    impl AgentHarness for InspectingHarness {
        fn events(&mut self) -> &mut mpsc::UnboundedReceiver<HarnessEvent> {
            self.0.events()
        }
        fn diagnostics(&self) -> shep_harness_core::AgentDiagnostics {
            self.0.diagnostics()
        }
        async fn start(&mut self, spec: SpawnSpec) -> HarnessResult<Capabilities> {
            let output = tokio::process::Command::new("/bin/sh")
                .args(["-c", "printf %s \"$SHEPHERD_TEST_TOKEN\""])
                .envs(&spec.environment)
                .output()
                .await
                .map_err(|error| shep_harness_core::HarnessError::Protocol(error.to_string()))?;
            if !output.status.success() || output.stdout != b"fixture-private-value" {
                return Err(shep_harness_core::HarnessError::Protocol(
                    "environment not delivered".into(),
                ));
            }
            self.0.start(spec).await
        }
        async fn open_session(&mut self, resume: Option<SessionId>) -> HarnessResult<SessionId> {
            self.0.open_session(resume).await
        }
        async fn prompt(&mut self, input: PromptInput) -> HarnessResult<()> {
            self.0.prompt(input).await
        }
        async fn cancel(&mut self) -> HarnessResult<()> {
            self.0.cancel().await
        }
        async fn respond(&mut self, id: ApprovalId, decision: Decision) -> HarnessResult<()> {
            self.0.respond(id, decision).await
        }
        async fn shutdown(&mut self) -> HarnessResult<()> {
            self.0.shutdown().await
        }
    }
    let repository = TestRepository::new()?;
    std::fs::write(repository.path().join("token"), "fixture-private-value\n")?;
    let database = TestDatabase::new()?;
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&database.uri()).await?),
        Box::new(Factory),
    );
    let project = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            project_id: project,
            title: "environment".into(),
            agent: Agent::Codex,
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id);
    let workspace = workspace_of(
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("hello"),
        })
        .await?,
    );
    assert!(!serde_json::to_string(&workspace)?.contains("fixture-private-value"));
    drop(core);
    let mut reopened = Core::load(
        Box::new(TursoStore::open(&database.uri()).await?),
        Box::new(UnavailableHarnessFactory),
    )
    .await?;
    assert!(
        !serde_json::to_string(&reopened.open_task_workspace(&task_id).await?)?
            .contains("fixture-private-value")
    );
    drop(reopened);
    for entry in std::fs::read_dir(database.path())? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            let data = std::fs::read(entry.path())?;
            assert!(
                !data
                    .windows(b"fixture-private-value".len())
                    .any(|window| window == b"fixture-private-value")
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn cancelled_admission_failure_is_persisted_and_cleanup_runs()
-> Result<(), Box<dyn std::error::Error>> {
    use shep_harness_core::fake_harness::FakeHarness;
    use shep_harness_core::{AgentHarness, Capabilities, HarnessResult, SpawnSpec};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Factory {
        started: Arc<tokio::sync::Notify>,
        shutdowns: Arc<AtomicUsize>,
    }
    impl HarnessFactory for Factory {
        fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
            let (sender, receiver) = mpsc::unbounded_channel();
            Ok(Box::new(AdmissionHarness {
                inner: FakeHarness::one_turn(FakeTurn::default()),
                started: self.started.clone(),
                shutdowns: self.shutdowns.clone(),
                sender,
                receiver,
            }))
        }
    }
    struct AdmissionHarness {
        inner: FakeHarness,
        started: Arc<tokio::sync::Notify>,
        shutdowns: Arc<AtomicUsize>,
        sender: mpsc::UnboundedSender<HarnessEvent>,
        receiver: mpsc::UnboundedReceiver<HarnessEvent>,
    }
    struct CancelledAdmission(mpsc::UnboundedSender<HarnessEvent>);
    impl Drop for CancelledAdmission {
        fn drop(&mut self) {
            if self
                .0
                .send(HarnessEvent::ConnectionFailed {
                    kind: FailureKind::Protocol,
                    text: "fixture admission cancelled".into(),
                })
                .is_err()
            {
                tracing::debug!("cancelled admission fixture receiver closed");
            }
        }
    }
    #[async_trait::async_trait]
    impl AgentHarness for AdmissionHarness {
        fn events(&mut self) -> &mut mpsc::UnboundedReceiver<HarnessEvent> {
            &mut self.receiver
        }
        async fn start(&mut self, spec: SpawnSpec) -> HarnessResult<Capabilities> {
            self.inner.start(spec).await
        }
        async fn open_session(&mut self, resume: Option<SessionId>) -> HarnessResult<SessionId> {
            self.inner.open_session(resume).await
        }
        async fn prompt(&mut self, _: PromptInput) -> HarnessResult<()> {
            let _guard = CancelledAdmission(self.sender.clone());
            self.started.notify_one();
            std::future::pending().await
        }
        async fn cancel(&mut self) -> HarnessResult<()> {
            self.inner.cancel().await
        }
        async fn respond(&mut self, id: ApprovalId, decision: Decision) -> HarnessResult<()> {
            self.inner.respond(id, decision).await
        }
        async fn shutdown(&mut self) -> HarnessResult<()> {
            self.shutdowns.fetch_add(1, Ordering::SeqCst);
            self.inner.shutdown().await
        }
    }

    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let started = Arc::new(tokio::sync::Notify::new());
    let shutdowns = Arc::new(AtomicUsize::new(0));
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&database.uri()).await?),
        Box::new(Factory {
            started: started.clone(),
            shutdowns: shutdowns.clone(),
        }),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            project_id,
            title: "Cancelled admission".into(),
            agent: Agent::Codex,
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id);
    let mut sending = Box::pin(core.execute(Command::SendPrompt {
        task_id: task_id.clone(),
        input: PromptInput::text("Keep this prompt"),
    }));
    tokio::select! {
        result = &mut sending => return Err(format!("admission finished unexpectedly: {result:?}").into()),
        _ = started.notified() => {}
    }
    drop(sending);
    assert!(core.pump().await?);
    let workspace = core.task_workspace(&task_id)?;
    assert_eq!(workspace.task.state, TaskState::Failed);
    let text = summaries(&workspace).join("\n");
    assert!(text.contains("Keep this prompt"));
    assert!(text.contains("fixture admission cancelled"));
    core.execute(Command::Quit { force: true }).await?;
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    drop(core);

    let mut reopened = Core::load(
        Box::new(TursoStore::open(&database.uri()).await?),
        Box::new(UnavailableHarnessFactory),
    )
    .await?;
    let restored = reopened.open_task_workspace(&task_id).await?;
    assert_eq!(restored.task.state, TaskState::Failed);
    let text = summaries(&restored).join("\n");
    assert!(text.contains("Keep this prompt"));
    assert!(text.contains("fixture admission cancelled"));
    Ok(())
}

#[tokio::test]
async fn same_named_projects_keep_identical_tasks_and_approvals_isolated()
-> Result<(), Box<dyn std::error::Error>> {
    let roots = TestDirectory::new()?;
    let first_path = roots.join("left/app");
    let second_path = roots.join("right/app");
    repository_at(&first_path)?;
    repository_at(&second_path)?;
    let database = TestDatabase::new()?;
    let uri = database.uri();
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(NamedAgentFactory),
    );
    let first_project = add_project(&mut core, first_path).await?;
    let second_project = add_project(&mut core, second_path).await?;
    let (first, first_worktree) =
        prompt_task(&mut core, &first_project, "Same title", Agent::Codex).await?;
    let (second, second_worktree) =
        prompt_task(&mut core, &second_project, "Same title", Agent::OpenCode).await?;
    assert_ne!(first, second);
    assert_ne!(first_worktree, second_worktree);
    assert_eq!(core.task_workspace(&first)?.task.state, TaskState::Waiting);
    assert_eq!(core.task_workspace(&second)?.task.state, TaskState::Waiting);

    let first_done = workspace_of(
        core.execute(Command::ResolveApproval {
            task_id: first.clone(),
            approval_id: ApprovalId::new("shared-approval"),
            decision: Decision::Allow,
        })
        .await?,
    );
    assert_eq!(first_done.task.state, TaskState::Review);
    assert_eq!(
        std::fs::read_to_string(first_worktree.join("agent-result.txt"))?,
        "codex"
    );
    assert!(!second_worktree.join("agent-result.txt").exists());
    assert_eq!(core.task_workspace(&second)?.task.state, TaskState::Waiting);
    let first_text = summaries(&first_done).join("\n");
    assert!(first_text.contains("codex finished"));
    assert!(!first_text.contains("opencode started"));

    let second_done = workspace_of(
        core.execute(Command::ResolveApproval {
            task_id: second.clone(),
            approval_id: ApprovalId::new("shared-approval"),
            decision: Decision::Deny,
        })
        .await?,
    );
    assert_ne!(second_done.task.state, TaskState::Waiting);
    assert!(!second_worktree.join("agent-result.txt").exists());
    let second_text = summaries(&second_done).join("\n");
    assert!(!second_text.contains("codex started"));
    assert!(!second_text.contains("opencode finished"));

    assert!(
        core.project_approvals(&first_project)
            .await?
            .saved
            .is_empty()
    );
    assert!(
        core.project_approvals(&second_project)
            .await?
            .saved
            .is_empty()
    );
    assert!(
        core.task_list(&first_project)?
            .tasks
            .iter()
            .all(|task| task.id != second.as_str())
    );
    assert!(
        core.task_list(&second_project)?
            .tasks
            .iter()
            .all(|task| task.id != first.as_str())
    );

    drop(core);
    let mut reopened = Core::load(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(UnavailableHarnessFactory),
    )
    .await?;
    assert_eq!(
        reopened.task_workspace(&first)?.task.state,
        TaskState::Review
    );
    assert!(reopened.task_workspace(&second)?.task.state != TaskState::Waiting);
    assert!(
        reopened
            .project_approvals(&first_project)
            .await?
            .saved
            .is_empty()
    );
    assert!(
        reopened
            .project_approvals(&second_project)
            .await?
            .saved
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn two_agents_in_one_project_keep_approvals_and_grants_separate()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let uri = database.uri();
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(NamedAgentFactory),
    );
    let project = add_project(&mut core, repository.path().to_path_buf()).await?;
    let (codex, codex_worktree) =
        prompt_task(&mut core, &project, "First task", Agent::Codex).await?;
    let (opencode, opencode_worktree) =
        prompt_task(&mut core, &project, "Second task", Agent::OpenCode).await?;
    let codex_done = workspace_of(
        core.execute(Command::ResolveApproval {
            task_id: codex.clone(),
            approval_id: ApprovalId::new("shared-approval"),
            decision: Decision::Allow,
        })
        .await?,
    );
    assert_eq!(codex_done.task.state, TaskState::Review);
    assert_eq!(
        std::fs::read_to_string(codex_worktree.join("agent-result.txt"))?,
        "codex"
    );
    assert_eq!(
        core.task_workspace(&opencode)?.task.state,
        TaskState::Waiting
    );
    assert!(!opencode_worktree.join("agent-result.txt").exists());

    let opencode_done = workspace_of(
        core.execute(Command::ResolveApproval {
            task_id: opencode.clone(),
            approval_id: ApprovalId::new("shared-approval"),
            decision: Decision::Deny,
        })
        .await?,
    );
    assert_ne!(opencode_done.task.state, TaskState::Waiting);
    assert!(!opencode_worktree.join("agent-result.txt").exists());
    assert!(
        summaries(&opencode_done)
            .join("\n")
            .contains("opencode started")
    );
    assert!(
        !summaries(&opencode_done)
            .join("\n")
            .contains("codex finished")
    );
    Ok(())
}

#[tokio::test]
async fn a_same_title_task_reports_a_worktree_conflict_without_touching_the_first()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&database.uri()).await?),
        Box::new(NamedAgentFactory),
    );
    let project = add_project(&mut core, repository.path().to_path_buf()).await?;
    let (first, first_worktree) =
        prompt_task(&mut core, &project, "Same title", Agent::Codex).await?;
    let duplicate = workspace_of(
        core.execute(Command::CreateDraft {
            project_id: project.clone(),
            title: "Same title".into(),
            agent: Agent::OpenCode,
        })
        .await?,
    );
    let duplicate_id = TaskId::new(duplicate.task.id);
    let duplicate_workspace = workspace_of(
        core.execute(Command::SendPrompt {
            task_id: duplicate_id.clone(),
            input: PromptInput::text("second prompt"),
        })
        .await?,
    );
    assert_eq!(duplicate_workspace.task.state, TaskState::Waiting);
    assert_ne!(
        duplicate_workspace.task.worktree_path.as_deref(),
        Some(first_worktree.to_string_lossy().as_ref())
    );
    assert!(first_worktree.exists());
    assert_eq!(core.task_workspace(&first)?.task.state, TaskState::Waiting);
    assert!(!first_worktree.join("agent-result.txt").exists());

    let completed = workspace_of(
        core.execute(Command::ResolveApproval {
            task_id: first,
            approval_id: ApprovalId::new("shared-approval"),
            decision: Decision::Allow,
        })
        .await?,
    );
    assert_eq!(completed.task.state, TaskState::Review);
    assert_eq!(
        std::fs::read_to_string(first_worktree.join("agent-result.txt"))?,
        "codex"
    );
    Ok(())
}

#[tokio::test]
async fn three_agent_sessions_keep_projects_approvals_and_worktrees_isolated()
-> Result<(), Box<dyn std::error::Error>> {
    struct ThreeAgentFactory;
    impl HarnessFactory for ThreeAgentFactory {
        fn create(&self) -> Result<Box<dyn shep_harness_core::AgentHarness>, CoreError> {
            Err(CoreError::HarnessUnavailable)
        }

        fn launch(
            &self,
            agent: Agent,
            cwd: PathBuf,
        ) -> Result<
            (
                Box<dyn shep_harness_core::AgentHarness>,
                shep_harness_core::SpawnSpec,
            ),
            CoreError,
        > {
            let name = agent.as_str();
            let turn = FakeTurn::new(vec![
                FakeStep::Message(format!("{name} started")),
                FakeStep::approval("same-approval-id", &format!("Approve {name}")),
                FakeStep::WriteFile {
                    path: "agent-result.txt".into(),
                    contents: name.into(),
                },
                FakeStep::Message(format!("{name} finished")),
            ]);
            Ok((
                Box::new(shep_harness_core::fake_harness::FakeHarness::one_turn(turn)),
                self.spawn_spec(cwd),
            ))
        }
    }

    let first_repository = TestRepository::new()?;
    let second_repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let uri = database.uri();
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(ThreeAgentFactory),
    );
    let first_project = add_project(&mut core, first_repository.path().to_path_buf()).await?;
    let second_project = add_project(&mut core, second_repository.path().to_path_buf()).await?;
    let mut tasks = Vec::new();
    for (project, agent) in [
        (&first_project, Agent::Codex),
        (&first_project, Agent::OpenCode),
        (&second_project, Agent::Claude),
    ] {
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                project_id: project.clone(),
                title: format!("{} task", agent.as_str()),
                agent,
            })
            .await?,
        );
        let id = TaskId::new(draft.task.id);
        let waiting = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: id.clone(),
                input: PromptInput::text(format!("prompt for {}", agent.as_str())),
            })
            .await?,
        );
        assert_eq!(waiting.task.state, TaskState::Waiting);
        assert_eq!(waiting.task.project_id, project.as_str());
        let worktree = waiting.task.worktree_path.ok_or("missing worktree")?;
        let branch = waiting.task.branch.ok_or("missing branch")?;
        tasks.push((id, agent, PathBuf::from(worktree), branch));
    }
    assert!(tasks.iter().all(|(id, _, _, _)| {
        core.task_workspace(id)
            .is_ok_and(|workspace| workspace.task.state == TaskState::Waiting)
    }));
    let mut worktrees: Vec<_> = tasks.iter().map(|(_, _, path, _)| path).collect();
    worktrees.sort();
    worktrees.dedup();
    assert_eq!(
        worktrees.len(),
        tasks.len(),
        "each task must own a distinct worktree"
    );
    let mut branches: Vec<_> = tasks.iter().map(|(_, _, _, branch)| branch).collect();
    branches.sort();
    branches.dedup();
    assert_eq!(
        branches.len(),
        tasks.len(),
        "each task must own a distinct branch"
    );
    assert_eq!(core.task_list(&first_project)?.tasks.len(), 2);
    assert_eq!(core.task_list(&second_project)?.tasks.len(), 1);
    for (index, (id, agent, path, _)) in tasks.iter().enumerate().rev() {
        let denied = *agent == Agent::Claude;
        let completed = workspace_of(
            core.execute(Command::ResolveApproval {
                task_id: id.clone(),
                approval_id: ApprovalId::new("same-approval-id"),
                decision: if denied {
                    Decision::Deny
                } else {
                    Decision::Allow
                },
            })
            .await?,
        );
        if denied {
            assert_ne!(completed.task.state, TaskState::Waiting);
            assert!(!path.join("agent-result.txt").exists());
        } else {
            assert_eq!(completed.task.state, TaskState::Review);
            assert_eq!(
                std::fs::read_to_string(path.join("agent-result.txt"))?,
                agent.as_str()
            );
        }
        let text = summaries(&completed).join("\n");
        assert_eq!(
            text.contains(&format!("{} finished", agent.as_str())),
            !denied
        );
        assert!(completed.transcript.iter().any(|entry| matches!(
            &entry.kind,
            shep_harness_core::EntryKind::Permission { approval_id, decision: Some(decision), .. }
                if approval_id == "same-approval-id" && *decision == if denied { Decision::Deny } else { Decision::Allow }
        )));
        for (other_id, other_agent, other_path, _) in &tasks[..index] {
            assert_eq!(
                core.task_workspace(other_id)?.task.state,
                TaskState::Waiting
            );
            assert!(!other_path.join("agent-result.txt").exists());
            assert!(!text.contains(&format!("{} started", other_agent.as_str())));
        }
    }
    drop(core);
    let mut reopened = Core::load(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(UnavailableHarnessFactory),
    )
    .await?;
    for (id, agent, _, _) in tasks {
        let restored = reopened.open_task_workspace(&id).await?;
        assert!(restored.transcript.iter().any(|entry| matches!(
            &entry.kind,
            shep_harness_core::EntryKind::Permission { approval_id, decision: Some(decision), .. }
                if approval_id == "same-approval-id" && *decision == if agent == Agent::Claude { Decision::Deny } else { Decision::Allow }
        )));
        let text = summaries(&restored).join("\n");
        assert_eq!(
            text.contains(&format!("{} finished", agent.as_str())),
            agent != Agent::Claude
        );
        for other in [Agent::Codex, Agent::OpenCode, Agent::Claude] {
            if other != agent {
                assert!(!text.contains(&format!("{} started", other.as_str())));
            }
        }
    }
    Ok(())
}

fn git_in(repository: &TestRepository, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repository.path())
        .args(args)
        .output()
        .expect("git should run");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn worktree_count(repository: &TestRepository) -> usize {
    git_in(repository, &["worktree", "list", "--porcelain"])
        .lines()
        .filter(|line| line.starts_with("worktree "))
        .count()
}

#[tokio::test]
async fn adding_a_project_marks_a_missing_path() {
    let directory = TestDirectory::new().expect("temp directory");
    let missing = directory.join("not-there");
    let mut core = Core::new();

    let result = core
        .execute(Command::AddProject {
            path: missing.clone(),
        })
        .await;
    let CommandResult::ProjectList(projects) = result.expect("project should be added") else {
        panic!("expected a project list");
    };

    assert_eq!(projects.projects.len(), 1);
    assert!(projects.projects[0].missing);
    assert_eq!(projects.projects[0].unfinished_task_count, 0);
}

#[tokio::test]
async fn duplicate_project_paths_are_rejected() {
    let directory = TestDirectory::new().expect("temp directory");
    let mut core = Core::new();
    let first = core
        .execute(Command::AddProject {
            path: directory.path().to_path_buf(),
        })
        .await;
    assert!(first.is_ok());

    let second = core
        .execute(Command::AddProject {
            path: directory.path().to_path_buf(),
        })
        .await;
    assert!(matches!(second, Err(CoreError::DuplicateProject { .. })));
}

#[tokio::test]
async fn a_draft_has_no_worktree_or_branch() {
    let repository = TestRepository::new().expect("temp repository");
    let mut core = Core::new();
    let project_id = add_project(&mut core, repository.path().to_path_buf())
        .await
        .expect("project should be added");

    let result = core
        .execute(Command::CreateDraft {
            agent: Agent::Codex,
            project_id: project_id.clone(),
            title: "Fix the parser".to_string(),
        })
        .await;
    let CommandResult::TaskWorkspace(workspace) = result.expect("draft should be created") else {
        panic!("expected a task workspace");
    };

    assert_eq!(workspace.task.state, TaskState::Draft);
    assert!(workspace.task.worktree_path.is_none());
    assert!(workspace.task.branch.is_none());
    assert!(workspace.transcript.is_empty());
    assert_eq!(workspace.task.proposed_branch, "shepherd/fix-the-parser");
    // The project path is canonicalized when it is added, so compare the
    // tail of the path rather than the whole temp directory.
    assert!(
        PathBuf::from(&workspace.task.proposed_worktree_path)
            .ends_with(".git/shepherd/worktrees/fix-the-parser"),
        "unexpected proposed worktree path: {}",
        workspace.task.proposed_worktree_path
    );

    // Naming the branch and the path must not create either of them.
    assert!(!PathBuf::from(&workspace.task.proposed_worktree_path).exists());
    assert_eq!(git_in(&repository, &["branch", "--list", "shepherd/*"]), "");
    assert_eq!(worktree_count(&repository), 1);
    assert_eq!(git_in(&repository, &["status", "--porcelain"]), "");

    let tasks = core.task_list(&project_id).expect("task list");
    assert_eq!(tasks.tasks.len(), 1);
    assert_eq!(workspace.project.unfinished_task_count, 1);
}

#[tokio::test]
async fn discarding_a_draft_removes_it_without_git_state() {
    let repository = TestRepository::new().expect("temp repository");
    let mut core = Core::new();
    let project_id = add_project(&mut core, repository.path().to_path_buf())
        .await
        .expect("project should be added");
    let result = core
        .execute(Command::CreateDraft {
            agent: Agent::Codex,
            project_id: project_id.clone(),
            title: "Temporary".to_string(),
        })
        .await;
    let CommandResult::TaskWorkspace(workspace) = result.expect("draft should be created") else {
        panic!("expected a task workspace");
    };
    let proposed_worktree_path = PathBuf::from(&workspace.task.proposed_worktree_path);
    let task_id = TaskId::new(workspace.task.id);

    assert!(
        core.execute(Command::DiscardDraft { task_id })
            .await
            .is_ok()
    );
    assert!(
        core.task_list(&project_id)
            .expect("task list")
            .tasks
            .is_empty()
    );

    // Discarding removes the record and leaves the repository untouched.
    assert!(!proposed_worktree_path.exists());
    assert_eq!(git_in(&repository, &["branch", "--list", "shepherd/*"]), "");
    assert_eq!(worktree_count(&repository), 1);
    assert_eq!(git_in(&repository, &["status", "--porcelain"]), "");
}

#[tokio::test]
async fn discarding_draft_removes_ended_terminals_from_layout()
-> Result<(), Box<dyn std::error::Error>> {
    use shep_core::terminal::{SplitAxis, TerminalSplit};

    let repository = TestRepository::new()?;
    let mut core = Core::new();
    let project = add_project(&mut core, repository.path().to_path_buf()).await?;
    let CommandResult::TaskWorkspace(draft) = core
        .execute(Command::CreateDraft {
            project_id: project.clone(),
            title: "Draft".into(),
            agent: Agent::Codex,
        })
        .await?
    else {
        return Err("expected draft workspace".into());
    };
    let task = TaskId::new(draft.task.id);
    let shared = TerminalRecord::project_shared(
        TerminalId::new("shared"),
        project.clone(),
        "Shared",
        repository.path().to_path_buf(),
        "shell",
        1,
    );
    let scoped = TerminalRecord::project_shared(
        TerminalId::new("task"),
        project.clone(),
        "Task",
        repository.path().to_path_buf(),
        "shell",
        2,
    )
    .for_task(task.clone());
    core.record_terminal(&shared).await?;
    core.record_terminal(&scoped).await?;
    let layout = TerminalLayout {
        root: Some(TerminalSplit::Split {
            axis: SplitAxis::Horizontal,
            ratio: 500,
            first: Box::new(TerminalSplit::Pane {
                terminal_id: "shared".into(),
            }),
            second: Box::new(TerminalSplit::Pane {
                terminal_id: "task".into(),
            }),
        }),
        ..TerminalLayout::default()
    };
    core.save_terminal_layout(&project, layout).await?;
    assert!(matches!(
        core.execute(Command::DiscardDraft {
            task_id: task.clone()
        })
        .await,
        Err(CoreError::TaskTerminalsRunning)
    ));
    core.end_terminal(&scoped.id, None).await?;
    core.execute(Command::DiscardDraft { task_id: task })
        .await?;
    assert_eq!(
        core.terminal_layout(&project)?.root,
        Some(TerminalSplit::Pane {
            terminal_id: "shared".into()
        })
    );
    assert_eq!(core.terminals_for(&project).len(), 1);
    Ok(())
}

#[tokio::test]
async fn deleting_a_saved_task_preserves_its_worktree_and_branch()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let uri = database.uri();
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
            FakeStep::WriteFile {
                path: "notes.txt".into(),
                contents: "Keep this work".into(),
            },
        ])]])),
    );
    let project = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            project_id: project.clone(),
            title: "Keep work".into(),
            agent: Agent::Codex,
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id);
    let task = workspace_of(
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("Edit notes"),
        })
        .await?,
    );
    assert_eq!(task.task.state, TaskState::Review);
    let worktree = PathBuf::from(task.task.worktree_path.ok_or("missing worktree")?);
    let branch = task.task.branch.ok_or("missing branch")?;
    assert!(!core.store.load_transcript(&task_id).await?.is_empty());
    assert!(matches!(
        core.execute(Command::DeleteTask {
            task_id: task_id.clone(),
            remove_worktree: true
        })
        .await,
        Err(CoreError::Git(GitError::DirtyWorktree { .. }))
    ));
    assert!(core.task_workspace(&task_id).is_ok());
    core.execute(Command::DeleteTask {
        task_id: task_id.clone(),
        remove_worktree: false,
    })
    .await?;
    assert!(core.task_list(&project)?.tasks.is_empty());
    assert!(core.store.load_transcript(&task_id).await?.is_empty());
    assert_eq!(
        std::fs::read_to_string(worktree.join("notes.txt"))?,
        "Keep this work"
    );
    assert!(git_in(&repository, &["branch", "--list", &branch]).contains(&branch));
    drop(core);
    let store = TursoStore::open(&uri).await?;
    assert!(store.load_tasks().await?.is_empty());
    assert!(store.load_sessions().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn deleting_a_task_with_an_active_turn_is_rejected() -> Result<(), Box<dyn std::error::Error>>
{
    let repository = TestRepository::new()?;
    let mut core = Core::with_services(
        Box::new(MemoryStore::new()),
        Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
            FakeStep::approval("approval", "Run"),
        ])]])),
    );
    let project = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            project_id: project,
            title: "Active".into(),
            agent: Agent::Codex,
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id);
    let task = workspace_of(
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("go"),
        })
        .await?,
    );
    assert_eq!(task.task.state, TaskState::Waiting);
    assert!(matches!(
        core.execute(Command::DeleteTask {
            task_id: task_id.clone(),
            remove_worktree: false,
        })
        .await,
        Err(CoreError::TaskDeletionBusy { .. })
    ));
    assert_eq!(
        core.task_workspace(&task_id)?.task.state,
        TaskState::Waiting
    );
    Ok(())
}

#[tokio::test]
async fn deleting_a_task_can_remove_a_clean_worktree_without_removing_its_branch()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let mut core = Core::with_services(
        Box::new(MemoryStore::new()),
        Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![])]])),
    );
    let project = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            project_id: project,
            title: "Clean".into(),
            agent: Agent::Codex,
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id);
    let task = workspace_of(
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("hello"),
        })
        .await?,
    );
    let worktree = PathBuf::from(task.task.worktree_path.ok_or("missing worktree")?);
    let branch = task.task.branch.ok_or("missing branch")?;
    core.execute(Command::DeleteTask {
        task_id,
        remove_worktree: true,
    })
    .await?;
    assert!(!worktree.exists());
    assert!(git_in(&repository, &["branch", "--list", &branch]).contains(&branch));
    Ok(())
}

#[tokio::test]
async fn failed_task_deletion_restores_checkout_and_ignored_files()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let uri = database.uri();
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![])]])),
    );
    let project = add_project(&mut core, fixture.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            project_id: project,
            title: "Recover deletion".into(),
            agent: Agent::Codex,
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id);
    let workspace = workspace_of(
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("hello"),
        })
        .await?,
    );
    let worktree = PathBuf::from(workspace.task.worktree_path.ok_or("missing worktree")?);
    let repository = GitService::discover(fixture.path())?;
    std::fs::write(repository.git_dir.join("info/exclude"), "ignored.txt\n")?;
    std::fs::write(worktree.join("ignored.txt"), "important ignored data")?;
    let mut database_probe = toasty::Db::builder()
        .build(toasty_driver_turso::Turso::new(&uri)?.experimental_index_method(true))
        .await?;
    toasty::sql::statement("CREATE TRIGGER block_task_deletion BEFORE DELETE ON tasks BEGIN SELECT RAISE(ABORT, 'fixture deletion failure'); END").exec(&mut database_probe).await?;
    assert!(
        core.execute(Command::DeleteTask {
            task_id: task_id.clone(),
            remove_worktree: true
        })
        .await
        .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("ignored.txt"))?,
        "important ignored data"
    );
    assert!(core.task(&task_id).is_ok());
    assert!(core.store.load_worktree_deletions().await?.is_empty());
    toasty::sql::statement("DROP TRIGGER block_task_deletion")
        .exec(&mut database_probe)
        .await?;
    core.execute(Command::DeleteTask {
        task_id: task_id.clone(),
        remove_worktree: true,
    })
    .await?;
    assert!(!worktree.exists());
    assert!(core.task(&task_id).is_err());
    assert!(core.store.load_worktree_deletions().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn committed_task_deletion_recovers_if_journal_cleanup_fails()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let uri = database.uri();
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![])]])),
    );
    let project = add_project(&mut core, fixture.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            project_id: project,
            title: "Finish deletion".into(),
            agent: Agent::Codex,
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id);
    let workspace = workspace_of(
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("hello"),
        })
        .await?,
    );
    let worktree = PathBuf::from(workspace.task.worktree_path.ok_or("missing worktree")?);
    let mut database_probe = toasty::Db::builder()
        .build(toasty_driver_turso::Turso::new(&uri)?.experimental_index_method(true))
        .await?;
    toasty::sql::statement("CREATE TRIGGER block_deletion_cleanup BEFORE DELETE ON worktree_deletions BEGIN SELECT RAISE(ABORT, 'fixture cleanup failure'); END").exec(&mut database_probe).await?;
    let error = core
        .execute(Command::DeleteTask {
            task_id: task_id.clone(),
            remove_worktree: true,
        })
        .await
        .err()
        .ok_or("expected cleanup failure")?;
    assert!(matches!(error, CoreError::TaskDeletedCleanupPending { .. }));
    assert!(core.task(&task_id).is_err());
    assert!(!worktree.exists());
    assert_eq!(core.store.load_worktree_deletions().await?.len(), 1);
    toasty::sql::statement("DROP TRIGGER block_deletion_cleanup")
        .exec(&mut database_probe)
        .await?;
    drop(core);
    let reopened = Core::load(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(UnavailableHarnessFactory),
    )
    .await?;
    assert!(!reopened.tasks.contains_key(&task_id));
    assert!(reopened.store.load_worktree_deletions().await?.is_empty());
    Ok(())
}

#[test]
fn the_task_state_machine_rejects_skipping_states() {
    let directory = TestDirectory::new().expect("temp directory");
    let mut task = Task::draft(ProjectId::new("project-1"), "Title", directory.path());

    assert!(task.set_state(TaskState::Working).is_err());
    assert!(task.set_state(TaskState::Starting).is_ok());
    assert!(task.set_state(TaskState::Ready).is_ok());
    assert!(task.set_state(TaskState::Working).is_ok());
    assert!(task.set_state(TaskState::Waiting).is_ok());
    assert!(task.set_state(TaskState::Ready).is_ok());
    assert!(task.set_state(TaskState::Working).is_ok());
    assert!(task.set_state(TaskState::Waiting).is_ok());
    assert!(task.set_state(TaskState::Review).is_ok());
    assert!(task.set_state(TaskState::Done).is_ok());
    assert!(task.set_state(TaskState::Review).is_ok());
}

#[tokio::test]
async fn quitting_with_live_work_requires_confirmation_then_interrupts() {
    let directory = TestDirectory::new().expect("temp directory");
    let mut core = Core::new();
    let project_id = add_project(&mut core, directory.path().to_path_buf())
        .await
        .expect("project should be added");
    let result = core
        .execute(Command::CreateDraft {
            agent: Agent::Codex,
            project_id,
            title: "Live work".to_string(),
        })
        .await;
    let CommandResult::TaskWorkspace(workspace) = result.expect("draft should be created") else {
        panic!("expected a task workspace");
    };
    let task_id = TaskId::new(workspace.task.id);
    if let Some(task) = core.tasks.get_mut(&task_id) {
        task.state = TaskState::Working;
    }

    let refused = core.execute(Command::Quit { force: false }).await;
    assert!(matches!(
        refused,
        Err(CoreError::QuitConfirmationRequired {
            live_tasks: 1,
            live_terminals: 0
        })
    ));
    assert_eq!(core.shutdown_state(), ShutdownState::Running);
    assert_eq!(
        core.tasks.get(&task_id).map(|task| task.state),
        Some(TaskState::Working)
    );

    let result = core.execute(Command::Quit { force: true }).await;
    let CommandResult::Quit(summary) = result.expect("force quit should succeed") else {
        panic!("expected a quit result");
    };
    assert_eq!(summary.interrupted_tasks, 1);
    assert_eq!(core.shutdown_state(), ShutdownState::Closed);
    assert_eq!(
        core.tasks.get(&task_id).map(|task| task.state),
        Some(TaskState::Interrupted)
    );

    let after_shutdown = core
        .execute(Command::AddProject {
            path: directory.path().to_path_buf(),
        })
        .await;
    assert!(matches!(
        after_shutdown,
        Err(CoreError::NotRunning {
            state: ShutdownState::Closed
        })
    ));
}

#[tokio::test]
async fn failed_session_record_save_stops_agent_and_allows_retry()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let uri = database.uri();
    let turns = || vec![FakeTurn::new(vec![FakeStep::Message("done".into())])];
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(FakeHarnessFactory::new(vec![turns(), turns()])),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            project_id,
            agent: Agent::Codex,
            title: "Retry session".into(),
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id);
    let mut database_probe = toasty::Db::builder()
        .build(toasty_driver_turso::Turso::new(&uri)?.experimental_index_method(true))
        .await?;
    toasty::sql::statement("CREATE TRIGGER block_session_start BEFORE INSERT ON sessions BEGIN SELECT RAISE(ABORT, 'session save blocked'); END")
        .exec(&mut database_probe).await?;
    let failure = core
        .execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("go"),
        })
        .await;
    assert!(failure.is_err());
    assert!(
        core.sessions
            .get(&task_id)
            .is_some_and(HarnessSession::is_closed)
    );
    assert!(core.store.load_sessions().await?.is_empty());
    toasty::sql::statement("DROP TRIGGER block_session_start")
        .exec(&mut database_probe)
        .await?;
    let workspace = workspace_of(
        core.execute(Command::SendPrompt {
            task_id,
            input: PromptInput::text("go"),
        })
        .await?,
    );
    assert!(
        summaries(&workspace)
            .iter()
            .any(|summary| summary.contains("done"))
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn killed_deletion_recovers_worktree_before_and_after_commit()
-> Result<(), Box<dyn std::error::Error>> {
    if let Some(uri) = std::env::var_os("SHEPHERD_DELETION_CRASH_DATABASE") {
        let uri = uri.into_string().map_err(|_| "database URI is not UTF-8")?;
        let repository_path = PathBuf::from(
            std::env::var_os("SHEPHERD_DELETION_CRASH_REPOSITORY").ok_or("missing repository")?,
        );
        let mut core = Core::with_services(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![])]])),
        );
        let project_id = add_project(&mut core, repository_path.clone()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                project_id,
                title: "Crash deletion".into(),
                agent: Agent::Codex,
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id);
        let workspace = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("fixture"),
            })
            .await?,
        );
        let worktree = PathBuf::from(workspace.task.worktree_path.ok_or("missing worktree")?);
        let repository = GitService::discover(&repository_path)?;
        std::fs::write(repository.git_dir.join("info/exclude"), "ignored.txt\n")?;
        std::fs::write(worktree.join("ignored.txt"), "important ignored data")?;
        core.execute(Command::DeleteTask {
            task_id,
            remove_worktree: true,
        })
        .await?;
        return Err("deletion fixture unexpectedly finished".into());
    }

    use std::os::unix::process::ExitStatusExt as _;
    for phase in ["staged", "committed"] {
        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let uri = database.uri();
        let ready = database.path().join("deletion-ready");
        let mut child = tokio::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "killed_deletion_recovers_worktree_before_and_after_commit",
                "--nocapture",
            ])
            .env("SHEPHERD_DELETION_CRASH_DATABASE", &uri)
            .env("SHEPHERD_DELETION_CRASH_REPOSITORY", repository.path())
            .env("SHEPHERD_DELETION_CRASH_READY", &ready)
            .env("SHEPHERD_DELETION_CRASH_PHASE", phase)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let task_id = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                match std::fs::read_to_string(&ready) {
                    Ok(id) if !id.is_empty() => {
                        break Ok::<_, Box<dyn std::error::Error>>(TaskId::new(id));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => break Err(error.into()),
                }
                if let Some(status) = child.try_wait()? {
                    break Err(
                        format!("deletion fixture exited before becoming ready: {status}").into(),
                    );
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        child.kill().await?;
        assert_eq!(child.wait().await?.signal(), Some(9));
        let store = TursoStore::open(&uri).await?;
        let deletion = store
            .load_worktree_deletions()
            .await?
            .into_iter()
            .next()
            .ok_or("missing deletion journal")?;
        assert_eq!(deletion.task_id, task_id);
        assert_eq!(
            std::fs::read_to_string(deletion.staged_path.join("ignored.txt"))?,
            "important ignored data"
        );
        drop(store);
        let core = Core::load(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(UnavailableHarnessFactory),
        )
        .await?;
        assert!(core.store.load_worktree_deletions().await?.is_empty());
        assert!(!deletion.staged_path.exists());
        if phase == "staged" {
            assert!(core.tasks.contains_key(&task_id));
            assert_eq!(
                std::fs::read_to_string(deletion.original_path.join("ignored.txt"))?,
                "important ignored data"
            );
        } else {
            assert!(!core.tasks.contains_key(&task_id));
            assert!(!deletion.original_path.exists());
        }
    }
    Ok(())
}

#[tokio::test]
async fn startup_recovers_staged_worktree_deletions_conservatively()
-> Result<(), Box<dyn std::error::Error>> {
    for scenario in [
        "before-move",
        "restore",
        "finish",
        "finished",
        "wrong-identity",
        "both-paths",
        "dirty",
        "locked",
    ] {
        let fixture = TestRepository::new()?;
        let repository = GitService::discover(fixture.path())?;
        let database = TestDatabase::new()?;
        let store = TursoStore::open(&database.uri()).await?;
        let project = projects::Project::new(repository.root.clone());
        let mut task = Task::draft(project.id.clone(), "Recovery", &repository.git_dir);
        let original = task.proposed_worktree_path.clone();
        GitService.create_worktree(
            &repository,
            &original,
            &task.proposed_branch,
            &repository.default_branch,
        )?;
        task.worktree_path = Some(original.clone());
        task.branch = Some(task.proposed_branch.clone());
        std::fs::write(repository.git_dir.join("info/exclude"), "ignored.txt\n")?;
        std::fs::write(original.join("ignored.txt"), "preserve me")?;
        store.save_project(&project).await?;
        store.save_task(&task).await?;
        let identity = uuid::Uuid::new_v4().to_string();
        let git_dir = GitService.mark_worktree_deletion(&repository, &original, &identity)?;
        let staged = repository
            .git_dir
            .join("shepherd/worktrees")
            .join(format!(".shepherd-deletion-{identity}"));
        let deletion = WorktreeDeletion {
            task_id: task.id.clone(),
            repository_root: repository.root.clone(),
            original_path: original.clone(),
            staged_path: staged.clone(),
            worktree_git_dir: git_dir.clone(),
            identity: identity.clone(),
        };
        store.save_worktree_deletion(&deletion).await?;
        if scenario != "before-move" {
            GitService.move_worktree(&repository, &original, &staged)?;
        }
        if matches!(scenario, "finish" | "finished" | "dirty") {
            store.delete_task(&task.id, None).await?;
        }
        if scenario == "finished" {
            GitService.remove_clean_worktree(&repository, &staged)?;
        }
        if scenario == "both-paths" {
            std::fs::create_dir(&original)?;
        }
        if scenario == "dirty" {
            std::fs::write(staged.join("new-file"), "do not remove")?;
        }
        if scenario == "wrong-identity" {
            GitService.clear_worktree_deletion_marker(&repository, &staged, &git_dir, &identity)?;
        }
        drop(store);
        let lock = if scenario == "locked" {
            Some(GitService.lock_worktree_deletions(&repository)?)
        } else {
            None
        };
        let result = Core::load(
            Box::new(TursoStore::open(&database.uri()).await?),
            Box::new(UnavailableHarnessFactory),
        )
        .await;
        if scenario == "locked" {
            assert!(result.is_err());
            assert_eq!(
                std::fs::read_to_string(staged.join("ignored.txt"))?,
                "preserve me"
            );
            drop(lock);
            let core = Core::load(
                Box::new(TursoStore::open(&database.uri()).await?),
                Box::new(UnavailableHarnessFactory),
            )
            .await?;
            assert!(core.store.load_worktree_deletions().await?.is_empty());
            assert_eq!(
                std::fs::read_to_string(original.join("ignored.txt"))?,
                "preserve me"
            );
            continue;
        }
        if matches!(scenario, "wrong-identity" | "both-paths" | "dirty") {
            assert!(result.is_err());
            assert_eq!(
                std::fs::read_to_string(staged.join("ignored.txt"))?,
                "preserve me"
            );
            continue;
        }
        let core = result?;
        assert!(core.store.load_worktree_deletions().await?.is_empty());
        if matches!(scenario, "finish" | "finished") {
            assert!(!original.exists() && !staged.exists());
            assert!(!core.tasks.contains_key(&task.id));
        } else {
            assert_eq!(
                std::fs::read_to_string(original.join("ignored.txt"))?,
                "preserve me"
            );
            assert!(!staged.exists());
            assert!(core.tasks.contains_key(&task.id));
        }
    }
    Ok(())
}

#[tokio::test]
async fn failed_unrecorded_session_cleanup_is_retried_before_new_agent()
-> Result<(), Box<dyn std::error::Error>> {
    use shep_harness_core::fake_harness::FakeHarness;
    use shep_harness_core::{AgentHarness, Capabilities, HarnessError, HarnessResult, SpawnSpec};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Factory {
        starts: Arc<AtomicUsize>,
        stops: Arc<AtomicUsize>,
    }
    impl HarnessFactory for Factory {
        fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FailingShutdown {
                inner: FakeHarness::one_turn(FakeTurn::new(vec![FakeStep::Message("done".into())])),
                stops: self.stops.clone(),
            }))
        }
    }
    struct FailingShutdown {
        inner: FakeHarness,
        stops: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl AgentHarness for FailingShutdown {
        fn events(&mut self) -> &mut mpsc::UnboundedReceiver<HarnessEvent> {
            self.inner.events()
        }
        async fn start(&mut self, spec: SpawnSpec) -> HarnessResult<Capabilities> {
            self.inner.start(spec).await
        }
        async fn open_session(&mut self, resume: Option<SessionId>) -> HarnessResult<SessionId> {
            self.inner.open_session(resume).await
        }
        async fn prompt(&mut self, input: PromptInput) -> HarnessResult<()> {
            self.inner.prompt(input).await
        }
        async fn cancel(&mut self) -> HarnessResult<()> {
            self.inner.cancel().await
        }
        async fn respond(&mut self, id: ApprovalId, decision: Decision) -> HarnessResult<()> {
            self.inner.respond(id, decision).await
        }
        async fn shutdown(&mut self) -> HarnessResult<()> {
            if self.stops.fetch_add(1, Ordering::SeqCst) < 2 {
                return Err(HarnessError::Protocol("fixture shutdown failed".into()));
            }
            self.inner.shutdown().await
        }
    }
    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let uri = database.uri();
    let starts = Arc::new(AtomicUsize::new(0));
    let stops = Arc::new(AtomicUsize::new(0));
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(Factory {
            starts: starts.clone(),
            stops: stops.clone(),
        }),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            project_id,
            agent: Agent::Codex,
            title: "Retry cleanup".into(),
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id);
    let mut database_probe = toasty::Db::builder()
        .build(toasty_driver_turso::Turso::new(&uri)?.experimental_index_method(true))
        .await?;
    toasty::sql::statement("CREATE TRIGGER block_session_start BEFORE INSERT ON sessions BEGIN SELECT RAISE(ABORT, 'session save blocked'); END").exec(&mut database_probe).await?;
    let prompt = || Command::SendPrompt {
        task_id: task_id.clone(),
        input: PromptInput::text("go"),
    };
    let error = core
        .execute(prompt())
        .await
        .err()
        .ok_or("expected save and cleanup failure")?;
    assert!(matches!(error, CoreError::SessionRecordCleanup { .. }));
    assert!(
        core.sessions
            .get(&task_id)
            .is_some_and(|session| session.has_failed() && !session.is_closed())
    );
    toasty::sql::statement("DROP TRIGGER block_session_start")
        .exec(&mut database_probe)
        .await?;
    assert!(core.execute(prompt()).await.is_err());
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    let workspace = workspace_of(core.execute(prompt()).await?);
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    assert_eq!(stops.load(Ordering::SeqCst), 3);
    assert!(
        summaries(&workspace)
            .iter()
            .any(|summary| summary.contains("done"))
    );
    Ok(())
}

#[tokio::test]
async fn quit_retries_agent_shutdown_after_failure() -> Result<(), Box<dyn std::error::Error>> {
    use shep_harness_core::{AgentHarness, Capabilities, HarnessError, HarnessResult, SpawnSpec};
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct RetryShutdown {
        receiver: mpsc::UnboundedReceiver<HarnessEvent>,
        attempts: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl AgentHarness for RetryShutdown {
        fn events(&mut self) -> &mut mpsc::UnboundedReceiver<HarnessEvent> {
            &mut self.receiver
        }
        async fn start(&mut self, _: SpawnSpec) -> HarnessResult<Capabilities> {
            Err(HarnessError::NotStarted)
        }
        async fn open_session(&mut self, _: Option<SessionId>) -> HarnessResult<SessionId> {
            Err(HarnessError::NotStarted)
        }
        async fn prompt(&mut self, _: PromptInput) -> HarnessResult<()> {
            Err(HarnessError::NotStarted)
        }
        async fn cancel(&mut self) -> HarnessResult<()> {
            Err(HarnessError::NotStarted)
        }
        async fn respond(&mut self, _: ApprovalId, _: Decision) -> HarnessResult<()> {
            Err(HarnessError::NotStarted)
        }
        async fn shutdown(&mut self) -> HarnessResult<()> {
            if self.attempts.fetch_add(1, Ordering::SeqCst) < 4 {
                Err(HarnessError::Protocol("fixture shutdown failed".into()))
            } else {
                Ok(())
            }
        }
    }
    let directory = TestDirectory::new()?;
    let mut core = Core::with_services(
        Box::new(MemoryStore::new()),
        Box::new(UnavailableHarnessFactory),
    );
    let project_id = add_project(&mut core, directory.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            project_id,
            agent: Agent::Codex,
            title: "Retry quit".into(),
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id);
    let (sender, receiver) = mpsc::unbounded_channel();
    let attempts = Arc::new(AtomicUsize::new(0));
    let mut session = HarnessSession::new(Box::new(RetryShutdown {
        receiver,
        attempts: attempts.clone(),
    }));
    sender.send(HarnessEvent::ConnectionFailed {
        kind: FailureKind::Protocol,
        text: "fixture disconnect".into(),
    })?;
    session.pump();
    assert!(session.is_closed());
    assert!(session.needs_shutdown());
    core.sessions.insert(task_id.clone(), session);
    assert!(core.ensure_session(&task_id, None).await.is_err());
    assert!(core.sessions.contains_key(&task_id));
    assert!(
        core.execute(Command::DeleteTask {
            task_id: task_id.clone(),
            remove_worktree: false
        })
        .await
        .is_err()
    );
    assert!(core.task(&task_id).is_ok());
    core.tasks.get_mut(&task_id).ok_or("missing task")?.state = TaskState::Review;
    assert!(core.archive_reviewed(vec![task_id.clone()]).await.is_err());
    assert_eq!(core.task(&task_id)?.state, TaskState::Review);
    assert!(core.execute(Command::Quit { force: true }).await.is_err());
    assert_eq!(core.shutdown_state(), ShutdownState::Draining);
    assert!(core.sessions.contains_key(&task_id));
    assert!(!core.shutdown_events.contains_key(&task_id));
    assert!(matches!(
        core.execute(Command::Quit { force: true }).await?,
        CommandResult::Quit(_)
    ));
    assert_eq!(attempts.load(Ordering::SeqCst), 5);
    assert_eq!(core.shutdown_state(), ShutdownState::Closed);
    Ok(())
}

#[tokio::test]
async fn failed_shutdown_persistence_keeps_the_session_for_retry()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let mut core = Core::with_services(
        Box::new(MemoryStore::new()),
        Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
            FakeStep::Message("working".into()),
            FakeStep::approval("approval", "Run tests"),
        ])]])),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            project_id,
            agent: Agent::Codex,
            title: "Retry shutdown".into(),
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id);
    let workspace = workspace_of(
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("go"),
        })
        .await?,
    );
    assert_eq!(workspace.task.state, TaskState::Waiting);
    core.sessions
        .get_mut(&task_id)
        .ok_or("missing session")?
        .restore_transcript(workspace.transcript);
    let record = core
        .session_records
        .get_mut(&task_id)
        .ok_or("missing session record")?;
    let original_id = std::mem::replace(&mut record.id, "missing-session".into());
    assert!(core.execute(Command::Quit { force: true }).await.is_err());
    assert_eq!(core.shutdown_state(), ShutdownState::Draining);
    assert_eq!(core.task(&task_id)?.state, TaskState::Waiting);
    assert!(core.sessions.contains_key(&task_id));
    assert!(matches!(
        core.execute(Command::AddProject {
            path: repository.path().to_path_buf()
        })
        .await,
        Err(CoreError::NotRunning {
            state: ShutdownState::Draining
        })
    ));
    core.session_records
        .get_mut(&task_id)
        .ok_or("missing session record")?
        .id = original_id;
    let CommandResult::Quit(summary) = core.execute(Command::Quit { force: true }).await? else {
        return Err("expected quit summary".into());
    };
    assert_eq!(summary.interrupted_tasks, 1);
    assert_eq!(core.task(&task_id)?.state, TaskState::Interrupted);
    assert!(core.sessions.is_empty());
    let saved = core.store.load_tasks().await?;
    assert_eq!(
        saved.first().ok_or("missing task")?.state,
        TaskState::Interrupted
    );
    assert!(!core.store.load_transcript(&task_id).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn question_answer_resumes_the_turn_and_persists_the_transcript()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let mut core = Core::with_services(
        Box::new(MemoryStore::new()),
        Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
            FakeStep::Question {
                question_id: "question-1".into(),
                text: "Which target?".into(),
            },
            FakeStep::Message("Continuing".into()),
        ])]])),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            project_id,
            agent: Agent::Codex,
            title: "Question".into(),
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id);
    let waiting = workspace_of(
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("go"),
        })
        .await?,
    );
    assert_eq!(waiting.task.state, TaskState::Waiting);
    assert!(
        core.execute(Command::AnswerQuestion {
            task_id: task_id.clone(),
            question_id: "wrong".into(),
            answer: "debug".into(),
        })
        .await
        .is_err()
    );
    let finished = workspace_of(
        core.execute(Command::AnswerQuestion {
            task_id: task_id.clone(),
            question_id: "question-1".into(),
            answer: "debug".into(),
        })
        .await?,
    );
    assert_eq!(finished.task.state, TaskState::Ready);
    assert!(finished.transcript.iter().any(|entry| matches!(&entry.kind,
        shep_harness_core::EntryKind::Question { question_id, answer: Some(answer), .. }
            if question_id == "question-1" && answer == "debug")));
    assert_eq!(
        core.store.load_transcript(&task_id).await?,
        finished.transcript
    );
    Ok(())
}

#[tokio::test]
async fn quitting_idle_work_is_immediate() {
    let mut core = Core::new();
    let result = core.execute(Command::Quit { force: false }).await;
    let CommandResult::Quit(summary) = result.expect("idle quit should succeed") else {
        panic!("expected a quit result");
    };
    assert_eq!(summary.interrupted_tasks, 0);
    assert_eq!(core.shutdown_state(), ShutdownState::Closed);
}

#[tokio::test]
async fn a_task_left_waiting_is_interrupted_by_the_next_load()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let uri = database.uri();

    let first_run = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
            FakeStep::approval("approval-1", "Run tests"),
        ])]])),
    );
    let mut scenario = TestCore::with_core(first_run);
    let project_id = add_project(scenario.core_mut(), repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        scenario
            .execute(Command::CreateDraft {
                agent: Agent::Codex,
                project_id,
                title: "Survive a crash".to_string(),
            })
            .await?,
    );
    let task_id = TaskId::new(draft.task.id.clone());
    let workspace = workspace_of(
        scenario
            .execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("go"),
            })
            .await?,
    );
    assert_eq!(workspace.task.state, TaskState::Waiting);

    // No quit: the process disappears while the task waits for approval.
    drop(scenario);

    let reopened = Core::load(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(UnavailableHarnessFactory),
    )
    .await?;
    let workspace = reopened.task_workspace(&task_id)?;
    // The record survives and is no longer live; the transcript belongs to
    // the session that vanished, so it comes back empty.
    assert_eq!(workspace.task.state, TaskState::Interrupted);
    assert!(workspace.transcript.is_empty());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn abrupt_process_death_recovers_the_waiting_task() -> Result<(), Box<dyn std::error::Error>>
{
    if let Some(uri) = std::env::var_os("SHEPHERD_CRASH_FIXTURE_DATABASE") {
        let uri = uri.into_string().map_err(|_| "database URI is not UTF-8")?;
        let repository = PathBuf::from(
            std::env::var_os("SHEPHERD_CRASH_FIXTURE_REPOSITORY").ok_or("missing repository")?,
        );
        let ready = PathBuf::from(
            std::env::var_os("SHEPHERD_CRASH_FIXTURE_READY").ok_or("missing ready path")?,
        );
        let mut core = Core::with_services(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
                FakeStep::Message("unfinished answer".into()),
                FakeStep::TerminalError {
                    id: "crash-terminal".into(),
                    text: "Agent terminal error: missing exit status".into(),
                },
                FakeStep::approval("crash-approval", "Run tests"),
            ])]])),
        );
        let project = add_project(&mut core, repository).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                agent: Agent::Codex,
                project_id: project,
                title: "Crash fixture".into(),
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id);
        let waiting = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("persist this prompt"),
            })
            .await?,
        );
        assert_eq!(waiting.task.state, TaskState::Waiting);
        let temporary = ready.with_extension("tmp");
        std::fs::write(&temporary, task_id.as_str())?;
        std::fs::rename(temporary, ready)?;
        std::future::pending::<()>().await;
        return Ok(());
    }

    use std::os::unix::process::ExitStatusExt as _;
    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let uri = database.uri();
    let ready = database.path().join("crash-fixture-ready");
    let mut child = tokio::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "abrupt_process_death_recovers_the_waiting_task",
            "--nocapture",
        ])
        .env("SHEPHERD_CRASH_FIXTURE_DATABASE", &uri)
        .env("SHEPHERD_CRASH_FIXTURE_REPOSITORY", repository.path())
        .env("SHEPHERD_CRASH_FIXTURE_READY", &ready)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let task_id = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            match std::fs::read_to_string(&ready) {
                Ok(id) => break Ok::<_, Box<dyn std::error::Error>>(TaskId::new(id)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => break Err(error.into()),
            }
            if let Some(status) = child.try_wait()? {
                break Err(format!("crash fixture exited before becoming ready: {status}").into());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await??;
    child.kill().await?;
    let status = child.wait().await?;
    assert_eq!(status.signal(), Some(9));
    let mut recovered = Core::load(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
            FakeStep::Message("answer after crash".into()),
        ])]])),
    )
    .await?;
    let workspace = recovered.open_task_workspace(&task_id).await?;
    assert_eq!(workspace.task.state, TaskState::Interrupted);
    let text = summaries(&workspace).join("\n");
    assert!(text.contains("persist this prompt"));
    assert!(workspace.transcript.iter().any(|entry| matches!(&entry.kind, shep_harness_core::EntryKind::Notice { text, is_error: true } if text.contains("missing exit status"))));
    assert!(!text.contains("unfinished answer"));
    assert!(!recovered.sessions.contains_key(&task_id));
    let workspace = workspace_of(
        recovered
            .execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("continue after crash"),
            })
            .await?,
    );
    assert!(workspace.transcript.iter().any(|entry| matches!(&entry.kind, shep_harness_core::EntryKind::Notice { text, is_error: true } if text.contains("missing exit status"))));
    assert!(
        summaries(&workspace)
            .iter()
            .any(|entry| entry.contains("answer after crash"))
    );
    let sequences: Vec<_> = workspace
        .transcript
        .iter()
        .map(|entry| entry.sequence)
        .collect();
    assert!(
        sequences
            .windows(2)
            .all(|pair| matches!(pair, [first, second] if first < second))
    );
    drop(recovered);
    let mut reopened = Core::load(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(UnavailableHarnessFactory),
    )
    .await?;
    let workspace = reopened.open_task_workspace(&task_id).await?;
    assert!(workspace.transcript.iter().any(|entry| matches!(&entry.kind, shep_harness_core::EntryKind::Notice { text, is_error: true } if text.contains("missing exit status"))));
    assert!(
        summaries(&workspace)
            .iter()
            .any(|entry| entry.contains("answer after crash"))
    );
    Ok(())
}

#[tokio::test]
async fn a_fake_task_runs_through_approval_quit_and_resume()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let uri = database.uri();

    let first_run = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        // One session script with two turns: the second prompt reuses the
        // same worktree and agent session as the first.
        Box::new(FakeHarnessFactory::new(vec![vec![
            FakeTurn::new(vec![
                FakeStep::Message("working on it".to_string()),
                FakeStep::approval("approval-1", "Run tests"),
                FakeStep::Message("all done".to_string()),
            ]),
            FakeTurn::new(vec![FakeStep::approval("approval-2", "Write the file")]),
        ]])),
    );
    let mut scenario = TestCore::with_core(first_run);

    let project_id = add_project(scenario.core_mut(), repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        scenario
            .execute(Command::CreateDraft {
                agent: Agent::Codex,
                project_id: project_id.clone(),
                title: "Fix the parser".to_string(),
            })
            .await?,
    );
    assert_eq!(draft.task.state, TaskState::Draft);
    assert!(draft.task.worktree_path.is_none());
    assert!(!PathBuf::from(&draft.task.proposed_worktree_path).exists());
    let task_id = TaskId::new(draft.task.id.clone());

    let workspace = workspace_of(
        scenario
            .execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("fix it"),
            })
            .await?,
    );
    assert_eq!(workspace.task.state, TaskState::Waiting);
    let worktree_path = PathBuf::from(
        workspace
            .task
            .worktree_path
            .as_deref()
            .ok_or("missing worktree")?,
    );
    assert_eq!(scenario.trace().matches("event: TaskNeedsInput").count(), 1);
    scenario.core_mut().pump().await?;
    assert!(
        !scenario
            .core_mut()
            .drain_events()
            .iter()
            .any(|event| matches!(event, Event::TaskNeedsInput { .. }))
    );
    assert_eq!(
        scenario.core().waiting_task_title(&task_id)?,
        Some("Fix the parser")
    );
    assert_eq!(
        scenario
            .core()
            .project_snapshot(scenario.core().project(&project_id)?)
            .running_task_count,
        1
    );
    assert_eq!(
        workspace.task.branch.as_deref(),
        Some("shepherd/fix-the-parser")
    );
    assert_eq!(workspace.task.base_branch.as_deref(), Some("main"));
    assert!(worktree_path.join("README.md").is_file());
    assert_eq!(
        summaries(&workspace),
        vec![
            "you: fix it".to_string(),
            "agent: working on it".to_string(),
            "approval: Run tests (pending)".to_string(),
        ]
    );
    assert_eq!(
        workspace
            .transcript
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(workspace.transcript[0].id, "entry-0");

    let workspace = workspace_of(
        scenario
            .execute(Command::ResolveApproval {
                task_id: task_id.clone(),
                approval_id: ApprovalId::new("approval-1"),
                decision: Decision::Allow,
            })
            .await?,
    );
    assert_eq!(workspace.task.state, TaskState::Review);
    assert_eq!(scenario.core().waiting_task_title(&task_id)?, None);
    assert_eq!(
        summaries(&workspace),
        vec![
            "you: fix it".to_string(),
            "agent: working on it".to_string(),
            "approval: Run tests (Allow)".to_string(),
            "agent: all done".to_string(),
        ]
    );

    let workspace = workspace_of(
        scenario
            .execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("go again"),
            })
            .await?,
    );
    assert_eq!(workspace.task.state, TaskState::Waiting);
    assert_eq!(scenario.trace().matches("event: TaskNeedsInput").count(), 2);

    let refused = scenario.execute(Command::Quit { force: false }).await;
    assert!(matches!(
        refused,
        Err(CoreError::QuitConfirmationRequired {
            live_tasks: 1,
            live_terminals: 0
        })
    ));
    let result = scenario.execute(Command::Quit { force: true }).await?;
    let CommandResult::Quit(summary) = result else {
        panic!("expected a quit result");
    };
    assert_eq!(summary.interrupted_tasks, 1);
    drop(scenario);

    let reopened = Core::load(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
            FakeStep::Message("resumed answer".to_string()),
        ])]])),
    )
    .await?;
    let mut scenario = TestCore::with_core(reopened);

    let workspace = scenario.core_mut().open_task_workspace(&task_id).await?;
    assert_eq!(workspace.task.state, TaskState::Interrupted);
    assert!(
        workspace
            .capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.resume)
    );
    assert_eq!(
        summaries(&workspace),
        [
            "you: fix it",
            "agent: working on it",
            "approval: Run tests (Allow)",
            "agent: all done",
            "you: go again",
            "approval: Write the file (pending)",
        ]
    );
    let saved = workspace.transcript;
    assert!(worktree_path.join("README.md").is_file());
    assert_eq!(workspace.project.unfinished_task_count, 1);

    let workspace = workspace_of(
        scenario
            .execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("continue"),
            })
            .await?,
    );
    assert_eq!(workspace.task.state, TaskState::Ready);
    assert!(workspace.transcript.starts_with(&saved));
    assert!(workspace.transcript.iter().any(|entry| {
        entry
            .summary()
            .contains("resumed the previous agent session")
    }));
    assert_eq!(
        workspace.transcript.last().map(Entry::summary),
        Some("agent: resumed answer".to_string())
    );
    let store = TursoStore::open(&uri).await?;
    assert_eq!(store.load_transcript(&task_id).await?, workspace.transcript);
    Ok(())
}

#[tokio::test]
async fn approval_errors_fold_queued_disconnect_and_superseding_request()
-> Result<(), Box<dyn std::error::Error>> {
    use shep_harness_core::{
        AgentHarness, ApprovalRequest, Capabilities, DeltaKind, FailureKind, HarnessError,
        HarnessResult, ItemId, SpawnSpec, ToolCallId, TurnId,
    };
    use tokio::sync::mpsc;
    struct Factory {
        stale: bool,
        withdrawn: bool,
    }
    impl HarnessFactory for Factory {
        fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
            let (sender, receiver) = mpsc::unbounded_channel();
            Ok(Box::new(FailingApproval {
                sender,
                receiver,
                stale: self.stale,
                withdrawn: self.withdrawn,
            }))
        }
        fn launch(
            &self,
            _: Agent,
            cwd: PathBuf,
        ) -> Result<(Box<dyn AgentHarness>, SpawnSpec), CoreError> {
            Ok((self.create()?, self.spawn_spec(cwd)))
        }
    }
    struct FailingApproval {
        sender: mpsc::UnboundedSender<HarnessEvent>,
        receiver: mpsc::UnboundedReceiver<HarnessEvent>,
        stale: bool,
        withdrawn: bool,
    }
    #[async_trait::async_trait]
    impl AgentHarness for FailingApproval {
        fn events(&mut self) -> &mut mpsc::UnboundedReceiver<HarnessEvent> {
            &mut self.receiver
        }
        async fn start(&mut self, _: SpawnSpec) -> HarnessResult<Capabilities> {
            Ok(Capabilities::default())
        }
        async fn open_session(&mut self, _: Option<SessionId>) -> HarnessResult<SessionId> {
            Ok(SessionId::new("fixture"))
        }
        async fn prompt(&mut self, _: PromptInput) -> HarnessResult<()> {
            self.sender
                .send(HarnessEvent::TurnStarted {
                    turn_id: TurnId::new("turn"),
                })
                .map_err(|_| HarnessError::ChannelClosed)?;
            self.sender
                .send(HarnessEvent::ApprovalRequested {
                    request: ApprovalRequest {
                        approval_id: ApprovalId::new("approval"),
                        tool_call_id: ToolCallId::new("tool"),
                        title: "Run tests".into(),
                        arguments: serde_json::Value::Null,
                        paths: Vec::new(),
                        patch: None,
                        choices: None,
                        stream_permit: None,
                    },
                })
                .map_err(|_| HarnessError::ChannelClosed)?;
            Ok(())
        }
        async fn respond(&mut self, _: ApprovalId, _: Decision) -> HarnessResult<()> {
            if self.stale {
                let settled = if self.withdrawn {
                    HarnessEvent::ApprovalWithdrawn {
                        approval_id: ApprovalId::new("approval"),
                    }
                } else {
                    HarnessEvent::ApprovalResolved {
                        approval_id: ApprovalId::new("approval"),
                        decision: Decision::Deny,
                        source: shep_harness_core::ApprovalSource::Agent,
                    }
                };
                self.sender
                    .send(settled)
                    .map_err(|_| HarnessError::ChannelClosed)?;
                self.sender
                    .send(HarnessEvent::ApprovalRequested {
                        request: ApprovalRequest {
                            approval_id: ApprovalId::new("next"),
                            tool_call_id: ToolCallId::new("next-tool"),
                            title: "Next request".into(),
                            arguments: serde_json::Value::Null,
                            paths: Vec::new(),
                            patch: None,
                            choices: None,
                            stream_permit: None,
                        },
                    })
                    .map_err(|_| HarnessError::ChannelClosed)?;
                return Err(HarnessError::NoPendingApproval);
            }
            for _ in 0..300 {
                self.sender
                    .send(HarnessEvent::Delta {
                        item_id: ItemId::new("stream"),
                        kind: DeltaKind::Message,
                        text: "x".into(),
                        stream_permit: None,
                    })
                    .map_err(|_| HarnessError::ChannelClosed)?;
            }
            self.sender
                .send(HarnessEvent::ApprovalWithdrawn {
                    approval_id: ApprovalId::new("approval"),
                })
                .map_err(|_| HarnessError::ChannelClosed)?;
            self.sender
                .send(HarnessEvent::ConnectionFailed {
                    kind: FailureKind::Protocol,
                    text: "approval delivery failed".into(),
                })
                .map_err(|_| HarnessError::ChannelClosed)?;
            Err(HarnessError::Reported {
                kind: FailureKind::Protocol,
                message: "approval delivery failed".into(),
            })
        }
        async fn cancel(&mut self) -> HarnessResult<()> {
            Ok(())
        }
        async fn shutdown(&mut self) -> HarnessResult<()> {
            Ok(())
        }
    }
    for (stale, was_withdrawn) in [(false, false), (true, false), (true, true)] {
        let repository = TestRepository::new()?;
        let database = TestDatabase::new()?;
        let uri = database.uri();
        let mut core = Core::with_services(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(Factory {
                stale,
                withdrawn: was_withdrawn,
            }),
        );
        let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                project_id,
                agent: Agent::OpenCode,
                title: "Approval failure".into(),
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id);
        let workspace = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("go"),
            })
            .await?,
        );
        assert_eq!(workspace.task.state, TaskState::Waiting);
        let error = core
            .execute(Command::ResolveApproval {
                task_id: task_id.clone(),
                approval_id: ApprovalId::new("approval"),
                decision: Decision::Allow,
            })
            .await;
        assert!(error.is_err());
        let workspace = core.task_workspace(&task_id)?;
        if stale {
            assert!(matches!(
                error,
                Err(CoreError::Harness(HarnessError::NoPendingApproval))
            ));
            assert_eq!(workspace.task.state, TaskState::Waiting);
            assert_eq!(
                core.sessions
                    .get(&task_id)
                    .and_then(HarnessSession::pending_approval)
                    .map(ApprovalId::as_str),
                Some("next")
            );
            assert!(workspace.transcript.iter().any(|entry| matches!(&entry.kind, shep_harness_core::EntryKind::Permission { approval_id, decision, withdrawn, .. } if approval_id == "approval" && *withdrawn == was_withdrawn && (was_withdrawn || *decision == Some(Decision::Deny)))));
            assert!(workspace.transcript.iter().any(|entry| matches!(&entry.kind, shep_harness_core::EntryKind::Permission { approval_id, decision: None, .. } if approval_id == "next")));
            let store = TursoStore::open(&uri).await?;
            let saved = store.load_transcript(&task_id).await?;
            assert!(saved.iter().any(|entry| matches!(&entry.kind, shep_harness_core::EntryKind::Permission { approval_id, decision, withdrawn, .. } if approval_id == "approval" && *withdrawn == was_withdrawn && (was_withdrawn || *decision == Some(Decision::Deny)))));
            assert!(!saved.iter().any(|entry| matches!(&entry.kind, shep_harness_core::EntryKind::Permission { approval_id, .. } if approval_id == "next")));
            drop(store);
            drop(core);
            let mut reopened = Core::load(
                Box::new(TursoStore::open(&uri).await?),
                Box::new(Factory {
                    stale,
                    withdrawn: was_withdrawn,
                }),
            )
            .await?;
            let restored = reopened.open_task_workspace(&task_id).await?;
            assert_eq!(restored.task.state, TaskState::Interrupted);
            assert!(restored.transcript.iter().any(|entry| matches!(&entry.kind, shep_harness_core::EntryKind::Permission { approval_id, decision, withdrawn, .. } if approval_id == "approval" && *withdrawn == was_withdrawn && (was_withdrawn || *decision == Some(Decision::Deny)))));
            assert!(!restored.transcript.iter().any(|entry| matches!(&entry.kind, shep_harness_core::EntryKind::Permission { approval_id, .. } if approval_id == "next")));
            continue;
        }
        assert_eq!(workspace.task.state, TaskState::Failed);
        assert!(workspace.transcript.iter().any(|entry| matches!(&entry.kind, shep_harness_core::EntryKind::Permission { approval_id, withdrawn: true, .. } if approval_id == "approval")));
        assert_eq!(workspace.transcript.iter().filter(|entry| matches!(&entry.kind, shep_harness_core::EntryKind::Failure { text, .. } if text == "approval delivery failed")).count(), 1);
        assert!(
            core.sessions
                .get(&task_id)
                .is_some_and(HarnessSession::is_closed)
        );
        drop(core);
        let mut reopened = Core::load(
            Box::new(TursoStore::open(&uri).await?),
            Box::new(Factory {
                stale,
                withdrawn: was_withdrawn,
            }),
        )
        .await?;
        let restored = reopened.open_task_workspace(&task_id).await?;
        assert_eq!(restored.task.state, TaskState::Failed);
        assert!(restored.transcript.iter().any(|entry| matches!(&entry.kind, shep_harness_core::EntryKind::Permission { approval_id, withdrawn: true, .. } if approval_id == "approval")));
    }
    Ok(())
}

#[tokio::test]
async fn denying_an_approval_ends_the_turn() -> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let mut core = Core::with_services(
        Box::new(MemoryStore::new()),
        Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
            FakeStep::approval("approval-1", "Run tests"),
            FakeStep::Message("must not appear".to_string()),
        ])]])),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            agent: Agent::Codex,
            project_id,
            title: "Deny me".to_string(),
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id.clone());
    let workspace = workspace_of(
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("go"),
        })
        .await?,
    );
    assert_eq!(workspace.task.state, TaskState::Waiting);

    let workspace = workspace_of(
        core.execute(Command::ResolveApproval {
            task_id,
            approval_id: ApprovalId::new("approval-1"),
            decision: Decision::Deny,
        })
        .await?,
    );
    assert_eq!(workspace.task.state, TaskState::Ready);
    assert_eq!(
        summaries(&workspace),
        vec![
            "you: go".to_string(),
            "approval: Run tests (Deny)".to_string(),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn compacting_a_ready_conversation_reaches_the_adapter()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let mut core = Core::with_services(
        Box::new(MemoryStore::new()),
        Box::new(
            FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![FakeStep::Message(
                "done".to_string(),
            )])]])
            .with_compact(),
        ),
    );

    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            agent: Agent::Codex,
            project_id,
            title: "Compact me".to_string(),
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id.clone());
    let workspace = workspace_of(
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("go"),
        })
        .await?,
    );
    assert_eq!(workspace.task.state, TaskState::Ready);

    let workspace = workspace_of(
        core.execute(Command::CompactTask {
            task_id: task_id.clone(),
        })
        .await?,
    );
    assert_eq!(workspace.task.state, TaskState::Ready);
    assert_eq!(
        summaries(&workspace),
        vec!["you: go".to_string(), "agent: done".to_string()]
    );
    Ok(())
}

#[tokio::test]
async fn cancelling_keeps_the_worktree_and_the_session() -> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let mut core = Core::with_services(
        Box::new(MemoryStore::new()),
        Box::new(FakeHarnessFactory::new(vec![vec![
            FakeTurn::new(vec![FakeStep::approval("approval-1", "Run tests")]),
            FakeTurn::new(vec![FakeStep::Message("after cancel".to_string())]),
        ]])),
    );

    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            agent: Agent::Codex,
            project_id,
            title: "Cancel me".to_string(),
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id.clone());

    let workspace = workspace_of(
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("go"),
        })
        .await?,
    );
    assert_eq!(workspace.task.state, TaskState::Waiting);
    let worktree_path = PathBuf::from(
        workspace
            .task
            .worktree_path
            .clone()
            .expect("the first send should create a worktree"),
    );

    let workspace = workspace_of(
        core.execute(Command::CancelTask {
            task_id: task_id.clone(),
        })
        .await?,
    );
    assert_eq!(workspace.task.state, TaskState::Ready);
    assert!(workspace.task.worktree_path.is_some());
    assert!(worktree_path.is_dir());
    assert!(summaries(&workspace).contains(&"approval: Run tests (Deny)".to_string()));

    let workspace = workspace_of(
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("try again"),
        })
        .await?,
    );
    assert_eq!(workspace.task.state, TaskState::Ready);
    assert_eq!(
        workspace.transcript.last().map(Entry::summary),
        Some("agent: after cancel".to_string())
    );
    Ok(())
}

#[tokio::test]
async fn later_turns_compare_content_in_an_already_dirty_worktree()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let edit = |contents: &str| {
        FakeTurn::new(vec![FakeStep::WriteFile {
            path: "README.md".into(),
            contents: contents.into(),
        }])
    };
    let mut core = Core::with_services(
        Box::new(MemoryStore::new()),
        Box::new(FakeHarnessFactory::new(vec![vec![
            FakeTurn::new(vec![
                FakeStep::WriteFile {
                    path: "README.md".into(),
                    contents: "temporary\n".into(),
                },
                FakeStep::WriteFile {
                    path: "README.md".into(),
                    contents: "initial\n".into(),
                },
            ]),
            edit("first edit\n"),
            FakeTurn::new(vec![]),
            edit("second edit\n"),
        ]])),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            agent: Agent::Codex,
            project_id,
            title: "Iterate on an edit".into(),
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id);
    for (text, expected) in [
        ("Try an edit and restore it", TaskState::Ready),
        ("Edit", TaskState::Review),
        ("Explain", TaskState::Ready),
        ("Edit again", TaskState::Review),
    ] {
        let workspace = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text(text),
            })
            .await?,
        );
        assert_eq!(workspace.task.state, expected, "{text}");
    }
    Ok(())
}

#[tokio::test]
async fn a_queued_review_note_waits_for_the_current_turn() -> Result<(), Box<dyn std::error::Error>>
{
    let repository = TestRepository::new()?;
    let mut core = Core::with_services(
        Box::new(MemoryStore::new()),
        Box::new(FakeHarnessFactory::new(vec![vec![
            FakeTurn::new(vec![
                FakeStep::approval("permission", "Edit the readme"),
                FakeStep::WriteFile {
                    path: "README.md".into(),
                    contents: "first pass\n".into(),
                },
            ]),
            FakeTurn::new(vec![]),
        ]])),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            agent: Agent::Codex,
            project_id,
            title: "Review notes".into(),
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id);
    let waiting = workspace_of(
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("Edit the readme"),
        })
        .await?,
    );
    assert_eq!(waiting.task.state, TaskState::Waiting);

    assert!(matches!(
        core.execute(Command::ReviewTask {
            task_id: task_id.clone(),
            action: ReviewAction::Reopen,
        })
        .await,
        Err(CoreError::TaskNotArchived {
            state: "waiting",
            ..
        })
    ));
    assert_eq!(core.task(&task_id)?.state, TaskState::Waiting);

    let queued = workspace_of(
        core.execute(Command::ReviewTask {
            task_id: task_id.clone(),
            action: ReviewAction::SendNote {
                text: "Explain the changes".into(),
            },
        })
        .await?,
    );
    assert_eq!(
        queued.queued_review_note.as_deref(),
        Some("Explain the changes")
    );
    assert_eq!(queued.task.state, TaskState::Waiting);
    core.pump().await?;
    assert!(core.task_workspace(&task_id)?.queued_review_note.is_some());

    core.execute(Command::ResolveApproval {
        task_id: task_id.clone(),
        approval_id: ApprovalId::new("permission"),
        decision: Decision::Allow,
    })
    .await?;
    core.pump().await?;
    let finished = core.task_workspace(&task_id)?;
    assert!(finished.queued_review_note.is_none());
    assert_eq!(finished.task.state, TaskState::Ready);
    assert!(
        summaries(&finished)
            .iter()
            .any(|entry| { entry.contains("Explain the changes") && entry.contains("README.md") })
    );
    Ok(())
}

#[tokio::test]
async fn a_completed_turn_with_changes_enters_review() -> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let mut core = Core::with_services(
        Box::new(MemoryStore::new()),
        Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
            FakeStep::WriteFile {
                path: PathBuf::from("notes.txt"),
                contents: "hello\n".to_string(),
            },
        ])]])),
    );

    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            agent: Agent::Codex,
            project_id,
            title: "Change a file".to_string(),
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id.clone());

    let workspace = workspace_of(
        core.execute(Command::SendPrompt {
            task_id,
            input: PromptInput::text("write it"),
        })
        .await?,
    );
    assert_eq!(workspace.task.state, TaskState::Review);
    let worktree_path = PathBuf::from(
        workspace
            .task
            .worktree_path
            .expect("a changed turn needs a worktree"),
    );
    assert!(worktree_path.join("notes.txt").is_file());
    assert!(!repository.path().join("notes.txt").exists());
    Ok(())
}

#[tokio::test]
async fn batch_archives_review_tasks_across_projects_and_rejects_a_stale_selection()
-> Result<(), Box<dyn std::error::Error>> {
    let first_repository = TestRepository::new()?;
    let second_repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let uri = database.uri();
    let changing_turn = || {
        vec![FakeTurn::new(vec![FakeStep::WriteFile {
            path: "notes.txt".into(),
            contents: "A change under review\n".into(),
        }])]
    };
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(FakeHarnessFactory::new(vec![
            changing_turn(),
            changing_turn(),
        ])),
    );
    let first_project = add_project(&mut core, first_repository.path().into()).await?;
    let second_project = add_project(&mut core, second_repository.path().into()).await?;
    let mut review_ids = Vec::new();
    for project_id in [first_project.clone(), second_project] {
        let draft = workspace_of(
            core.execute(Command::CreateDraft {
                project_id,
                title: "Edit notes".into(),
                agent: Agent::Codex,
            })
            .await?,
        );
        let task_id = TaskId::new(draft.task.id);
        let completed = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("Edit notes"),
            })
            .await?,
        );
        assert_eq!(completed.task.state, TaskState::Review);
        review_ids.push(task_id);
    }
    assert_eq!(core.review_task_lists().len(), 2);

    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            project_id: first_project,
            title: "Unsaved work".into(),
            agent: Agent::Codex,
        })
        .await?,
    );
    let mut stale_selection = review_ids.clone();
    stale_selection.push(TaskId::new(draft.task.id));
    assert!(
        core.execute(Command::ArchiveReviewed {
            task_ids: stale_selection
        })
        .await
        .is_err()
    );
    for task_id in &review_ids {
        assert_eq!(core.task(task_id)?.state, TaskState::Review);
        assert!(
            core.sessions
                .get(task_id)
                .is_some_and(|session| !session.is_closed())
        );
    }
    assert_eq!(
        core.store
            .load_tasks()
            .await?
            .iter()
            .filter(|task| task.state == TaskState::Review)
            .count(),
        2
    );

    let mut database_probe = toasty::Db::builder()
        .build(toasty_driver_turso::Turso::new(&uri)?.experimental_index_method(true))
        .await?;
    toasty::sql::statement(
        "CREATE TRIGGER block_session_close BEFORE UPDATE ON sessions \
         WHEN NEW.state = 'ended' BEGIN SELECT RAISE(ABORT, 'session write blocked'); END",
    )
    .exec(&mut database_probe)
    .await?;
    let error = core
        .execute(Command::ArchiveReviewed {
            task_ids: review_ids.clone(),
        })
        .await
        .err()
        .ok_or("the session write should fail")?;
    assert!(error.to_string().contains("session write blocked"));
    let first_task = review_ids.iter().min().ok_or("missing first review task")?;
    assert!(
        core.sessions
            .get(first_task)
            .is_some_and(HarnessSession::is_closed)
    );
    let closed_record = core
        .session_records
        .get(first_task)
        .ok_or("missing session record")?
        .clone();
    assert_eq!(closed_record.exit_code, Some(0));
    assert!(closed_record.is_ended());
    toasty::sql::statement("DROP TRIGGER block_session_close")
        .exec(&mut database_probe)
        .await?;

    let stale_task = review_ids
        .iter()
        .max()
        .ok_or("missing second review task")?;
    toasty::sql::statement("UPDATE tasks SET state = 'ready' WHERE id = ?1")
        .bind(stale_task.as_str())
        .exec(&mut database_probe)
        .await?;
    assert!(matches!(
        core.execute(Command::ArchiveReviewed { task_ids: review_ids.clone() }).await,
        Err(CoreError::Store(StoreError::ReviewStateChanged { task_id })) if task_id == *stale_task
    ));
    for task in core.store.load_tasks().await? {
        if task.id == *first_task {
            assert_eq!(task.state, TaskState::Review);
        } else if task.id == *stale_task {
            assert_eq!(task.state, TaskState::Ready);
        }
    }
    let saved_record = core
        .store
        .load_sessions()
        .await?
        .into_iter()
        .find(|record| record.id == closed_record.id)
        .ok_or("missing durable session record")?;
    assert_eq!(saved_record.ended_at, closed_record.ended_at);
    assert_eq!(saved_record.exit_code, closed_record.exit_code);
    toasty::sql::statement("UPDATE tasks SET state = 'review' WHERE id = ?1")
        .bind(stale_task.as_str())
        .exec(&mut database_probe)
        .await?;

    let failing_id = stale_task.as_str().replace('\'', "''");
    let trigger = format!(
        "CREATE TRIGGER block_archive BEFORE UPDATE ON tasks WHEN NEW.state = 'done' AND NEW.id = '{failing_id}' BEGIN SELECT RAISE(ABORT, 'archive write blocked'); END"
    );
    toasty::sql::statement(&trigger)
        .exec(&mut database_probe)
        .await?;
    let error = core
        .execute(Command::ArchiveReviewed {
            task_ids: review_ids.clone(),
        })
        .await
        .err()
        .ok_or("the archive write should fail")?;
    assert!(error.to_string().contains("archive write blocked"));
    let saved = core.store.load_tasks().await?;
    for task_id in &review_ids {
        assert_eq!(core.task(task_id)?.state, TaskState::Review);
        assert_eq!(
            saved
                .iter()
                .find(|task| &task.id == task_id)
                .ok_or("missing task")?
                .state,
            TaskState::Review
        );
    }
    toasty::sql::statement("DROP TRIGGER block_archive")
        .exec(&mut database_probe)
        .await?;

    core.drain_events();
    assert!(matches!(
        core.execute(Command::ArchiveReviewed {
            task_ids: review_ids.clone()
        })
        .await?,
        CommandResult::Ack
    ));
    assert!(core.review_task_lists().is_empty());
    assert!(
        core.project_list()
            .projects
            .iter()
            .all(|project| project.review_task_count == 0)
    );
    assert_eq!(
        core.drain_events()
            .iter()
            .filter(|event| matches!(event, Event::TaskListChanged(_)))
            .count(),
        2
    );
    for task_id in &review_ids {
        let task = core.task(task_id)?;
        assert_eq!(task.state, TaskState::Done);
        assert!(task.worktree_path.is_none());
        assert!(
            core.sessions
                .get(task_id)
                .is_some_and(HarnessSession::is_closed)
        );
    }
    drop(core);
    let reopened = Core::load(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(UnavailableHarnessFactory),
    )
    .await?;
    for task_id in review_ids {
        assert_eq!(reopened.task(&task_id)?.state, TaskState::Done);
    }
    Ok(())
}

#[tokio::test]
async fn live_turn_errors_are_saved_while_unfinished_text_is_not()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let scripts = vec![vec![FakeTurn::new(vec![
        FakeStep::Message("unfinished answer".into()),
        FakeStep::TurnError {
            kind: shep_harness_core::FailureKind::Protocol,
            text: "fixture turn error".into(),
        },
        FakeStep::TerminalError {
            id: "terminal".into(),
            text: "Agent terminal error: missing exit status".into(),
        },
        FakeStep::approval("pending", "Await approval"),
    ])]];
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&database.uri()).await?),
        Box::new(FakeHarnessFactory::new(scripts)),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            project_id,
            title: "terminal error".into(),
            agent: Agent::Codex,
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id);
    core.execute(Command::SendPrompt {
        task_id: task_id.clone(),
        input: PromptInput::text("run"),
    })
    .await?;
    assert_eq!(core.task(&task_id)?.state, TaskState::Waiting);
    let store = TursoStore::open(&database.uri()).await?;
    let saved = store.load_transcript(&task_id).await?;
    assert!(saved.iter().any(|entry| matches!(&entry.kind, shep_harness_core::EntryKind::Notice { text, is_error: true } if text.contains("missing exit status"))));
    assert!(saved.iter().any(|entry| matches!(&entry.kind, shep_harness_core::EntryKind::Failure { text, .. } if text == "fixture turn error")));
    assert!(!saved.iter().any(|entry| matches!(&entry.kind, shep_harness_core::EntryKind::AgentText { text } if text.contains("unfinished answer"))));
    drop(store);
    drop(core);
    let mut reopened = Core::load(
        Box::new(TursoStore::open(&database.uri()).await?),
        Box::new(UnavailableHarnessFactory),
    )
    .await?;
    let workspace = reopened.open_task_workspace(&task_id).await?;
    assert!(workspace.transcript.iter().any(|entry| matches!(&entry.kind, shep_harness_core::EntryKind::Notice { text, is_error: true } if text.contains("missing exit status"))));
    assert!(workspace.transcript.iter().any(|entry| matches!(&entry.kind, shep_harness_core::EntryKind::Failure { text, .. } if text == "fixture turn error")));
    assert!(!workspace.transcript.iter().any(|entry| matches!(&entry.kind, shep_harness_core::EntryKind::AgentText { text } if text.contains("unfinished answer"))));
    Ok(())
}

#[tokio::test]
async fn terminal_error_notice_survives_task_reopen() -> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let scripts = vec![vec![FakeTurn::new(vec![FakeStep::TerminalError {
        id: "fixture-terminal".into(),
        text: "Agent terminal error: missing exit status".into(),
    }])]];
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&database.uri()).await?),
        Box::new(FakeHarnessFactory::new(scripts)),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            project_id,
            title: "terminal error".into(),
            agent: Agent::Codex,
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id);
    let workspace = workspace_of(
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("run"),
        })
        .await?,
    );
    assert!(workspace.transcript.iter().any(|entry| matches!(&entry.kind, shep_harness_core::EntryKind::Notice { text, is_error: true } if text.contains("missing exit status"))));
    drop(core);
    let mut reopened = Core::load(
        Box::new(TursoStore::open(&database.uri()).await?),
        Box::new(UnavailableHarnessFactory),
    )
    .await?;
    let workspace = reopened.open_task_workspace(&task_id).await?;
    assert!(workspace.transcript.iter().any(|entry| matches!(&entry.kind, shep_harness_core::EntryKind::Notice { text, is_error: true } if text.contains("missing exit status"))));
    Ok(())
}

#[tokio::test]
async fn large_transcript_and_git_diff_survive_restart() -> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let content: String = (0..12_000)
        .map(|index| format!("large diff line {index:05}\n"))
        .collect();
    let mut steps = vec![FakeStep::WriteFile {
        path: "large.txt".into(),
        contents: content,
    }];
    steps.extend((0..1_000).map(|index| FakeStep::Message(format!("message {index:04}"))));
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&database.uri()).await?),
        Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(steps)]])),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            project_id,
            title: "Large review".into(),
            agent: Agent::Codex,
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id);
    core.execute(Command::SendPrompt {
        task_id: task_id.clone(),
        input: PromptInput::text("Create a large review"),
    })
    .await?;
    while core.pump().await? {}
    let workspace = core.open_task_workspace(&task_id).await?;
    assert_eq!(workspace.task.state, TaskState::Review);
    let transcript = summaries(&workspace);
    for index in 0..1_000 {
        assert!(
            transcript
                .iter()
                .any(|entry| entry == &format!("agent: message {index:04}"))
        );
    }
    let diff = core.review_file_diff(&task_id, "large.txt")?;
    let expected_lines: Vec<String> = (0..12_000)
        .map(|index| format!("+large diff line {index:05}"))
        .collect();
    let actual_lines: Vec<&str> = diff
        .lines()
        .filter(|line| line.starts_with("+large diff line "))
        .collect();
    assert_eq!(
        actual_lines,
        expected_lines
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
    drop(core);

    let mut reopened = Core::load(
        Box::new(TursoStore::open(&database.uri()).await?),
        Box::new(UnavailableHarnessFactory),
    )
    .await?;
    let restored = reopened.open_task_workspace(&task_id).await?;
    assert_eq!(restored.task.state, TaskState::Review);
    assert_eq!(summaries(&restored), transcript);
    assert_eq!(reopened.review_file_diff(&task_id, "large.txt")?, diff);
    for index in [0, 32, 999] {
        let text = format!("message {index:04}");
        let entry = restored
            .transcript
            .iter()
            .find(|entry| {
                matches!(
                    &entry.kind,
                    shep_harness_core::EntryKind::AgentText { text: message } if message == &text
                )
            })
            .ok_or("missing indexed message")?;
        let hits = reopened.task_search().search(&format!("{text} ")).await?;
        assert!(
            hits.iter()
                .any(|hit| hit.task_id == task_id.as_str() && hit.entry_id == entry.id)
        );
    }
    Ok(())
}

#[tokio::test]
async fn missing_worktree_is_repaired_before_the_next_turn()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let scripts = vec![
        vec![FakeTurn::new(vec![FakeStep::Message("first reply".into())])],
        vec![FakeTurn::new(vec![FakeStep::Message(
            "second reply".into(),
        )])],
    ];
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&database.uri()).await?),
        Box::new(FakeHarnessFactory::new(scripts)),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            project_id,
            title: "repair".into(),
            agent: Agent::Codex,
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id);
    let first = workspace_of(
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("first prompt"),
        })
        .await?,
    );
    let path = PathBuf::from(first.task.worktree_path.ok_or("missing worktree")?);
    let _branch = first.task.branch.ok_or("missing branch")?;
    let tip = shep_core::git::run_git(&path, &["rev-parse", "HEAD"])?;
    std::fs::remove_dir_all(&path)?;
    let second = workspace_of(
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("second prompt"),
        })
        .await?,
    );
    assert_eq!(second.task.state, TaskState::Ready);
    assert_eq!(shep_core::git::run_git(&path, &["rev-parse", "HEAD"])?, tip);
    let transcript = summaries(&second).join("\n");
    assert!(transcript.contains("first reply"));
    assert!(transcript.contains("second reply"));
    drop(core);
    let mut reopened = Core::load(
        Box::new(TursoStore::open(&database.uri()).await?),
        Box::new(UnavailableHarnessFactory),
    )
    .await?;
    let transcript = summaries(&reopened.open_task_workspace(&task_id).await?).join("\n");
    assert!(transcript.contains("first reply"));
    assert!(transcript.contains("second reply"));
    Ok(())
}

#[tokio::test]
async fn archived_collection_keeps_ignored_files_and_clears_its_intent()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&database.uri()).await?),
        Box::new(UnavailableHarnessFactory),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let mut task = Task::draft(
        project_id,
        "Archived task",
        &repository.repository().git_dir,
    );
    GitService.create_worktree(
        repository.repository(),
        &task.proposed_worktree_path,
        &task.proposed_branch,
        "main",
    )?;
    std::fs::write(
        repository.repository().git_dir.join("info/exclude"),
        "ignored.txt\n",
    )?;
    let ignored = task.proposed_worktree_path.join("ignored.txt");
    std::fs::write(&ignored, "keep this data")?;
    task.worktree_path = Some(task.proposed_worktree_path.clone());
    task.branch = Some(task.proposed_branch.clone());
    task.state = TaskState::Done;
    core.store.save_task(&task).await?;
    core.tasks.insert(task.id.clone(), task.clone());

    assert!(matches!(
        core.collect_archived_worktree(&task.id).await,
        Err(CoreError::Git(GitError::IgnoredWorktreeFiles { .. }))
    ));
    assert_eq!(std::fs::read_to_string(ignored)?, "keep this data");
    assert!(!core.task(&task.id)?.collection_pending);
    assert!(
        !core
            .store
            .load_tasks()
            .await?
            .into_iter()
            .find(|saved| saved.id == task.id)
            .ok_or("missing task")?
            .collection_pending
    );
    Ok(())
}

#[tokio::test]
async fn touching_an_interrupted_collection_preserves_and_recovers_its_intent()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&database.uri()).await?),
        Box::new(UnavailableHarnessFactory),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let mut task = Task::draft(
        project_id,
        "Archived task",
        &repository.repository().git_dir,
    );
    GitService.create_worktree(
        repository.repository(),
        &task.proposed_worktree_path,
        &task.proposed_branch,
        "main",
    )?;
    let tip = GitService.checked_out_tip(&task.proposed_worktree_path, &task.proposed_branch)?;
    GitService.retain_task_tip(repository.repository(), task.id.as_str(), &tip)?;
    GitService.remove_clean_worktree(repository.repository(), &task.proposed_worktree_path)?;
    task.worktree_path = Some(task.proposed_worktree_path.clone());
    task.branch = Some(task.proposed_branch.clone());
    task.state = TaskState::Done;
    task.collection_pending = true;
    task.branch_tip_sha = Some("invalid-tip".into());
    core.store.save_task(&task).await?;
    core.tasks.insert(task.id.clone(), task.clone());

    assert!(core.touch_archived_worktree(&task.id).await.is_err());
    assert!(core.task(&task.id)?.collection_pending);
    assert!(
        core.review_task(&task.id, ReviewAction::Reopen)
            .await
            .is_err()
    );
    assert!(core.task(&task.id)?.collection_pending);
    assert_eq!(core.task(&task.id)?.state, TaskState::Done);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(repository.path(), &task.proposed_worktree_path)?;
        assert!(core.touch_archived_worktree(&task.id).await.is_err());
        assert!(core.collect_archived_worktree(&task.id).await.is_err());
        assert!(core.task(&task.id)?.collection_pending);
        std::fs::remove_file(&task.proposed_worktree_path)?;
    }
    GitService.create_worktree(
        repository.repository(),
        &task.proposed_worktree_path,
        "different-task-branch",
        "main",
    )?;
    assert!(core.touch_archived_worktree(&task.id).await.is_err());
    assert!(core.collect_archived_worktree(&task.id).await.is_err());
    assert!(core.task(&task.id)?.collection_pending);
    GitService.remove_clean_worktree(repository.repository(), &task.proposed_worktree_path)?;
    task.branch_tip_sha = Some(tip);
    core.store.save_task(&task).await?;
    core.tasks.insert(task.id.clone(), task.clone());
    core.touch_archived_worktree(&task.id).await?;
    let recovered = core.task(&task.id)?;
    assert!(!recovered.collection_pending && recovered.worktree_path.is_none());
    core.review_task(&task.id, ReviewAction::Reopen).await?;
    assert!(task.proposed_worktree_path.is_dir());
    Ok(())
}

#[tokio::test]
async fn startup_reconciles_recent_pending_collections_without_removing_live_worktrees()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let uri = database.uri();
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(UnavailableHarnessFactory),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let mut missing = Task::draft(
        project_id.clone(),
        "Missing",
        &repository.repository().git_dir,
    );
    let mut present = Task::draft(project_id, "Present", &repository.repository().git_dir);
    for task in [&mut missing, &mut present] {
        GitService.create_worktree(
            repository.repository(),
            &task.proposed_worktree_path,
            &task.proposed_branch,
            "main",
        )?;
        let tip =
            GitService.checked_out_tip(&task.proposed_worktree_path, &task.proposed_branch)?;
        GitService.retain_task_tip(repository.repository(), task.id.as_str(), &tip)?;
        task.branch_tip_sha = Some(tip);
        task.branch = Some(task.proposed_branch.clone());
        task.worktree_path = Some(task.proposed_worktree_path.clone());
        task.state = TaskState::Done;
        task.collection_pending = true;
    }
    GitService.remove_clean_worktree(repository.repository(), &missing.proposed_worktree_path)?;
    core.store.save_task(&missing).await?;
    core.store.save_task(&present).await?;
    drop(core);
    let reopened = Core::load(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(UnavailableHarnessFactory),
    )
    .await?;
    let recovered = reopened.task(&missing.id)?;
    assert!(!recovered.collection_pending && recovered.worktree_path.is_none());
    let kept = reopened.task(&present.id)?;
    assert!(!kept.collection_pending && kept.proposed_worktree_path.is_dir());
    Ok(())
}

#[tokio::test]
async fn startup_keeps_dirty_and_recent_archived_worktrees()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let uri = database.uri();
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(UnavailableHarnessFactory),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let mut dirty = Task::draft(
        project_id.clone(),
        "Dirty task",
        &repository.repository().git_dir,
    );
    let mut recent = Task::draft(project_id, "Recent task", &repository.repository().git_dir);
    for task in [&mut dirty, &mut recent] {
        GitService.create_worktree(
            repository.repository(),
            &task.proposed_worktree_path,
            &task.proposed_branch,
            "main",
        )?;
        task.worktree_path = Some(task.proposed_worktree_path.clone());
        task.branch = Some(task.proposed_branch.clone());
        task.state = TaskState::Done;
    }
    dirty.updated_at = projects::now_millis().saturating_sub(31 * 24 * 60 * 60 * 1000);
    dirty.worktree_last_used_at = dirty.updated_at;
    recent.updated_at = projects::now_millis().saturating_sub(30 * 24 * 60 * 60 * 1000 + 30_000);
    recent.worktree_last_used_at = recent.updated_at;
    std::fs::write(dirty.proposed_worktree_path.join("draft.txt"), "keep me")?;
    core.store.save_task(&dirty).await?;
    core.store.save_task(&recent).await?;
    drop(core);

    let reopened = Core::load(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(UnavailableHarnessFactory),
    )
    .await?;
    assert_eq!(
        std::fs::read_to_string(dirty.proposed_worktree_path.join("draft.txt"))?,
        "keep me"
    );
    assert!(recent.proposed_worktree_path.is_dir());
    let [issue] = reopened.archived_worktree_issues() else {
        return Err("expected one collection issue".into());
    };
    assert_eq!(issue.task_id, dirty.id.to_string());
    assert!(issue.reason.contains("untracked changes"));
    Ok(())
}

#[tokio::test]
async fn retry_task_branch_clears_a_branch_creation_failure()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    for suffix in 0..100 {
        let branch = if suffix == 0 {
            "shepherd/first-task".to_string()
        } else {
            format!("shepherd/first-task-{suffix}")
        };
        shep_core::git::run_git(repository.path(), &["branch", &branch])?;
    }
    let mut core = Core::with_services(
        Box::new(MemoryStore::new()),
        Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
            FakeStep::Message("ready".into()),
        ])]])),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let workspace = workspace_of(
        core.execute(Command::StartTask {
            project_id,
            agent: Agent::Codex,
            input: PromptInput::text("first task"),
        })
        .await?,
    );
    assert!(workspace.task.branch.is_none());
    assert!(workspace.task.branch_error.is_some());

    let task_id = TaskId::new(&workspace.task.id);
    shep_core::git::run_git(repository.path(), &["branch", "-D", "shepherd/first-task"])?;
    let workspace = workspace_of(
        core.execute(Command::RetryTaskBranch {
            task_id: task_id.clone(),
        })
        .await?,
    );
    assert_eq!(
        workspace.task.branch.as_deref(),
        Some("shepherd/first-task")
    );
    assert!(workspace.task.branch_error.is_none());
    assert_eq!(
        core.task(&task_id)?.branch.as_deref(),
        Some("shepherd/first-task")
    );
    Ok(())
}

#[tokio::test]
async fn start_task_persists_the_title_and_turn_options_across_reload()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let uri = database.uri();
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
            FakeStep::Message("ready".into()),
        ])]])),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let mut input = PromptInput::text("\n  Keep this title  \nsecond line");
    input.model = Some("acme/qwen".into());
    let workspace = workspace_of(
        core.execute(Command::StartTask {
            project_id: project_id.clone(),
            agent: Agent::Codex,
            input,
        })
        .await?,
    );
    assert_eq!(workspace.task.title, "Keep this title");
    assert_eq!(workspace.task.model.as_deref(), Some("acme/qwen"));
    let task_id = TaskId::new(&workspace.task.id);
    let workspace = workspace_of(
        core.execute(Command::SetTaskOptions {
            task_id: task_id.clone(),
            model: Some("acme/qwen-next".into()),
            mode: Some("plan".into()),
        })
        .await?,
    );
    assert_eq!(workspace.task.model.as_deref(), Some("acme/qwen-next"));
    assert_eq!(workspace.task.mode.as_deref(), Some("plan"));
    let tasks = core.store.load_tasks().await?;
    assert_eq!(tasks.len(), 1);
    assert!(!tasks.iter().any(|task| task.state == TaskState::Draft));
    drop(core);

    let core = Core::load(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(UnavailableHarnessFactory),
    )
    .await?;
    let restored = core.task(&task_id)?;
    assert_eq!(restored.title, "Keep this title");
    assert_eq!(restored.model.as_deref(), Some("acme/qwen-next"));
    assert_eq!(restored.mode.as_deref(), Some("plan"));
    Ok(())
}

#[tokio::test]
async fn a_rejected_first_prompt_removes_the_task_and_worktree()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let worktree_root = repository.repository().git_dir.join("shepherd/worktrees");
    let mut core = Core::with_services(
        Box::new(MemoryStore::new()),
        Box::new(FakeHarnessFactory::failing_prompt(
            shep_harness_core::fake_harness::FakeFailure::AuthRequired,
        )),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let result = core
        .execute(Command::StartTask {
            project_id,
            agent: Agent::Codex,
            input: PromptInput::text("Sign in first"),
        })
        .await;
    assert!(matches!(result, Err(CoreError::Harness(_))));
    assert!(core.tasks.is_empty());
    assert!(core.store.load_tasks().await?.is_empty());
    assert!(core.store.load_worktree_pool().await?.is_empty());
    let projects = core.project_list();
    assert!(projects.projects[0].last_task_id.is_none());
    let remaining = std::fs::read_dir(&worktree_root)
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or(0);
    assert_eq!(remaining, 0, "the rejected first prompt left a worktree");
    Ok(())
}

struct DirtyRejectingHarness {
    inner: FakeHarness,
    cwd: Option<PathBuf>,
}

#[async_trait]
impl AgentHarness for DirtyRejectingHarness {
    fn events(&mut self) -> &mut tokio::sync::mpsc::UnboundedReceiver<HarnessEvent> {
        self.inner.events()
    }

    async fn start(&mut self, spec: SpawnSpec) -> HarnessResult<Capabilities> {
        self.cwd = Some(spec.cwd.clone());
        self.inner.start(spec).await
    }

    async fn open_session(&mut self, resume: Option<SessionId>) -> HarnessResult<SessionId> {
        self.inner.open_session(resume).await
    }

    async fn prompt(&mut self, _input: PromptInput) -> HarnessResult<()> {
        let cwd = self.cwd.as_ref().ok_or(HarnessError::NotStarted)?;
        std::fs::write(cwd.join("rejected-first-prompt.txt"), "dirty").map_err(|error| {
            HarnessError::Protocol(format!("could not dirty the worktree: {error}"))
        })?;
        Err(HarnessError::Protocol(
            "401 Unauthorized: run `codex login`".to_string(),
        ))
    }

    async fn cancel(&mut self) -> HarnessResult<()> {
        self.inner.cancel().await
    }

    async fn respond(&mut self, approval_id: ApprovalId, decision: Decision) -> HarnessResult<()> {
        self.inner.respond(approval_id, decision).await
    }

    async fn shutdown(&mut self) -> HarnessResult<()> {
        self.inner.shutdown().await
    }
}

struct DirtyRejectingFactory;

impl HarnessFactory for DirtyRejectingFactory {
    fn create(&self) -> Result<Box<dyn AgentHarness>, CoreError> {
        Ok(Box::new(DirtyRejectingHarness {
            inner: FakeHarness::new(Vec::new()),
            cwd: None,
        }))
    }
}

#[tokio::test]
async fn a_rejected_first_prompt_removes_records_when_worktree_removal_fails()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let worktree_root = repository.repository().git_dir.join("shepherd/worktrees");
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&database.uri()).await?),
        Box::new(DirtyRejectingFactory),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let mut input = PromptInput::text("Reject this first prompt");
    input.model = Some("acme/qwen".into());
    let error = match core
        .execute(Command::StartTask {
            project_id: project_id.clone(),
            agent: Agent::Codex,
            input,
        })
        .await
    {
        Err(error) => error,
        Ok(_) => return Err("the first prompt should have been rejected".into()),
    };
    assert!(
        matches!(&error, CoreError::StartTaskCleanup { .. }),
        "unexpected StartTask error: {error:?}"
    );
    let message = error.to_string();
    assert!(message.contains("401 Unauthorized"), "{message}");
    assert!(message.contains("cleanup was incomplete"), "{message}");
    assert!(message.contains("worktree"), "{message}");

    assert!(core.tasks.is_empty());
    assert!(core.store.load_tasks().await?.is_empty());
    assert!(core.store.load_worktree_pool().await?.is_empty());
    assert!(core.project(&project_id)?.last_task_id.is_none());
    assert!(
        core.store
            .load_projects()
            .await?
            .first()
            .is_some_and(|project| project.last_task_id.is_none())
    );
    let remaining: Vec<_> = std::fs::read_dir(&worktree_root)?
        .filter_map(Result::ok)
        .collect();
    assert_eq!(remaining.len(), 1, "the dirty worktree should remain");
    assert!(
        GitService.has_changes(&remaining[0].path())?,
        "the test must leave a dirty worktree so removal fails"
    );
    Ok(())
}

#[tokio::test]
async fn worktree_pool_persists_leases_reuses_idle_slots_and_retires_dirty_checkouts()
-> Result<(), Box<dyn std::error::Error>> {
    fn turn(message: &str) -> Vec<FakeTurn> {
        vec![FakeTurn::new(vec![FakeStep::Message(message.to_string())])]
    }

    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let uri = database.uri();
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(FakeHarnessFactory::new(vec![
            turn("first answer"),
            turn("second answer"),
        ])),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let first = workspace_of(
        core.execute(Command::StartTask {
            project_id: project_id.clone(),
            agent: Agent::Codex,
            input: PromptInput::text("first task"),
        })
        .await?,
    );
    let first_path = PathBuf::from(first.task.worktree_path.ok_or("missing first worktree")?);
    let first_id = TaskId::new(&first.task.id);
    let mut first_record = core.task(&first_id)?.clone();
    first_record.state = TaskState::Review;
    core.store.save_task(&first_record).await?;
    core.tasks.insert(first_id.clone(), first_record);
    let records = core.store.load_worktree_pool().await?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].state, WorktreePoolState::Leased);
    assert_eq!(
        records[0].task_id.as_ref().map(TaskId::as_str),
        Some(first.task.id.as_str())
    );
    assert_eq!(records[0].path, first_path);

    let second = workspace_of(
        core.execute(Command::StartTask {
            project_id,
            agent: Agent::Codex,
            input: PromptInput::text("second task"),
        })
        .await?,
    );
    let second_path = PathBuf::from(second.task.worktree_path.ok_or("missing second worktree")?);
    assert_ne!(first_path, second_path);
    let mut records = core.store.load_worktree_pool().await?;
    records.sort_by(|left, right| left.path.cmp(&right.path));
    assert_eq!(records.len(), 2);
    assert!(
        records
            .iter()
            .all(|record| record.state == WorktreePoolState::Leased)
    );

    core.execute(Command::ArchiveReviewed {
        task_ids: vec![first_id],
    })
    .await?;
    drop(core);

    let mut core = Core::load(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(FakeHarnessFactory::new(vec![
            turn("third answer"),
            turn("fourth answer"),
        ])),
    )
    .await?;
    let mut records = core.store.load_worktree_pool().await?;
    records.sort_by(|left, right| left.path.cmp(&right.path));
    assert_eq!(
        records
            .iter()
            .map(|record| record.state)
            .collect::<Vec<_>>(),
        [WorktreePoolState::Idle, WorktreePoolState::Leased]
    );
    assert_eq!(records[0].path, first_path);

    let project_id = ProjectId::new(&first.task.project_id);
    let third = workspace_of(
        core.execute(Command::StartTask {
            project_id: project_id.clone(),
            agent: Agent::Codex,
            input: PromptInput::text("third task"),
        })
        .await?,
    );
    assert_eq!(
        PathBuf::from(third.task.worktree_path.ok_or("missing third worktree")?),
        first_path
    );
    assert_eq!(core.store.load_worktree_pool().await?.len(), 2);

    std::fs::write(second_path.join("dirty.txt"), "keep me")?;
    let second_id = TaskId::new(&second.task.id);
    let mut second_record = core.task(&second_id)?.clone();
    second_record.state = TaskState::Review;
    core.store.save_task(&second_record).await?;
    core.tasks.insert(second_id.clone(), second_record);
    core.execute(Command::ArchiveReviewed {
        task_ids: vec![second_id],
    })
    .await?;
    let retired = core
        .store
        .load_worktree_pool()
        .await?
        .into_iter()
        .find(|record| record.path == second_path)
        .ok_or("missing retired record")?;
    assert_eq!(retired.state, WorktreePoolState::Retired);
    assert!(retired.task_id.is_none());
    assert!(retired.leased_at.is_none());
    assert!(second_path.join("dirty.txt").is_file());

    let fourth = workspace_of(
        core.execute(Command::StartTask {
            project_id,
            agent: Agent::Codex,
            input: PromptInput::text("fourth task"),
        })
        .await?,
    );
    let fourth_path = PathBuf::from(fourth.task.worktree_path.ok_or("missing fourth worktree")?);
    assert_ne!(fourth_path, first_path);
    assert_ne!(fourth_path, second_path);
    let records = core.store.load_worktree_pool().await?;
    assert_eq!(records.len(), 3);
    assert_eq!(
        records
            .iter()
            .filter(|record| record.state == WorktreePoolState::Retired)
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn deleting_a_task_reclaims_only_its_worktree_pool_record()
-> Result<(), Box<dyn std::error::Error>> {
    fn turn(message: &str) -> Vec<FakeTurn> {
        vec![FakeTurn::new(vec![FakeStep::Message(message.to_string())])]
    }

    let first_repository = TestRepository::new()?;
    let second_repository = TestRepository::new()?;
    let mut core = Core::with_services(
        Box::new(MemoryStore::new()),
        Box::new(FakeHarnessFactory::new(vec![
            turn("first answer"),
            turn("second answer"),
        ])),
    );
    let first_project = add_project(&mut core, first_repository.path().to_path_buf()).await?;
    let second_project = add_project(&mut core, second_repository.path().to_path_buf()).await?;
    let first = workspace_of(
        core.execute(Command::StartTask {
            project_id: first_project,
            agent: Agent::Codex,
            input: PromptInput::text("first task"),
        })
        .await?,
    );
    let second = workspace_of(
        core.execute(Command::StartTask {
            project_id: second_project,
            agent: Agent::Codex,
            input: PromptInput::text("second task"),
        })
        .await?,
    );
    let first_path = PathBuf::from(first.task.worktree_path.ok_or("missing first worktree")?);
    let second_path = PathBuf::from(second.task.worktree_path.ok_or("missing second worktree")?);
    assert_eq!(core.store.load_worktree_pool().await?.len(), 2);

    let first_id = TaskId::new(&first.task.id);
    let mut first_record = core.task(&first_id)?.clone();
    first_record.state = TaskState::Review;
    core.store.save_task(&first_record).await?;
    core.tasks.insert(first_id.clone(), first_record);
    core.execute(Command::DeleteTask {
        task_id: first_id,
        remove_worktree: true,
    })
    .await?;

    let records = core.store.load_worktree_pool().await?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].path, second_path);
    assert_eq!(records[0].project_id.as_str(), second.task.project_id);
    assert!(records.iter().all(|record| record.path != first_path));
    assert_eq!(records[0].state, WorktreePoolState::Leased);
    Ok(())
}

#[tokio::test]
async fn review_commit_merge_archive_and_reopen_a_task() -> Result<(), Box<dyn std::error::Error>> {
    use shep_core::git::GitMutation;

    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let uri = database.uri();
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(FakeHarnessFactory::new(vec![vec![
            FakeTurn::new(vec![
                FakeStep::WriteFile {
                    path: "README.md".into(),
                    contents: "first pass\n".into(),
                },
                FakeStep::WriteFile {
                    path: "notes.txt".into(),
                    contents: "review context\n".into(),
                },
            ]),
            FakeTurn::new(vec![FakeStep::WriteFile {
                path: "README.md".into(),
                contents: "reviewed result\n".into(),
            }]),
        ]])),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            agent: Agent::Codex,
            project_id,
            title: "Improve the readme".into(),
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id);
    let review = workspace_of(
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("Edit the readme and leave notes"),
        })
        .await?,
    );
    assert_eq!(review.task.state, TaskState::Review);
    let path = PathBuf::from(review.task.worktree_path.ok_or("missing worktree")?);
    let branch = review.task.branch.ok_or("missing task branch")?;
    let files = core.review_snapshot(&task_id)?.files;
    assert!(files.iter().any(|file| file.path == "README.md"));
    assert!(files.iter().any(|file| file.path == "notes.txt"));
    let diff = core.review_file_diff(&task_id, "README.md")?;
    assert!(diff.contains("+first pass"));
    assert!(!diff.contains("review context"));

    let reviewed = workspace_of(
        core.execute(Command::ReviewTask {
            task_id: task_id.clone(),
            action: ReviewAction::SendNote {
                text: "Revise the first line".into(),
            },
        })
        .await?,
    );
    assert_eq!(reviewed.task.state, TaskState::Review);
    assert!(
        summaries(&reviewed).iter().any(|entry| {
            entry.contains("Revise the first line") && entry.contains("notes.txt")
        })
    );

    let staged = core
        .mutate_git(&task_id, GitMutation::Stage { path: None })
        .await?;
    let committed = core
        .mutate_git(
            &task_id,
            GitMutation::Commit {
                message: "Apply the review feedback".into(),
                expected: staged.staged_revision,
            },
        )
        .await?;
    assert!(committed.files.is_empty());
    let _tip = shep_core::git::run_git(&path, &["rev-parse", "HEAD"])?;
    core.mutate_git(&task_id, GitMutation::SquashMerge).await?;
    assert_eq!(
        std::fs::read_to_string(repository.path().join("README.md"))?,
        "reviewed result\n"
    );

    let archived = workspace_of(
        core.execute(Command::ReviewTask {
            task_id: task_id.clone(),
            action: ReviewAction::Archive,
        })
        .await?,
    );
    assert_eq!(archived.task.state, TaskState::Done);
    let mut archived_record = core.task(&task_id)?.clone();
    archived_record.updated_at = projects::now_millis().saturating_sub(31 * 24 * 60 * 60 * 1000);
    archived_record.worktree_last_used_at = archived_record.updated_at;
    core.store.save_task(&archived_record).await?;
    let expected_tip = core
        .task(&task_id)?
        .branch_tip_sha
        .clone()
        .ok_or("missing branch tip")?;
    drop(core);

    let mut reopened = Core::load(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(UnavailableHarnessFactory),
    )
    .await?;
    assert!(
        reopened
            .task_workspace(&task_id)?
            .task
            .worktree_path
            .is_none()
    );
    assert_eq!(
        git_in(&repository, &["rev-parse", &format!("refs/heads/{branch}")]),
        expected_tip
    );
    shep_core::git::run_git(repository.path(), &["branch", "-D", &branch])?;
    let restored = workspace_of(
        reopened
            .execute(Command::ReviewTask {
                task_id: task_id.clone(),
                action: ReviewAction::Reopen,
            })
            .await?,
    );
    assert_eq!(restored.task.state, TaskState::Review);
    assert_eq!(restored.task.branch.as_deref(), Some(branch.as_str()));
    let restored_path = restored
        .task
        .worktree_path
        .as_deref()
        .ok_or("missing restored worktree")?;
    assert!(Path::new(restored_path).exists());
    assert_eq!(
        shep_core::git::run_git(Path::new(restored_path), &["branch", "--show-current"])?,
        branch
    );
    Ok(())
}

#[tokio::test]
async fn the_walking_skeleton_runs_from_project_to_restart()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let uri = database.uri();

    let first_run = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
            FakeStep::Message("streaming text".to_string()),
            FakeStep::approval("approval-1", "Run tests"),
        ])]])),
    );
    let mut scenario = TestCore::with_core(first_run);

    let project_id = add_project(scenario.core_mut(), repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        scenario
            .execute(Command::CreateDraft {
                agent: Agent::Codex,
                project_id: project_id.clone(),
                title: "Ship the skeleton".to_string(),
            })
            .await?,
    );
    assert_eq!(draft.task.state, TaskState::Draft);
    assert_eq!(draft.task.proposed_branch, "shepherd/ship-the-skeleton");
    let task_id = TaskId::new(draft.task.id.clone());

    let workspace = workspace_of(
        scenario
            .execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("start"),
            })
            .await?,
    );
    assert_eq!(workspace.task.state, TaskState::Waiting);
    assert_eq!(
        summaries(&workspace),
        vec![
            "you: start".to_string(),
            "agent: streaming text".to_string(),
            "approval: Run tests (pending)".to_string(),
        ]
    );
    let worktree_path = PathBuf::from(
        workspace
            .task
            .worktree_path
            .clone()
            .expect("the first prompt creates the worktree"),
    );
    assert!(worktree_path.is_dir());
    assert!(
        !git_in(
            &repository,
            &["branch", "--list", "shepherd/ship-the-skeleton"]
        )
        .is_empty()
    );

    let trace = scenario.trace();
    for expected in [
        "event: ProjectListChanged",
        "event: TaskListChanged",
        "event: TaskChanged",
        "event: TaskWorkspaceChanged",
    ] {
        assert!(
            trace.contains(expected),
            "{expected} missing from the stream:\n{trace}"
        );
    }

    let refused = scenario.execute(Command::Quit { force: false }).await;
    assert!(matches!(
        refused,
        Err(CoreError::QuitConfirmationRequired {
            live_tasks: 1,
            live_terminals: 0
        })
    ));
    let result = scenario.execute(Command::Quit { force: true }).await?;
    let CommandResult::Quit(summary) = result else {
        panic!("expected a quit result");
    };
    assert_eq!(summary.interrupted_tasks, 1);
    assert_eq!(summary.live_terminals, 0);
    assert_eq!(scenario.core().shutdown_state(), ShutdownState::Closed);
    drop(scenario);

    let reopened = Core::load(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
            FakeStep::Message("after restart".to_string()),
        ])]])),
    )
    .await?;
    let mut scenario = TestCore::with_core(reopened);

    let projects = scenario.core().project_list();
    assert_eq!(projects.projects.len(), 1);
    assert_eq!(projects.projects[0].unfinished_task_count, 1);

    let workspace = scenario.core().task_workspace(&task_id)?;
    assert_eq!(workspace.task.state, TaskState::Interrupted);
    assert_eq!(
        workspace.task.branch.as_deref(),
        Some("shepherd/ship-the-skeleton")
    );
    // Quitting mid-turn keeps the agent output the transcript showed.
    let restored = scenario.core_mut().open_task_workspace(&task_id).await?;
    assert_eq!(
        summaries(&restored),
        vec![
            "you: start".to_string(),
            "agent: streaming text".to_string(),
            "approval: Run tests (pending)".to_string(),
        ]
    );

    let workspace = workspace_of(
        scenario
            .execute(Command::SendPrompt {
                task_id,
                input: PromptInput::text("continue"),
            })
            .await?,
    );
    assert_eq!(
        workspace.transcript.first().map(Entry::summary).as_deref(),
        Some("you: start")
    );
    assert!(workspace.transcript.iter().any(|entry| {
        entry
            .summary()
            .contains("resumed the previous agent session")
    }));
    Ok(())
}

#[tokio::test]
async fn a_completed_tool_call_persists_the_live_turn() -> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let uri = database.uri();
    let core = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
            FakeStep::Message("reading the file".to_string()),
            FakeStep::ToolCall {
                tool_call_id: "call-1".to_string(),
                title: "read".to_string(),
                kind: "read".to_string(),
                status: shep_harness_core::ToolStatus::Completed,
                output: Some("contents".to_string()),
                diffs: Vec::new(),
            },
            FakeStep::approval("approval-1", "Read src/main.rs"),
        ])]])),
    );
    let mut scenario = TestCore::with_core(core);
    let project_id = add_project(scenario.core_mut(), repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        scenario
            .execute(Command::CreateDraft {
                agent: Agent::Codex,
                project_id,
                title: "Read the manifest".to_string(),
            })
            .await?,
    );
    let task_id = TaskId::new(draft.task.id.clone());
    scenario
        .execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("read it"),
        })
        .await?;

    let observer = TursoStore::open(&uri).await?;
    let persisted = observer.load_transcript(&task_id).await?;
    let summaries: Vec<String> = persisted.iter().map(Entry::summary).collect();
    assert_eq!(
        summaries,
        vec![
            "you: read it".to_string(),
            "agent: reading the file".to_string(),
            "read (Completed)".to_string(),
            "approval: Read src/main.rs (pending)".to_string(),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn workspace_width_is_saved_per_task_and_restored() -> Result<(), Box<dyn std::error::Error>>
{
    let directory = TestDirectory::new()?;
    let database = directory.join("shepherd.db");
    let store = TursoStore::open_at(&database).await?;
    let mut core = Core::with_services(Box::new(store), Box::new(UnavailableHarnessFactory));
    let project_id = add_project(&mut core, directory.path().to_path_buf()).await?;
    let first = workspace_of(
        core.execute(Command::CreateDraft {
            agent: Agent::Codex,
            project_id: project_id.clone(),
            title: "Wide inspector".into(),
        })
        .await?,
    );
    let second = workspace_of(
        core.execute(Command::CreateDraft {
            agent: Agent::Codex,
            project_id,
            title: "Default inspector".into(),
        })
        .await?,
    );
    let first = TaskId::new(first.task.id);
    let second = TaskId::new(second.task.id);
    let layout = WorkspaceLayout {
        inspector_width: Some(760),
    };
    core.save_workspace_layout(&first, layout).await?;
    assert!(matches!(
        core.save_workspace_layout(
            &first,
            WorkspaceLayout {
                inspector_width: Some(0)
            }
        )
        .await,
        Err(CoreError::InvalidWorkspaceLayout)
    ));
    drop(core);

    let store = TursoStore::open_at(&database).await?;
    let core = Core::load(Box::new(store), Box::new(UnavailableHarnessFactory)).await?;
    assert_eq!(core.task_workspace(&first)?.layout, layout);
    assert_eq!(
        core.task_workspace(&second)?.layout,
        WorkspaceLayout::default()
    );
    Ok(())
}

#[tokio::test]
async fn terminal_layout_round_trips_and_rejects_stale_or_foreign_panes()
-> Result<(), Box<dyn std::error::Error>> {
    use shep_core::terminal::{SplitAxis, TerminalSplit};

    let directory = TestDirectory::new()?;
    let other = TestDirectory::new()?;
    let database = directory.join("shepherd.db");
    let store = TursoStore::open_at(&database).await?;
    let mut core = Core::with_services(Box::new(store), Box::new(UnavailableHarnessFactory));
    let project = add_project(&mut core, directory.path().to_path_buf()).await?;
    core.execute(Command::AddProject {
        path: other.path().to_path_buf(),
    })
    .await?;
    let foreign = core
        .project_order
        .last()
        .cloned()
        .ok_or("second project was not added")?;
    for (id, owner, path) in [
        ("first", &project, directory.path()),
        ("second", &project, directory.path()),
        ("foreign", &foreign, other.path()),
    ] {
        let terminal = TerminalRecord::project_shared(
            TerminalId::new(id),
            owner.clone(),
            id,
            path.to_path_buf(),
            "/bin/sh",
            1,
        );
        core.record_terminal(&terminal).await?;
    }
    let pane = |id: &str| {
        Box::new(TerminalSplit::Pane {
            terminal_id: id.into(),
        })
    };
    let legacy_layout: TerminalLayout = serde_json::from_str(r#"{"revision":0,"root":null}"#)?;
    assert_eq!(legacy_layout, TerminalLayout::default());
    let edit = TerminalLayout {
        root: Some(TerminalSplit::Split {
            axis: SplitAxis::Horizontal,
            ratio: 650,
            first: pane("first"),
            second: pane("second"),
        }),
        ..legacy_layout
    };
    core.save_inspector_tab(&project, InspectorTab::Review)
        .await?;
    let saved = core.save_terminal_layout(&project, edit.clone()).await?;
    assert_eq!(saved.revision, 1);
    assert_eq!(saved.inspector_tab, InspectorTab::Review);
    assert!(matches!(
        core.save_terminal_layout(&project, edit).await,
        Err(CoreError::TerminalLayoutChanged)
    ));
    assert!(matches!(
        core.save_terminal_layout(
            &project,
            TerminalLayout {
                revision: saved.revision,
                root: Some(*pane("foreign")),
                ..TerminalLayout::default()
            }
        )
        .await,
        Err(CoreError::InvalidTerminalLayout)
    ));
    drop(core);

    let store = TursoStore::open_at(&database).await?;
    let reopened = Core::load(Box::new(store), Box::new(UnavailableHarnessFactory)).await?;
    assert_eq!(reopened.terminal_layout(&project)?, saved);
    assert_eq!(
        reopened.terminal_layout(&foreign)?,
        TerminalLayout::default()
    );
    Ok(())
}

#[tokio::test]
async fn restoring_a_missing_terminal_keeps_the_other_panes()
-> Result<(), Box<dyn std::error::Error>> {
    use shep_core::terminal::{SplitAxis, TerminalSplit};

    let directory = TestDirectory::new()?;
    let database = directory.join("shepherd.db");
    let store = TursoStore::open_at(&database).await?;
    let mut core = Core::with_services(Box::new(store), Box::new(UnavailableHarnessFactory));
    let project = add_project(&mut core, directory.path().to_path_buf()).await?;
    core.record_terminal(&TerminalRecord::project_shared(
        TerminalId::new("kept"),
        project.clone(),
        "Kept",
        directory.path().to_path_buf(),
        "/bin/sh",
        1,
    ))
    .await?;
    let pane = |id: &str| TerminalSplit::Pane {
        terminal_id: id.into(),
    };
    let split = |ratio| TerminalSplit::Split {
        axis: SplitAxis::Horizontal,
        ratio,
        first: Box::new(pane("kept")),
        second: Box::new(pane("missing")),
    };
    let saved = TerminalLayout {
        root: Some(split(650)),
        inspector_tab: InspectorTab::Review,
        ..TerminalLayout::default()
    };
    assert!(matches!(
        core.save_terminal_layout(&project, saved.clone()).await,
        Err(CoreError::InvalidTerminalLayout)
    ));
    core.store.save_terminal_layout(&project, &saved).await?;
    drop(core);

    let store = TursoStore::open_at(&database).await?;
    let mut core = Core::load(Box::new(store), Box::new(UnavailableHarnessFactory)).await?;
    assert_eq!(core.terminal_layout(&project)?, saved);
    let resized = core
        .save_terminal_layout(
            &project,
            TerminalLayout {
                root: Some(split(700)),
                ..saved
            },
        )
        .await?;
    let cleaned = core
        .save_terminal_layout(
            &project,
            TerminalLayout {
                root: Some(pane("kept")),
                ..resized
            },
        )
        .await?;
    assert_eq!(cleaned.root, Some(pane("kept")));
    assert_eq!(cleaned.inspector_tab, InspectorTab::Review);
    assert!(matches!(
        core.save_terminal_layout(
            &project,
            TerminalLayout {
                root: Some(split(700)),
                ..cleaned
            },
        )
        .await,
        Err(CoreError::InvalidTerminalLayout)
    ));
    Ok(())
}

#[tokio::test]
async fn a_terminal_record_is_ended_after_a_restart() -> Result<(), Box<dyn std::error::Error>> {
    let directory = TestDirectory::new()?;
    let database = directory.join("shepherd.db");
    let store = shep_store::TursoStore::open_at(&database).await?;
    let mut core = Core::with_services(Box::new(store), Box::new(UnavailableHarnessFactory));
    let project_id = add_project(&mut core, directory.path().to_path_buf()).await?;

    let terminal = TerminalRecord::project_shared(
        TerminalId::new("terminal-1"),
        project_id.clone(),
        "Terminal 1".to_string(),
        directory.path().to_path_buf(),
        "/bin/zsh".to_string(),
        1_000,
    );
    core.record_terminal(&terminal).await?;
    drop(core);

    let store = shep_store::TursoStore::open_at(&database).await?;
    let reloaded = Core::load(Box::new(store), Box::new(UnavailableHarnessFactory)).await?;
    let terminals = reloaded.terminals_for(&project_id);
    assert_eq!(terminals.len(), 1);
    assert!(terminals[0].ended_at.is_some());
    assert_eq!(terminals[0].exit_code, None);
    Ok(())
}

#[tokio::test]
async fn the_agent_session_survives_a_restart() -> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let uri = database.uri();

    let first_run = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        // A turn waiting on approval stays live, so quitting interrupts it
        // and the restart has something to resume.
        Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
            FakeStep::approval("approval-1", "Run tests"),
        ])]])),
    );
    let mut scenario = TestCore::with_core(first_run);
    let project_id = add_project(scenario.core_mut(), repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        scenario
            .execute(Command::CreateDraft {
                agent: Agent::Codex,
                project_id,
                title: "Resume me".to_string(),
            })
            .await?,
    );
    let task_id = TaskId::new(draft.task.id.clone());
    scenario
        .execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("go"),
        })
        .await?;
    scenario.execute(Command::Quit { force: true }).await?;
    drop(scenario);

    let reopened = Core::load(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
            FakeStep::Message("second answer".to_string()),
        ])]])),
    )
    .await?;
    let mut scenario = TestCore::with_core(reopened);

    let workspace = scenario.core().task_workspace(&task_id)?;
    assert_eq!(workspace.task.state, TaskState::Interrupted);

    let workspace = workspace_of(
        scenario
            .execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("continue"),
            })
            .await?,
    );
    assert!(workspace.transcript.iter().any(|entry| {
        entry
            .summary()
            .contains("resumed the previous agent session fake-session-1")
    }));

    let sessions = TursoStore::open(&uri).await?.load_sessions().await?;
    assert_eq!(sessions.len(), 2, "each session start writes its own row");
    assert!(sessions.iter().all(|session| {
        session.task_id == task_id
            && session.adapter_session_id.as_deref() == Some("fake-session-1")
    }));
    assert!(sessions[0].is_ended(), "quit should end the first record");
    assert!(!sessions[1].is_ended(), "the resumed session is still live");
    Ok(())
}

#[tokio::test]
async fn a_gone_agent_session_falls_back_with_a_clear_note()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let uri = database.uri();

    let first_run = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        // A turn waiting on approval stays live, so quitting interrupts it
        // and the restart has something to resume.
        Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
            FakeStep::approval("approval-1", "Run tests"),
        ])]])),
    );
    let mut scenario = TestCore::with_core(first_run);
    let project_id = add_project(scenario.core_mut(), repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        scenario
            .execute(Command::CreateDraft {
                agent: Agent::Codex,
                project_id,
                title: "Resume me".to_string(),
            })
            .await?,
    );
    let task_id = TaskId::new(draft.task.id.clone());
    scenario
        .execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("go"),
        })
        .await?;
    scenario.execute(Command::Quit { force: true }).await?;
    drop(scenario);

    let reopened = Core::load(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(FakeHarnessFactory::refusing_resume(vec![vec![
            FakeTurn::new(vec![FakeStep::Message("second answer".to_string())]),
        ]])),
    )
    .await?;
    let mut scenario = TestCore::with_core(reopened);

    let workspace = workspace_of(
        scenario
            .execute(Command::SendPrompt {
                task_id,
                input: PromptInput::text("continue"),
            })
            .await?,
    );
    let note = workspace
        .transcript
        .iter()
        .map(Entry::summary)
        .find(|summary| summary.contains("cannot resume"))
        .ok_or("missing resume note")?;
    assert!(
        note.contains("Started a new session"),
        "unexpected note: {note}"
    );
    Ok(())
}

#[tokio::test]
async fn an_unconfigured_adapter_reports_why_it_could_not_launch()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let mut core = Core::new();
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            agent: Agent::Claude,
            project_id,
            title: "Unconfigured adapter".into(),
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id);
    let result = core
        .execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("go"),
        })
        .await;
    assert!(matches!(result, Err(CoreError::HarnessMissing { .. })));
    let workspace = core.task_workspace(&task_id)?;
    assert_eq!(workspace.task.state, TaskState::Ready);
    assert!(
        workspace
            .task
            .error
            .is_some_and(|text| text.contains("Claude Code ACP executable"))
    );
    assert!(
        workspace
            .transcript
            .iter()
            .any(|entry| entry.summary().contains("agent not installed"))
    );
    Ok(())
}

#[tokio::test]
async fn a_missing_binary_leaves_a_failure_row_and_a_ready_task()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let mut scenario = TestCore::with_core(Core::with_services(
        Box::new(MemoryStore::new()),
        Box::new(FakeHarnessFactory::failing_start(
            shep_harness_core::fake_harness::FakeFailure::MissingProgram,
        )),
    ));
    let project_id = add_project(scenario.core_mut(), repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        scenario
            .execute(Command::CreateDraft {
                agent: Agent::Codex,
                project_id,
                title: "Install me".to_string(),
            })
            .await?,
    );
    let task_id = TaskId::new(draft.task.id.clone());

    let result = scenario
        .execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("go"),
        })
        .await;
    assert!(matches!(
        result,
        Err(CoreError::Harness(
            shep_harness_core::HarnessError::MissingProgram { .. }
        ))
    ));

    let workspace = scenario.core().task_workspace(&task_id)?;
    assert_eq!(workspace.task.state, TaskState::Ready);
    assert!(
        workspace.transcript[0]
            .summary()
            .contains("failure: agent not installed"),
        "unexpected row: {:?}",
        workspace.transcript[0].summary()
    );
    Ok(())
}

#[tokio::test]
async fn a_rejected_prompt_shows_a_sign_in_failure() -> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let mut scenario = TestCore::with_core(Core::with_services(
        Box::new(MemoryStore::new()),
        Box::new(FakeHarnessFactory::failing_prompt(
            shep_harness_core::fake_harness::FakeFailure::AuthRequired,
        )),
    ));
    let project_id = add_project(scenario.core_mut(), repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        scenario
            .execute(Command::CreateDraft {
                agent: Agent::Codex,
                project_id,
                title: "Sign in".to_string(),
            })
            .await?,
    );
    let task_id = TaskId::new(draft.task.id.clone());

    let result = scenario
        .execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text("go"),
        })
        .await;
    assert!(matches!(result, Err(CoreError::Harness(_))));

    let workspace = scenario.core().task_workspace(&task_id)?;
    assert_eq!(workspace.task.state, TaskState::Ready);
    assert_eq!(
        workspace.transcript.first().map(Entry::summary),
        Some("you: go".to_string())
    );
    assert!(
        workspace
            .transcript
            .last()
            .is_some_and(|entry| entry.summary().contains("failure: sign-in required")),
        "unexpected transcript: {:?}",
        workspace
            .transcript
            .iter()
            .map(Entry::summary)
            .collect::<Vec<_>>()
    );
    Ok(())
}

#[tokio::test]
async fn a_mid_turn_exit_is_a_failure_row_and_a_failed_task()
-> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let mut scenario = TestCore::with_core(Core::with_services(
        Box::new(MemoryStore::new()),
        Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::new(vec![
            FakeStep::Exit {
                code: 3,
                stderr: "the agent crashed".to_string(),
            },
        ])]])),
    ));
    let project_id = add_project(scenario.core_mut(), repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        scenario
            .execute(Command::CreateDraft {
                agent: Agent::Codex,
                project_id,
                title: "Crash me".to_string(),
            })
            .await?,
    );
    let task_id = TaskId::new(draft.task.id.clone());

    let workspace = workspace_of(
        scenario
            .execute(Command::SendPrompt {
                task_id,
                input: PromptInput::text("go"),
            })
            .await?,
    );
    assert_eq!(workspace.task.state, TaskState::Failed);
    assert!(
        workspace
            .transcript
            .last()
            .is_some_and(|entry| entry.summary().contains("failure: agent exited mid-turn")),
        "unexpected transcript: {:?}",
        workspace
            .transcript
            .iter()
            .map(Entry::summary)
            .collect::<Vec<_>>()
    );
    Ok(())
}

#[tokio::test]
async fn diagnostics_name_the_command_and_protocol() -> Result<(), Box<dyn std::error::Error>> {
    let repository = TestRepository::new()?;
    let mut core = Core::with_services(
        Box::new(MemoryStore::new()),
        Box::new(FakeHarnessFactory::new(vec![vec![FakeTurn::default()]])),
    );
    let project_id = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            agent: Agent::Codex,
            project_id,
            title: "Diagnose".to_string(),
        })
        .await?,
    );
    assert!(
        draft.diagnostics.is_none(),
        "a draft has no session to report"
    );
    assert!(draft.adapter_tier.is_none() && draft.capabilities.is_none());

    let workspace = workspace_of(
        core.execute(Command::SendPrompt {
            task_id: TaskId::new(draft.task.id.clone()),
            input: PromptInput::text("go"),
        })
        .await?,
    );
    let diagnostics = workspace
        .diagnostics
        .expect("a running session reports diagnostics");
    assert_eq!(diagnostics.command, "fake");
    assert_eq!(diagnostics.protocol, "fake-harness");
    assert!(diagnostics.stderr_tail.is_empty());
    assert_eq!(workspace.adapter_tier, Some(shep_core::AgentTier::Native));
    assert!(
        workspace
            .capabilities
            .is_some_and(|capabilities| capabilities.cancel)
    );
    Ok(())
}

#[tokio::test]
async fn prompts_queue_in_order_until_the_live_turn_finishes()
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
    let project = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            agent: Agent::Codex,
            project_id: project,
            title: "Queue prompts".to_string(),
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id);

    core.execute(Command::SendPrompt {
        task_id: task_id.clone(),
        input: PromptInput::text("First turn"),
    })
    .await?;
    assert_eq!(sent.try_recv()?.1.text, "First turn");
    let first_batch = "Preview annotations\n\nAnnotation 1\nInstruction: Increase contrast.";
    let second_batch = "Preview annotations\n\nAnnotation 1\nInstruction: Quiet the cancel action.";
    for text in [first_batch, second_batch] {
        let queued = workspace_of(
            core.execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text(text),
            })
            .await?,
        );
        assert_eq!(queued.task.state, TaskState::Working);
        assert_eq!(summaries(&queued), ["you: First turn"]);
    }
    assert!(sent.try_recv().is_err());

    sender.send(HarnessEvent::TurnFinished {
        reason: TurnFinishReason::Completed,
    })?;
    assert!(core.pump().await?);
    assert_eq!(sent.try_recv()?.1.text, first_batch);
    assert_eq!(
        core.task_workspace(&task_id)?.task.state,
        TaskState::Working
    );

    sender.send(HarnessEvent::TurnFinished {
        reason: TurnFinishReason::Completed,
    })?;
    assert!(core.pump().await?);
    assert_eq!(sent.try_recv()?.1.text, second_batch);
    assert_eq!(
        summaries(&core.task_workspace(&task_id)?),
        [
            "you: First turn".to_string(),
            format!("you: {first_batch}"),
            format!("you: {second_batch}"),
        ]
    );
    assert!(sent.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn queued_prompts_survive_reload_and_deliver_once() -> Result<(), Box<dyn std::error::Error>>
{
    let repository = TestRepository::new()?;
    let database = TestDatabase::new()?;
    let uri = database.uri();
    let (sender, receiver) = mpsc::unbounded_channel();
    let (prompts, mut sent) = mpsc::unbounded_channel();
    let mut core = Core::with_services(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(LateHarnessFactory {
            receiver: Mutex::new(Some(receiver)),
            prompts,
        }),
    );
    let project = add_project(&mut core, repository.path().to_path_buf()).await?;
    let draft = workspace_of(
        core.execute(Command::CreateDraft {
            agent: Agent::Codex,
            project_id: project,
            title: "Reload the queue".to_string(),
        })
        .await?,
    );
    let task_id = TaskId::new(draft.task.id);

    core.execute(Command::SendPrompt {
        task_id: task_id.clone(),
        input: PromptInput::text("First turn"),
    })
    .await?;
    assert_eq!(sent.try_recv()?.1.text, "First turn");
    let first_batch = "Preview annotations\n\nAnnotation 1\nInstruction: Increase contrast.";
    let second_batch = "Preview annotations\n\nAnnotation 1\nInstruction: Quiet the cancel action.";
    for text in [first_batch, second_batch] {
        core.execute(Command::SendPrompt {
            task_id: task_id.clone(),
            input: PromptInput::text(text),
        })
        .await?;
    }
    let persisted = core.store.load_prompt_queues().await?;
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted[0].0, task_id);
    assert_eq!(
        persisted[0]
            .1
            .iter()
            .map(|input| input.text.as_str())
            .collect::<Vec<_>>(),
        [first_batch, second_batch]
    );
    assert!(sent.try_recv().is_err());

    sender.send(HarnessEvent::TurnFinished {
        reason: TurnFinishReason::Completed,
    })?;
    assert!(core.pump().await?);
    assert_eq!(sent.try_recv()?.1.text, first_batch);
    let persisted = core.store.load_prompt_queues().await?;
    assert_eq!(persisted.len(), 1);
    assert_eq!(
        persisted[0]
            .1
            .iter()
            .map(|input| input.text.as_str())
            .collect::<Vec<_>>(),
        [second_batch]
    );
    drop(core);

    let (sender, receiver) = mpsc::unbounded_channel();
    let (prompts, mut sent) = mpsc::unbounded_channel();
    let mut reloaded = Core::load(
        Box::new(TursoStore::open(&uri).await?),
        Box::new(LateHarnessFactory {
            receiver: Mutex::new(Some(receiver)),
            prompts,
        }),
    )
    .await?;
    let restored = reloaded.open_task_workspace(&task_id).await?;
    assert_eq!(restored.task.state, TaskState::Interrupted);
    let persisted = reloaded.store.load_prompt_queues().await?;
    assert_eq!(persisted.len(), 1);
    assert_eq!(
        persisted[0]
            .1
            .iter()
            .map(|input| input.text.as_str())
            .collect::<Vec<_>>(),
        [second_batch]
    );

    let continued = workspace_of(
        reloaded
            .execute(Command::SendPrompt {
                task_id: task_id.clone(),
                input: PromptInput::text("continue"),
            })
            .await?,
    );
    assert_eq!(continued.task.state, TaskState::Working);
    assert_eq!(sent.try_recv()?.1.text, "continue");
    assert!(sent.try_recv().is_err());
    assert_eq!(
        reloaded.store.load_prompt_queues().await?.len(),
        1,
        "the queued batch waits for the current turn to finish"
    );

    sender.send(HarnessEvent::TurnFinished {
        reason: TurnFinishReason::Completed,
    })?;
    assert!(reloaded.pump().await?);
    assert_eq!(sent.try_recv()?.1.text, second_batch);
    assert!(sent.try_recv().is_err());
    assert!(reloaded.store.load_prompt_queues().await?.is_empty());

    sender.send(HarnessEvent::TurnFinished {
        reason: TurnFinishReason::Completed,
    })?;
    assert!(reloaded.pump().await?);
    let final_workspace = reloaded.open_task_workspace(&task_id).await?;
    assert_eq!(final_workspace.task.state, TaskState::Ready);
    let rows = summaries(&final_workspace);
    let first = rows
        .iter()
        .position(|row| row == &format!("you: {first_batch}"))
        .ok_or("the first queued batch should be in the transcript")?;
    let second = rows
        .iter()
        .position(|row| row == &format!("you: {second_batch}"))
        .ok_or("the second queued batch should be in the transcript")?;
    assert_eq!(
        rows.iter()
            .filter(|row| *row == &format!("you: {first_batch}"))
            .count(),
        1
    );
    assert_eq!(
        rows.iter()
            .filter(|row| *row == &format!("you: {second_batch}"))
            .count(),
        1
    );
    assert!(first < second, "queued batches must retain FIFO order");
    Ok(())
}
