use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::PathBuf;

use chrono::DateTime;
use chrono::Utc;
use codex_protocol::ThreadId;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SessionContextWindow;
use codex_protocol::protocol::SessionMeta;
use codex_protocol::protocol::SessionMetaLine;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_protocol::protocol::ThreadMemoryMode;
use codex_rollout::persisted_rollout_items;

use crate::AppendThreadItemsParams;
use crate::ArchiveThreadParams;
use crate::CreateThreadParams;
use crate::DeleteThreadParams;
use crate::ExtraConfig;
use crate::ItemPage;
use crate::ListItemsParams;
use crate::ListThreadsParams;
use crate::ListTurnsParams;
use crate::LoadThreadHistoryParams;
use crate::ReadThreadByRolloutPathParams;
use crate::ReadThreadParams;
use crate::ResumeThreadParams;
use crate::SearchThreadsParams;
use crate::SortDirection;
use crate::StoredThread;
use crate::StoredThreadHistory;
use crate::ThreadMetadataPatch;
use crate::ThreadPage;
use crate::ThreadRelationFilter;
use crate::ThreadSearchPage;
use crate::ThreadSortKey;
use crate::ThreadStore;
use crate::ThreadStoreError;
use crate::ThreadStoreFuture;
use crate::ThreadStoreResult;
use crate::TurnPage;
use crate::UpdateThreadMetadataParams;
use crate::error::reject_paginated_history_mode;
use crate::types::canonical_history_mode_from_rollout_items;

/// Configuration for the AWS object-log DynamoDB + S3 thread store design.
///
/// This first implementation keeps the backing records in memory while
/// preserving the intended remote storage split: thread heads, commit pointers,
/// idempotency records, and payload objects are stored as separate records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AwsObjectLogThreadStoreConfig {
    pub namespace: String,
    pub payload_prefix: String,
}

impl Default for AwsObjectLogThreadStoreConfig {
    fn default() -> Self {
        Self {
            namespace: "default".to_string(),
            payload_prefix: "tenants/default/threads".to_string(),
        }
    }
}

/// Optional controls for a lower-level AWS object-log append.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AwsObjectLogAppendOptions {
    pub idempotency_key: Option<String>,
    pub expected_next_seq: Option<u64>,
}

/// Commit result produced by the AWS object-log append protocol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AwsObjectLogAppendResult {
    pub first_seq: u64,
    pub last_seq: u64,
    pub committed_item_count: usize,
    pub commit_id: String,
    pub idempotent_replay: bool,
}

/// AWS object-log thread-store prototype.
///
/// The store assumes an external owner guarantees that only one Codex process
/// is active for a thread. It still condition-checks the expected sequence so
/// accidental concurrent mutation fails closed instead of silently merging
/// histories.
#[derive(Debug)]
pub struct AwsObjectLogThreadStore {
    config: AwsObjectLogThreadStoreConfig,
    state: tokio::sync::Mutex<AwsObjectLogThreadStoreState>,
}

#[derive(Default, Debug)]
struct AwsObjectLogThreadStoreState {
    heads: HashMap<ThreadId, AwsObjectLogThreadHead>,
    commits: HashMap<ThreadId, BTreeMap<u64, AwsObjectLogCommitPointer>>,
    idempotency: HashMap<AwsObjectLogIdempotencyScope, AwsObjectLogIdempotencyRecord>,
    payloads: HashMap<String, AwsObjectLogCommitPayload>,
    live_threads: HashMap<ThreadId, AwsObjectLogLiveThreadContext>,
}

