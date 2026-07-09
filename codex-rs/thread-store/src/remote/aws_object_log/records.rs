use std::path::PathBuf;

use chrono::DateTime;
use chrono::Utc;
use codex_protocol::SessionId;
use codex_protocol::ThreadId;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::GitInfo;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_protocol::protocol::ThreadMemoryMode;
use codex_protocol::protocol::ThreadSource;
use codex_protocol::protocol::TokenUsage;

use crate::ExtraConfig;
use crate::StoredThreadConfigSnapshot;

use super::AwsObjectLogAppendResult;
use super::config::AwsObjectLogCompression;

pub(crate) const HEAD_SCHEMA: &str = "codex.thread.head.v1";
pub(crate) const COMMIT_SCHEMA: &str = "codex.thread.commit-pointer.v1";
pub(crate) const IDEMPOTENCY_SCHEMA: &str = "codex.thread.idempotency.v1";
pub(crate) const COMMIT_PAYLOAD_SCHEMA: &str = "codex.thread.commit-payload.v1";
pub(crate) const SNAPSHOT_PAYLOAD_SCHEMA: &str = "codex.thread.snapshot-payload.v1";
pub(crate) const GOAL_SCHEMA: &str = "codex.thread.goal.v1";

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct ThreadHeadRecord {
    pub schema: String,
    pub namespace: String,
    pub thread_id: ThreadId,
    pub session_id: SessionId,
    pub extra_config: Option<ExtraConfig>,
    pub config_snapshot: Option<StoredThreadConfigSnapshot>,
    pub forked_from_id: Option<ThreadId>,
    pub parent_thread_id: Option<ThreadId>,
    pub history_mode: ThreadHistoryMode,
    pub head_seq: u64,
    pub metadata: ThreadMetadataRecord,
    pub archived_at: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub latest_snapshot: Option<SnapshotPointerRecord>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct ThreadMetadataRecord {
    pub preview: String,
    pub name: Option<String>,
    pub model_provider: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<codex_protocol::openai_models::ReasoningEffort>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub recency_at: DateTime<Utc>,
    pub cwd: PathBuf,
    pub cli_version: String,
    pub source: SessionSource,
    pub thread_source: Option<ThreadSource>,
    pub agent_nickname: Option<String>,
    pub agent_role: Option<String>,
    pub agent_path: Option<String>,
    pub git_info: Option<GitInfo>,
    pub approval_mode: AskForApproval,
    pub permission_profile: PermissionProfile,
    pub token_usage: Option<TokenUsage>,
    pub first_user_message: Option<String>,
    pub memory_mode: ThreadMemoryMode,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct CommitPointerRecord {
    pub schema: String,
    pub namespace: String,
    pub thread_id: ThreadId,
    pub commit_id: String,
    pub start_seq: u64,
    pub end_seq: u64,
    pub item_count: usize,
    pub bucket: String,
    pub key: String,
    pub payload_len: usize,
    pub payload_sha256: String,
    pub compression: AwsObjectLogCompression,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct IdempotencyRecord {
    pub schema: String,
    pub namespace: String,
    pub thread_id: ThreadId,
    pub operation: String,
    pub idempotency_key: String,
    pub request_sha256: String,
    pub payload_sha256: String,
    pub result: AwsObjectLogAppendResult,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct ThreadGoalRecord {
    pub schema: String,
    pub namespace: String,
    pub thread_id: ThreadId,
    pub goal_id: String,
    pub objective: String,
    pub status: codex_state::ThreadGoalStatus,
    pub token_budget: Option<i64>,
    pub tokens_used: i64,
    pub time_used_seconds: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct SnapshotPointerRecord {
    pub seq: u64,
    pub bucket: String,
    pub key: String,
    pub payload_len: usize,
    pub payload_sha256: String,
    pub compression: AwsObjectLogCompression,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct CommitPayloadEnvelope {
    pub schema: String,
    pub namespace: String,
    pub thread_id: ThreadId,
    pub commit_id: String,
    pub start_seq: u64,
    pub end_seq: u64,
    pub items: Vec<SequencedRolloutItem>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct SnapshotPayloadEnvelope {
    pub schema: String,
    pub namespace: String,
    pub thread_id: ThreadId,
    pub seq: u64,
    pub items: Vec<SequencedRolloutItem>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct SequencedRolloutItem {
    pub seq: u64,
    pub item: RolloutItem,
}
