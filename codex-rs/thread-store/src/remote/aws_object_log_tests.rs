use codex_protocol::ThreadId;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Settings;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::PermissionProfile;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::SessionContextWindow;
use codex_protocol::protocol::SessionMeta;
use codex_protocol::protocol::SessionMetaLine;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_protocol::protocol::ThreadMemoryMode;
use pretty_assertions::assert_eq;

use crate::AppendThreadItemsParams;
use crate::AwsObjectLogAppendOptions;
use crate::AwsObjectLogThreadStore;
use crate::CreateThreadParams;
use crate::LoadThreadHistoryParams;
use crate::ReadThreadParams;
use crate::StoredThreadConfigSnapshot;
use crate::StoredThreadConfigSnapshotVersion;
use crate::ThreadPersistenceMetadata;
use crate::ThreadStoreError;

#[tokio::test]
async fn append_commits_payload_pointer_and_ordered_history() {
    let store = AwsObjectLogThreadStore::default();
    let thread_id = ThreadId::default();
    store
        .create_thread(create_thread_params(thread_id))
        .await
        .expect("create thread");

    let result = store
        .append_items_with_options(
            AppendThreadItemsParams {
                thread_id,
                items: vec![session_meta_item(thread_id, "append-window")],
            },
            AwsObjectLogAppendOptions {
                idempotency_key: Some("append-1".to_string()),
                expected_next_seq: Some(2),
            },
        )
        .await
        .expect("append items");

    assert_eq!(result.first_seq, 2);
    assert_eq!(result.last_seq, 2);
    assert_eq!(result.committed_item_count, 1);
    assert!(!result.idempotent_replay);

    let history = store
        .load_history(LoadThreadHistoryParams {
            thread_id,
            include_archived: false,
        })
        .await
        .expect("load history");
    assert_eq!(history.items.len(), 2);

    let payload_refs = store.payload_refs(thread_id).await;
    assert_eq!(payload_refs.len(), 2);
    assert_eq!(
        payload_refs[1],
        format!(
            "tenants/default/threads/{thread_id}/commits/00000000000000000002-00000000000000000002.json"
        )
    );
}

#[tokio::test]
async fn create_thread_persists_config_snapshot_on_stored_thread() {
    let store = AwsObjectLogThreadStore::default();
    let thread_id = ThreadId::default();
    let snapshot = stored_config_snapshot();
    let mut params = create_thread_params(thread_id);
    params.config_snapshot = Some(snapshot.clone());

    store.create_thread(params).await.expect("create thread");

    let stored_thread = store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: false,
            include_history: false,
        })
        .await
        .expect("read thread");

    assert_eq!(stored_thread.config_snapshot, Some(snapshot));
}

#[tokio::test]
async fn retry_with_same_idempotency_key_replays_original_commit() {
    let store = AwsObjectLogThreadStore::default();
    let thread_id = ThreadId::default();
    store
        .create_thread(create_thread_params(thread_id))
        .await
        .expect("create thread");
    let params = AppendThreadItemsParams {
        thread_id,
        items: vec![session_meta_item(thread_id, "append-window")],
    };
    let options = AwsObjectLogAppendOptions {
        idempotency_key: Some("append-1".to_string()),
        expected_next_seq: Some(2),
    };

    let first = store
        .append_items_with_options(params.clone(), options.clone())
        .await
        .expect("first append");
    let replay = store
        .append_items_with_options(params, options)
        .await
        .expect("idempotent replay");

    assert_eq!(replay.first_seq, first.first_seq);
    assert_eq!(replay.last_seq, first.last_seq);
    assert!(replay.idempotent_replay);

    let history = store
        .load_history(LoadThreadHistoryParams {
            thread_id,
            include_archived: false,
        })
        .await
        .expect("load history");
    assert_eq!(history.items.len(), 2);
}