#[derive(Clone, Debug)]
struct AwsObjectLogThreadHead {
    thread_id: ThreadId,
    extra_config: Option<ExtraConfig>,
    forked_from_id: Option<ThreadId>,
    parent_thread_id: Option<ThreadId>,
    history_mode: ThreadHistoryMode,
    head_seq: u64,
    metadata: ThreadMetadataSnapshot,
    archived_at: Option<DateTime<Utc>>,
    deleted_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
struct ThreadMetadataSnapshot {
    preview: String,
    name: Option<String>,
    model_provider: String,
    model: Option<String>,
    reasoning_effort: Option<codex_protocol::openai_models::ReasoningEffort>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    recency_at: DateTime<Utc>,
    cwd: PathBuf,
    cli_version: String,
    source: SessionSource,
    thread_source: Option<codex_protocol::protocol::ThreadSource>,
    agent_nickname: Option<String>,
    agent_role: Option<String>,
    agent_path: Option<String>,
    git_info: Option<codex_protocol::protocol::GitInfo>,
    approval_mode: AskForApproval,
    permission_profile: PermissionProfile,
    token_usage: Option<codex_protocol::protocol::TokenUsage>,
    first_user_message: Option<String>,
    rollout_path: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct AwsObjectLogCommitPointer {
    start_seq: u64,
    end_seq: u64,
    item_count: usize,
    payload_ref: String,
}

#[derive(Clone, Debug)]
struct AwsObjectLogCommitPayload {
    items: Vec<SequencedRolloutItem>,
    payload_json: Vec<u8>,
}

#[derive(Clone, Debug)]
struct SequencedRolloutItem {
    seq: u64,
    item: RolloutItem,
}

#[derive(Clone, Debug, Default)]
struct AwsObjectLogLiveThreadContext {
    last_committed_seq: u64,
    append_ordinal: u64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct AwsObjectLogIdempotencyScope {
    thread_id: ThreadId,
    operation: &'static str,
    key: String,
}

#[derive(Clone, Debug)]
struct AwsObjectLogIdempotencyRecord {
    request_json: Vec<u8>,
    result: AwsObjectLogAppendResult,
}

impl AwsObjectLogThreadStore {
    pub fn new(config: AwsObjectLogThreadStoreConfig) -> Self {
        Self {
            config,
            state: tokio::sync::Mutex::new(AwsObjectLogThreadStoreState::default()),
        }
    }

    pub async fn append_items_with_options(
        &self,
        params: AppendThreadItemsParams,
        options: AwsObjectLogAppendOptions,
    ) -> ThreadStoreResult<AwsObjectLogAppendResult> {
        let canonical_items = persisted_rollout_items(params.items.as_slice());
        if canonical_items.is_empty() {
            return Ok(AwsObjectLogAppendResult {
                first_seq: 0,
                last_seq: 0,
                committed_item_count: 0,
                commit_id: String::new(),
                idempotent_replay: false,
            });
        }
        let request_json = serialize_items(canonical_items.as_slice())?;
        let mut state = self.state.lock().await;
        let idempotency_key = match options.idempotency_key {
            Some(key) => key,
            None => next_append_idempotency_key(&mut state, params.thread_id),
        };
        append_canonical_items(
            self.config.payload_prefix.as_str(),
            &mut state,
            params.thread_id,
            canonical_items,
            request_json,
            idempotency_key,
            options.expected_next_seq,
        )
    }

    #[cfg(test)]
    async fn payload_refs(&self, thread_id: ThreadId) -> Vec<String> {
        self.state
            .lock()
            .await
            .commits
            .get(&thread_id)
            .map(|commits| {
                commits
                    .values()
                    .map(|commit| commit.payload_ref.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn create_thread(&self, params: CreateThreadParams) -> ThreadStoreResult<()> {
        reject_paginated_history_mode(params.history_mode)?;
        let mut state = self.state.lock().await;
        if state.heads.contains_key(&params.thread_id) {
            return Err(ThreadStoreError::Conflict {
                message: format!("thread {} already exists", params.thread_id),
            });
        }
        let now = Utc::now();
        let session_meta = SessionMeta {
            session_id: params.session_id,
            id: params.thread_id,
            forked_from_id: params.forked_from_id,
            parent_thread_id: params.parent_thread_id,
            cwd: params.metadata.cwd.clone().unwrap_or_default(),
            agent_nickname: params.source.get_nickname(),
            agent_role: params.source.get_agent_role(),
            agent_path: params.source.get_agent_path().map(Into::into),
            originator: params.originator.clone(),
            source: params.source.clone(),
            thread_source: params.thread_source.clone(),
            model_provider: Some(params.metadata.model_provider.clone()),
            base_instructions: Some(params.base_instructions.clone()),
            dynamic_tools: (!params.dynamic_tools.is_empty()).then(|| params.dynamic_tools.clone()),
            selected_capability_roots: params.selected_capability_roots.clone(),
            memory_mode: matches!(params.metadata.memory_mode, ThreadMemoryMode::Disabled)
                .then_some("disabled".to_string()),
            history_mode: params.history_mode,
            multi_agent_version: params.multi_agent_version,
            context_window: Some(SessionContextWindow::new(params.initial_window_id.clone())),
            ..SessionMeta::default()
        };
        let head = AwsObjectLogThreadHead {
            thread_id: params.thread_id,
            extra_config: params.extra_config.clone(),
            forked_from_id: params.forked_from_id,
            parent_thread_id: params.parent_thread_id,
            history_mode: params.history_mode,
            head_seq: 0,
            metadata: ThreadMetadataSnapshot {
                preview: String::new(),
                name: None,
                model_provider: params.metadata.model_provider.clone(),
                model: None,
                reasoning_effort: None,
                created_at: now,
                updated_at: now,
                recency_at: now,
                cwd: params.metadata.cwd.unwrap_or_default(),
                cli_version: env!("CARGO_PKG_VERSION").to_string(),
                source: params.source,
                thread_source: params.thread_source,
                agent_nickname: None,
                agent_role: None,
                agent_path: None,
                git_info: None,
                approval_mode: AskForApproval::Never,
                permission_profile: PermissionProfile::read_only(),
                token_usage: None,
                first_user_message: None,
                rollout_path: None,
            },
            archived_at: None,
            deleted_at: None,
        };
        state.heads.insert(params.thread_id, head);
        state
            .live_threads
            .insert(params.thread_id, AwsObjectLogLiveThreadContext::default());
        let session_items = vec![RolloutItem::SessionMeta(SessionMetaLine {
            meta: session_meta,
            git: None,
        })];
        let request_json = serialize_items(session_items.as_slice())?;
        let result = append_canonical_items(
            self.config.payload_prefix.as_str(),
            &mut state,
            params.thread_id,
            session_items,
            request_json,
            "create-thread-session-meta".to_string(),
            Some(1),
        )?;
        state
            .live_threads
            .entry(params.thread_id)
            .or_default()
            .last_committed_seq = result.last_seq;
        Ok(())
    }

    async fn resume_thread(&self, params: ResumeThreadParams) -> ThreadStoreResult<()> {
        let mut state = self.state.lock().await;
        let Some(head) = state.heads.get(&params.thread_id) else {
            if let Some(history) = params.history {
                let history_mode = canonical_history_mode_from_rollout_items(history.as_slice());
                reject_paginated_history_mode(history_mode)?;
                import_history(
                    &self.config,
                    &mut state,
                    params.thread_id,
                    history.as_slice(),
                )?;
                return Ok(());
            }
            return Err(ThreadStoreError::ThreadNotFound {
                thread_id: params.thread_id,
            });
        };
        reject_paginated_history_mode(head.history_mode)?;
        if head.archived_at.is_some() && !params.include_archived {
            return Err(ThreadStoreError::InvalidRequest {
                message: format!("thread {} is archived", params.thread_id),
            });
        }
        let head_seq = head.head_seq;
        state.live_threads.insert(
            params.thread_id,
            AwsObjectLogLiveThreadContext {
                last_committed_seq: head_seq,
                append_ordinal: 0,
            },
        );
        Ok(())
    }

    async fn load_history(
        &self,
        params: LoadThreadHistoryParams,
    ) -> ThreadStoreResult<StoredThreadHistory> {
        let state = self.state.lock().await;
        let head = require_readable_head(&state, params.thread_id, params.include_archived)?;
        reject_paginated_history_mode(head.history_mode)?;
        Ok(StoredThreadHistory {
            thread_id: params.thread_id,
            items: load_items_from_state(&state, params.thread_id)?,
        })
    }

    async fn read_thread(&self, params: ReadThreadParams) -> ThreadStoreResult<StoredThread> {
        let state = self.state.lock().await;
        let head = require_readable_head(&state, params.thread_id, params.include_archived)?;
        reject_paginated_history_mode(head.history_mode)?;
        stored_thread_from_head(&state, head, params.include_history)
    }

    async fn list_threads(&self, params: ListThreadsParams) -> ThreadStoreResult<ThreadPage> {
        let state = self.state.lock().await;
        let mut items = state
            .heads
            .values()
            .filter(|head| head.deleted_at.is_none())
            .filter(|head| params.archived == head.archived_at.is_some())
            .filter(|head| {
                params
                    .model_providers
                    .as_ref()
                    .is_none_or(|providers| providers.contains(&head.metadata.model_provider))
            })
            .filter(|head| {
                params
                    .cwd_filters
                    .as_ref()
                    .is_none_or(|filters| filters.contains(&head.metadata.cwd))
            })
            .filter(|head| {
                params.search_term.as_ref().is_none_or(|term| {
                    head.metadata.preview.contains(term)
                        || head
                            .metadata
                            .name
                            .as_ref()
                            .is_some_and(|name| name.contains(term))
                })
            })
            .map(|head| stored_thread_from_head(&state, head, /*include_history*/ false))
            .collect::<ThreadStoreResult<Vec<_>>>()?;
        apply_relation_filter(&mut items, params.relation_filter);
        sort_threads(&mut items, params.sort_key, params.sort_direction);
        items.truncate(params.page_size);
        Ok(ThreadPage {
            items,
            next_cursor: None,
        })
    }

    async fn update_thread_metadata(
        &self,
        params: UpdateThreadMetadataParams,
    ) -> ThreadStoreResult<StoredThread> {
        let mut state = self.state.lock().await;
        {
            let head = require_mutable_head(&mut state, params.thread_id, params.include_archived)?;
            apply_metadata_patch(&mut head.metadata, params.patch);
        }
        let head = require_readable_head(&state, params.thread_id, params.include_archived)?;
        stored_thread_from_head(&state, head, /*include_history*/ false)
    }

    async fn archive_thread(&self, params: ArchiveThreadParams) -> ThreadStoreResult<()> {
        let mut state = self.state.lock().await;
        let head =
            require_mutable_head(&mut state, params.thread_id, /*include_archived*/ true)?;
        head.archived_at.get_or_insert_with(Utc::now);
        Ok(())
    }

    async fn unarchive_thread(
        &self,
        params: ArchiveThreadParams,
    ) -> ThreadStoreResult<StoredThread> {
        let mut state = self.state.lock().await;
        {
            let head =
                require_mutable_head(&mut state, params.thread_id, /*include_archived*/ true)?;
            head.archived_at = None;
        }
        let head = require_readable_head(&state, params.thread_id, /*include_archived*/ true)?;
        stored_thread_from_head(&state, head, /*include_history*/ false)
    }

    async fn delete_thread(&self, params: DeleteThreadParams) -> ThreadStoreResult<()> {
        let mut state = self.state.lock().await;
        let head =
            require_mutable_head(&mut state, params.thread_id, /*include_archived*/ true)?;
        head.deleted_at = Some(Utc::now());
        Ok(())
    }
}

impl Default for AwsObjectLogThreadStore {
    fn default() -> Self {
        Self::new(AwsObjectLogThreadStoreConfig::default())
    }
}

impl ThreadStore for AwsObjectLogThreadStore {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn create_thread(&self, params: CreateThreadParams) -> ThreadStoreFuture<'_, ()> {
        Box::pin(AwsObjectLogThreadStore::create_thread(self, params))
    }

    fn resume_thread(&self, params: ResumeThreadParams) -> ThreadStoreFuture<'_, ()> {
        Box::pin(AwsObjectLogThreadStore::resume_thread(self, params))
    }

    fn append_items(&self, params: AppendThreadItemsParams) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async move {
            self.append_items_with_options(params, AwsObjectLogAppendOptions::default())
                .await
                .map(|_| ())
        })
    }