#[tokio::test]
async fn same_idempotency_key_with_different_payload_conflicts() {
    let store = AwsObjectLogThreadStore::default();
    let thread_id = ThreadId::default();
    store
        .create_thread(create_thread_params(thread_id))
        .await
        .expect("create thread");

    store
        .append_items_with_options(
            AppendThreadItemsParams {
                thread_id,
                items: vec![session_meta_item(thread_id, "append-window")],
            },
            AwsObjectLogAppendOptions {
                idempotency_key: Some("append-1".to_string()),
                expected_next_seq: Some(2),
            },
        )
        .await
        .expect("first append");
    let err = store
        .append_items_with_options(
            AppendThreadItemsParams {
                thread_id,
                items: vec![session_meta_item(thread_id, "different-window")],
            },
            AwsObjectLogAppendOptions {
                idempotency_key: Some("append-1".to_string()),
                expected_next_seq: Some(2),
            },
        )
        .await
        .expect_err("different payload should conflict");

    assert!(matches!(err, ThreadStoreError::Conflict { .. }));
}

#[tokio::test]
async fn unexpected_head_sequence_fails_closed() {
    let store = AwsObjectLogThreadStore::default();
    let thread_id = ThreadId::default();
    store
        .create_thread(create_thread_params(thread_id))
        .await
        .expect("create thread");

    let err = store
        .append_items_with_options(
            AppendThreadItemsParams {
                thread_id,
                items: vec![session_meta_item(thread_id, "append-window")],
            },
            AwsObjectLogAppendOptions {
                idempotency_key: Some("append-1".to_string()),
                expected_next_seq: Some(3),
            },
        )
        .await
        .expect_err("unexpected sequence should fail");

    assert!(matches!(err, ThreadStoreError::Conflict { .. }));
}

fn create_thread_params(thread_id: ThreadId) -> CreateThreadParams {
    CreateThreadParams {
        session_id: thread_id.into(),
        thread_id,
        extra_config: None,
        config_snapshot: None,
        forked_from_id: None,
        parent_thread_id: None,
        source: SessionSource::Exec,
        thread_source: None,
        originator: "test_originator".to_string(),
        base_instructions: BaseInstructions::default(),
        dynamic_tools: Vec::new(),
        selected_capability_roots: Vec::new(),
        multi_agent_version: None,
        history_mode: ThreadHistoryMode::Legacy,
        initial_window_id: "initial-window".to_string(),
        metadata: ThreadPersistenceMetadata {
            cwd: None,
            model_provider: "test-provider".to_string(),
            memory_mode: ThreadMemoryMode::Enabled,
        },
    }
}

fn stored_config_snapshot() -> StoredThreadConfigSnapshot {
    let cwd = std::env::current_dir().expect("current dir");
    StoredThreadConfigSnapshot {
        version: StoredThreadConfigSnapshotVersion::V1,
        model: "snapshot-model".to_string(),
        model_provider_id: "snapshot-provider".to_string(),
        service_tier: Some("priority".to_string()),
        approval_policy: codex_protocol::protocol::AskForApproval::OnRequest,
        approvals_reviewer: codex_protocol::config_types::ApprovalsReviewer::User,
        permission_profile: PermissionProfile::read_only(),
        active_permission_profile: None,
        cwd: cwd.clone(),
        workspace_roots: vec![cwd],
        profile_workspace_roots: Vec::new(),
        ephemeral: false,
        reasoning_effort: Some(ReasoningEffort::High),
        reasoning_summary: None,
        personality: None,
        collaboration_mode: CollaborationMode {
            mode: ModeKind::Default,
            settings: Settings {
                model: "snapshot-model".to_string(),
                reasoning_effort: Some(ReasoningEffort::High),
                developer_instructions: None,
            },
        },
        session_source: SessionSource::Exec,
        history_mode: ThreadHistoryMode::Legacy,
        forked_from_thread_id: None,
        parent_thread_id: None,
        thread_source: None,
        originator: "test_originator".to_string(),
    }
}

fn session_meta_item(
    thread_id: ThreadId,
    window_id: &str,
) -> codex_protocol::protocol::RolloutItem {
    codex_protocol::protocol::RolloutItem::SessionMeta(SessionMetaLine {
        meta: SessionMeta {
            session_id: thread_id.into(),
            id: thread_id,
            source: SessionSource::Exec,
            history_mode: ThreadHistoryMode::Legacy,
            context_window: Some(SessionContextWindow::new(window_id.to_string())),
            ..SessionMeta::default()
        },
        git: None,
    })
}