    fn persist_thread(&self, _thread_id: ThreadId) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn flush_thread(&self, thread_id: ThreadId) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async move {
            let state = self.state.lock().await;
            let head = require_readable_head(&state, thread_id, /*include_archived*/ true)?;
            if let Some(live) = state.live_threads.get(&thread_id)
                && live.last_committed_seq > head.head_seq
            {
                return Err(ThreadStoreError::Internal {
                    message: format!("thread {thread_id} live context is ahead of committed head"),
                });
            }
            Ok(())
        })
    }

    fn shutdown_thread(&self, thread_id: ThreadId) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async move {
            self.state.lock().await.live_threads.remove(&thread_id);
            Ok(())
        })
    }

    fn discard_thread(&self, thread_id: ThreadId) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async move {
            self.state.lock().await.live_threads.remove(&thread_id);
            Ok(())
        })
    }

    fn load_history(
        &self,
        params: LoadThreadHistoryParams,
    ) -> ThreadStoreFuture<'_, StoredThreadHistory> {
        Box::pin(AwsObjectLogThreadStore::load_history(self, params))
    }

    fn read_thread(&self, params: ReadThreadParams) -> ThreadStoreFuture<'_, StoredThread> {
        Box::pin(AwsObjectLogThreadStore::read_thread(self, params))
    }

    fn read_thread_by_rollout_path(
        &self,
        _params: ReadThreadByRolloutPathParams,
    ) -> ThreadStoreFuture<'_, StoredThread> {
        Box::pin(async {
            Err(ThreadStoreError::Unsupported {
                operation: "read_thread_by_rollout_path",
            })
        })
    }

    fn list_threads(&self, params: ListThreadsParams) -> ThreadStoreFuture<'_, ThreadPage> {
        Box::pin(AwsObjectLogThreadStore::list_threads(self, params))
    }

    fn search_threads(
        &self,
        params: SearchThreadsParams,
    ) -> ThreadStoreFuture<'_, ThreadSearchPage> {
        Box::pin(async move {
            let page = self
                .list_threads(ListThreadsParams {
                    page_size: params.page_size,
                    cursor: params.cursor,
                    sort_key: params.sort_key,
                    sort_direction: params.sort_direction,
                    allowed_sources: params.allowed_sources,
                    model_providers: None,
                    cwd_filters: None,
                    archived: params.archived,
                    search_term: Some(params.search_term),
                    relation_filter: None,
                    use_state_db_only: true,
                })
                .await?;
            Ok(ThreadSearchPage {
                items: page
                    .items
                    .into_iter()
                    .map(|thread| crate::StoredThreadSearchResult {
                        snippet: thread.preview.clone(),
                        thread,
                    })
                    .collect(),
                next_cursor: page.next_cursor,
            })
        })
    }

    fn list_turns(&self, _params: ListTurnsParams) -> ThreadStoreFuture<'_, TurnPage> {
        Box::pin(async {
            Err(ThreadStoreError::Unsupported {
                operation: "list_turns",
            })
        })
    }

    fn list_items(&self, _params: ListItemsParams) -> ThreadStoreFuture<'_, ItemPage> {
        Box::pin(async {
            Err(ThreadStoreError::Unsupported {
                operation: "list_items",
            })
        })
    }

    fn update_thread_metadata(
        &self,
        params: UpdateThreadMetadataParams,
    ) -> ThreadStoreFuture<'_, StoredThread> {
        Box::pin(AwsObjectLogThreadStore::update_thread_metadata(
            self, params,
        ))
    }

    fn archive_thread(&self, params: ArchiveThreadParams) -> ThreadStoreFuture<'_, ()> {
        Box::pin(AwsObjectLogThreadStore::archive_thread(self, params))
    }

    fn unarchive_thread(&self, params: ArchiveThreadParams) -> ThreadStoreFuture<'_, StoredThread> {
        Box::pin(AwsObjectLogThreadStore::unarchive_thread(self, params))
    }

    fn delete_thread(&self, params: DeleteThreadParams) -> ThreadStoreFuture<'_, ()> {
        Box::pin(AwsObjectLogThreadStore::delete_thread(self, params))
    }
}

fn append_canonical_items(
    payload_prefix: &str,
    state: &mut AwsObjectLogThreadStoreState,
    thread_id: ThreadId,
    items: Vec<RolloutItem>,
    request_json: Vec<u8>,
    idempotency_key: String,
    expected_next_seq: Option<u64>,
) -> ThreadStoreResult<AwsObjectLogAppendResult> {
    let scope = AwsObjectLogIdempotencyScope {
        thread_id,
        operation: "append",
        key: idempotency_key,
    };
    if let Some(record) = state.idempotency.get(&scope) {
        if record.request_json != request_json {
            return Err(ThreadStoreError::Conflict {
                message: format!("idempotency key reused with a different append for {thread_id}"),
            });
        }
        let mut result = record.result.clone();
        result.idempotent_replay = true;
        return Ok(result);
    }

    let head = require_mutable_head(state, thread_id, /*include_archived*/ true)?;
    let first_seq = head.head_seq + 1;
    if let Some(expected_next_seq) = expected_next_seq
        && expected_next_seq != first_seq
    {
        return Err(ThreadStoreError::Conflict {
            message: format!(
                "thread {thread_id} expected next seq {expected_next_seq}, found {first_seq}"
            ),
        });
    }
    let last_seq = head.head_seq + items.len() as u64;
    let commit_id = format!("{first_seq:020}-{last_seq:020}");
    let payload_ref =
        format!("{payload_prefix}/{thread_id}/commits/{first_seq:020}-{last_seq:020}.json");
    let sequenced_items = items
        .into_iter()
        .enumerate()
        .map(|(offset, item)| SequencedRolloutItem {
            seq: first_seq + offset as u64,
            item,
        })
        .collect::<Vec<_>>();
    let payload = AwsObjectLogCommitPayload {
        items: sequenced_items,
        payload_json: request_json.clone(),
    };
    state.payloads.insert(payload_ref.clone(), payload);
    state.commits.entry(thread_id).or_default().insert(
        first_seq,
        AwsObjectLogCommitPointer {
            start_seq: first_seq,
            end_seq: last_seq,
            item_count: (last_seq - first_seq + 1) as usize,
            payload_ref,
        },
    );
    let result = AwsObjectLogAppendResult {
        first_seq,
        last_seq,
        committed_item_count: (last_seq - first_seq + 1) as usize,
        commit_id,
        idempotent_replay: false,
    };
    state.idempotency.insert(
        scope,
        AwsObjectLogIdempotencyRecord {
            request_json,
            result: result.clone(),
        },
    );
    let head = require_mutable_head(state, thread_id, /*include_archived*/ true)?;
    head.head_seq = last_seq;
    head.metadata.updated_at = Utc::now();
    state
        .live_threads
        .entry(thread_id)
        .or_default()
        .last_committed_seq = last_seq;
    Ok(result)
}

fn next_append_idempotency_key(
    state: &mut AwsObjectLogThreadStoreState,
    thread_id: ThreadId,
) -> String {
    let live = state.live_threads.entry(thread_id).or_default();
    live.append_ordinal += 1;
    format!("append-{}", live.append_ordinal)
}

fn import_history(
    config: &AwsObjectLogThreadStoreConfig,
    state: &mut AwsObjectLogThreadStoreState,
    thread_id: ThreadId,
    history: &[RolloutItem],
) -> ThreadStoreResult<()> {
    if state.heads.contains_key(&thread_id) {
        return Err(ThreadStoreError::Conflict {
            message: format!("thread {thread_id} already exists"),
        });
    }
    let now = Utc::now();
    let history_mode = canonical_history_mode_from_rollout_items(history);
    let session_meta = history.iter().find_map(|item| match item {
        RolloutItem::SessionMeta(meta_line) => Some(&meta_line.meta),
        _ => None,
    });
    state.heads.insert(
        thread_id,
        AwsObjectLogThreadHead {
            thread_id,
            extra_config: None,
            forked_from_id: session_meta.and_then(|meta| meta.forked_from_id),
            parent_thread_id: session_meta.and_then(|meta| meta.parent_thread_id),
            history_mode,
            head_seq: 0,
            metadata: ThreadMetadataSnapshot {
                preview: String::new(),
                name: None,
                model_provider: session_meta
                    .and_then(|meta| meta.model_provider.clone())
                    .unwrap_or_else(|| "unknown".to_string()),
                model: None,
                reasoning_effort: None,
                created_at: now,
                updated_at: now,
                recency_at: now,
                cwd: session_meta
                    .map(|meta| meta.cwd.clone())
                    .unwrap_or_default(),
                cli_version: "imported".to_string(),
                source: session_meta
                    .map(|meta| meta.source.clone())
                    .unwrap_or(SessionSource::Exec),
                thread_source: session_meta.and_then(|meta| meta.thread_source.clone()),
                agent_nickname: session_meta.and_then(|meta| meta.agent_nickname.clone()),
                agent_role: session_meta.and_then(|meta| meta.agent_role.clone()),
                agent_path: session_meta.and_then(|meta| meta.agent_path.clone()),
                git_info: None,
                approval_mode: AskForApproval::Never,
                permission_profile: PermissionProfile::read_only(),
                token_usage: None,
                first_user_message: None,
                rollout_path: None,
            },
            archived_at: None,
            deleted_at: None,
        },
    );
    state
        .live_threads
        .insert(thread_id, AwsObjectLogLiveThreadContext::default());
    let items = persisted_rollout_items(history);
    let request_json = serialize_items(items.as_slice())?;
    let result = append_canonical_items(
        config.payload_prefix.as_str(),
        state,
        thread_id,
        items,
        request_json,
        "import-history".to_string(),
        Some(1),
    )?;
    state
        .live_threads
        .entry(thread_id)
        .or_default()
        .last_committed_seq = result.last_seq;
    Ok(())
}

fn serialize_items(items: &[RolloutItem]) -> ThreadStoreResult<Vec<u8>> {
    serde_json::to_vec(items).map_err(|err| ThreadStoreError::Internal {
        message: format!("failed to serialize rollout items: {err}"),
    })
}

fn load_items_from_state(
    state: &AwsObjectLogThreadStoreState,
    thread_id: ThreadId,
) -> ThreadStoreResult<Vec<RolloutItem>> {
    let Some(commits) = state.commits.get(&thread_id) else {
        return Ok(Vec::new());
    };
    let mut items = Vec::new();
    let mut expected_seq = 1;
    for commit in commits.values() {
        if commit.start_seq != expected_seq {
            return Err(ThreadStoreError::Internal {
                message: format!(
                    "thread {thread_id} has commit gap at seq {expected_seq}, found {}",
                    commit.start_seq
                ),
            });
        }
        let payload =
            state
                .payloads
                .get(&commit.payload_ref)
                .ok_or_else(|| ThreadStoreError::Internal {
                    message: format!("missing payload {}", commit.payload_ref),
                })?;
        let _payload_json_len = payload.payload_json.len();
        if payload.items.len() != commit.item_count {
            return Err(ThreadStoreError::Internal {
                message: format!(
                    "thread {thread_id} payload item count mismatch for {}",
                    commit.payload_ref
                ),
            });
        }
        for item in &payload.items {
            if item.seq != expected_seq {
                return Err(ThreadStoreError::Internal {
                    message: format!(
                        "thread {thread_id} expected seq {expected_seq}, found {}",
                        item.seq
                    ),
                });
            }
            items.push(item.item.clone());
            expected_seq += 1;
        }
        if commit.end_seq + 1 != expected_seq {
            return Err(ThreadStoreError::Internal {
                message: format!(
                    "thread {thread_id} commit range mismatch ending at {}",
                    commit.end_seq
                ),
            });
        }
    }
    Ok(items)
}

fn require_readable_head(
    state: &AwsObjectLogThreadStoreState,
    thread_id: ThreadId,
    include_archived: bool,
) -> ThreadStoreResult<&AwsObjectLogThreadHead> {
    let head = state
        .heads
        .get(&thread_id)
        .ok_or(ThreadStoreError::ThreadNotFound { thread_id })?;
    if head.deleted_at.is_some() {
        return Err(ThreadStoreError::ThreadNotFound { thread_id });
    }
    if head.archived_at.is_some() && !include_archived {
        return Err(ThreadStoreError::InvalidRequest {
            message: format!("thread {thread_id} is archived"),
        });
    }
    Ok(head)
}

fn require_mutable_head(
    state: &mut AwsObjectLogThreadStoreState,
    thread_id: ThreadId,
    include_archived: bool,
) -> ThreadStoreResult<&mut AwsObjectLogThreadHead> {
    let head = state
        .heads
        .get_mut(&thread_id)
        .ok_or(ThreadStoreError::ThreadNotFound { thread_id })?;
    if head.deleted_at.is_some() {
        return Err(ThreadStoreError::ThreadNotFound { thread_id });
    }
    if head.archived_at.is_some() && !include_archived {
        return Err(ThreadStoreError::InvalidRequest {
            message: format!("thread {thread_id} is archived"),
        });
    }
    Ok(head)
}

fn stored_thread_from_head(
    state: &AwsObjectLogThreadStoreState,
    head: &AwsObjectLogThreadHead,
    include_history: bool,
) -> ThreadStoreResult<StoredThread> {
    let history = if include_history {
        Some(StoredThreadHistory {
            thread_id: head.thread_id,
            items: load_items_from_state(state, head.thread_id)?,
        })
    } else {
        None
    };
    Ok(StoredThread {
        thread_id: head.thread_id,
        extra_config: head.extra_config.clone(),
        rollout_path: head.metadata.rollout_path.clone(),
        forked_from_id: head.forked_from_id,
        parent_thread_id: head.parent_thread_id,
        preview: head.metadata.preview.clone(),
        name: head.metadata.name.clone(),
        model_provider: head.metadata.model_provider.clone(),
        model: head.metadata.model.clone(),
        reasoning_effort: head.metadata.reasoning_effort.clone(),
        created_at: head.metadata.created_at,
        updated_at: head.metadata.updated_at,
        recency_at: head.metadata.recency_at,
        archived_at: head.archived_at,
        cwd: head.metadata.cwd.clone(),
        cli_version: head.metadata.cli_version.clone(),
        source: head.metadata.source.clone(),
        history_mode: head.history_mode,
        thread_source: head.metadata.thread_source.clone(),
        agent_nickname: head.metadata.agent_nickname.clone(),
        agent_role: head.metadata.agent_role.clone(),
        agent_path: head.metadata.agent_path.clone(),
        git_info: head.metadata.git_info.clone(),
        approval_mode: head.metadata.approval_mode,
        permission_profile: head.metadata.permission_profile.clone(),
        token_usage: head.metadata.token_usage.clone(),
        first_user_message: head.metadata.first_user_message.clone(),
        history,
    })
}

fn apply_metadata_patch(metadata: &mut ThreadMetadataSnapshot, patch: ThreadMetadataPatch) {
    if let Some(name) = patch.name {
        metadata.name = name;
    }
    if let Some(rollout_path) = patch.rollout_path {
        metadata.rollout_path = Some(rollout_path);
    }
    if let Some(preview) = patch.preview {
        metadata.preview = preview;
    }
    if let Some(title) = patch.title {
        metadata.name = Some(title);
    }
    if let Some(model_provider) = patch.model_provider {
        metadata.model_provider = model_provider;
    }
    if let Some(model) = patch.model {
        metadata.model = Some(model);
    }
    if let Some(reasoning_effort) = patch.reasoning_effort {
        metadata.reasoning_effort = Some(reasoning_effort);
    }
    if let Some(created_at) = patch.created_at {
        metadata.created_at = created_at;
    }
    if let Some(updated_at) = patch.updated_at {
        metadata.updated_at = updated_at;
    }
    if let Some(recency_at) = patch.advance_recency_at
        && recency_at > metadata.recency_at
    {
        metadata.recency_at = recency_at;
    }
    if let Some(source) = patch.source {
        metadata.source = source;
    }
    if let Some(thread_source) = patch.thread_source {
        metadata.thread_source = thread_source;
    }
    if let Some(agent_nickname) = patch.agent_nickname {
        metadata.agent_nickname = agent_nickname;
    }
    if let Some(agent_role) = patch.agent_role {
        metadata.agent_role = agent_role;
    }
    if let Some(agent_path) = patch.agent_path {
        metadata.agent_path = agent_path;
    }
    if let Some(cwd) = patch.cwd {
        metadata.cwd = cwd;
    }
    if let Some(cli_version) = patch.cli_version {
        metadata.cli_version = cli_version;
    }
    if let Some(approval_mode) = patch.approval_mode {
        metadata.approval_mode = approval_mode;
    }
    if let Some(permission_profile) = patch.permission_profile {
        metadata.permission_profile = permission_profile;
    }
    if let Some(token_usage) = patch.token_usage {
        metadata.token_usage = Some(token_usage);
    }
    if let Some(first_user_message) = patch.first_user_message {
        metadata.first_user_message = Some(first_user_message);
    }
    if let Some(git_info) = patch.git_info {
        metadata.git_info = git_info_from_patch(git_info);
    }
    if let Some(memory_mode) = patch.memory_mode
        && matches!(memory_mode, ThreadMemoryMode::Disabled)
    {
        metadata.preview = metadata.preview.clone();
    }
}

fn git_info_from_patch(patch: crate::GitInfoPatch) -> Option<codex_protocol::protocol::GitInfo> {
    let sha = patch.sha.flatten();
    let branch = patch.branch.flatten();
    let origin_url = patch.origin_url.flatten();
    if sha.is_none() && branch.is_none() && origin_url.is_none() {
        return None;
    }
    Some(codex_protocol::protocol::GitInfo {
        commit_hash: sha.as_deref().map(codex_git_utils::GitSha::new),
        branch,
        repository_url: origin_url,
    })
}

fn sort_threads(
    items: &mut [StoredThread],
    sort_key: ThreadSortKey,
    sort_direction: SortDirection,
) {
    items.sort_by(|left, right| {
        let ordering = match sort_key {
            ThreadSortKey::CreatedAt => left.created_at.cmp(&right.created_at),
            ThreadSortKey::UpdatedAt => left.updated_at.cmp(&right.updated_at),
            ThreadSortKey::RecencyAt => left.recency_at.cmp(&right.recency_at),
        };
        match sort_direction {
            SortDirection::Asc => ordering,
            SortDirection::Desc => ordering.reverse(),
        }
    });
}

fn apply_relation_filter(
    items: &mut Vec<StoredThread>,
    relation_filter: Option<ThreadRelationFilter>,
) {
    match relation_filter {
        Some(ThreadRelationFilter::DirectChildrenOf(parent_thread_id)) => {
            items.retain(|thread| thread.parent_thread_id == Some(parent_thread_id));
        }
        Some(ThreadRelationFilter::DescendantsOf(ancestor_thread_id)) => {
            let mut subtree = std::collections::HashSet::from([ancestor_thread_id]);
            loop {
                let mut discovered = false;
                for thread in items.iter() {
                    if thread
                        .parent_thread_id
                        .is_some_and(|parent_thread_id| subtree.contains(&parent_thread_id))
                    {
                        discovered |= subtree.insert(thread.thread_id);
                    }
                }
                if !discovered {
                    break;
                }
            }
            items.retain(|thread| {
                thread.thread_id != ancestor_thread_id && subtree.contains(&thread.thread_id)
            });
        }
        None => {}
    }
}

#[cfg(test)]
#[path = "aws_object_log_tests.rs"]
mod tests;
