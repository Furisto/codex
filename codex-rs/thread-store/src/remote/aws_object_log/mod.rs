mod config;
mod records;

use std::collections::HashMap;
use std::fmt::Debug;
use std::time::Instant;

use aws_config::BehaviorVersion;
use aws_sdk_dynamodb::Client as DynamoDbClient;
use aws_sdk_dynamodb::types::AttributeValue;
use aws_sdk_dynamodb::types::Put;
use aws_sdk_dynamodb::types::TransactWriteItem;
use aws_sdk_s3::Client as S3Client;
use aws_sdk_s3::primitives::ByteStream;
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
use codex_protocol::protocol::ThreadMemoryMode;
use codex_rollout::persisted_rollout_items;
use sha2::Digest;
use sha2::Sha256;
use tokio::sync::OnceCell;

pub use config::AwsObjectLogCompression;
pub use config::AwsObjectLogThreadStoreConfig;
use records::COMMIT_PAYLOAD_SCHEMA;
use records::COMMIT_SCHEMA;
use records::CommitPayloadEnvelope;
use records::CommitPointerRecord;
use records::HEAD_SCHEMA;
use records::IDEMPOTENCY_SCHEMA;
use records::IdempotencyRecord;
use records::SNAPSHOT_PAYLOAD_SCHEMA;
use records::SequencedRolloutItem;
use records::SnapshotPayloadEnvelope;
use records::SnapshotPointerRecord;
use records::ThreadHeadRecord;
use records::ThreadMetadataRecord;

use crate::AppendThreadItemsParams;
use crate::ArchiveThreadParams;
use crate::CreateThreadParams;
use crate::DeleteThreadParams;
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
use crate::types::canonical_history_mode_from_rollout_items;

const HEAD_SK: &str = "HEAD";
const RECORD_JSON_ATTR: &str = "record_json";

/// Optional controls for a lower-level AWS object-log append.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AwsObjectLogAppendOptions {
    pub idempotency_key: Option<String>,
    pub expected_next_seq: Option<u64>,
}

/// Commit result produced by the AWS object-log append protocol.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AwsObjectLogAppendResult {
    pub first_seq: u64,
    pub last_seq: u64,
    pub committed_item_count: usize,
    pub commit_id: String,
    pub idempotent_replay: bool,
}

#[derive(Debug)]
struct AwsObjectLogClients {
    dynamodb: DynamoDbClient,
    s3: S3Client,
}

/// Durable AWS object-log thread store backed by DynamoDB and S3.
#[derive(Debug)]
pub struct AwsObjectLogThreadStore {
    config: AwsObjectLogThreadStoreConfig,
    clients: OnceCell<AwsObjectLogClients>,
}

impl AwsObjectLogThreadStore {
    pub fn new(config: AwsObjectLogThreadStoreConfig) -> Self {
        Self {
            config,
            clients: OnceCell::new(),
        }
    }

    pub async fn from_config(config: AwsObjectLogThreadStoreConfig) -> ThreadStoreResult<Self> {
        let store = Self::new(config);
        store.clients().await?;
        Ok(store)
    }

    pub fn from_clients(
        config: AwsObjectLogThreadStoreConfig,
        dynamodb: DynamoDbClient,
        s3: S3Client,
    ) -> Self {
        let clients = OnceCell::new();
        clients
            .set(AwsObjectLogClients { dynamodb, s3 })
            .expect("clients cell should be empty");
        Self { config, clients }
    }

    async fn clients(&self) -> ThreadStoreResult<&AwsObjectLogClients> {
        self.clients
            .get_or_try_init(|| async {
                let mut loader = aws_config::defaults(BehaviorVersion::latest());
                if let Some(region) = self.config.aws_region.clone() {
                    loader = loader.region(aws_config::Region::new(region));
                }
                if let Some(endpoint_url) = self.config.endpoint_url.clone() {
                    loader = loader.endpoint_url(endpoint_url);
                }
                let sdk_config = loader.load().await;
                Ok::<_, ThreadStoreError>(AwsObjectLogClients {
                    dynamodb: DynamoDbClient::new(&sdk_config),
                    s3: S3Client::new(&sdk_config),
                })
            })
            .await
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
        let idempotency_key = options
            .idempotency_key
            .or(params.idempotency_key)
            .filter(|key| !key.is_empty())
            .ok_or_else(|| ThreadStoreError::InvalidRequest {
                message: "AWS object-log append requires an idempotency key".to_string(),
            })?;
        let expected_next_seq = options.expected_next_seq.or(params.expected_next_seq);
        let result = self
            .append_canonical_items(
                params.thread_id,
                canonical_items,
                idempotency_key,
                expected_next_seq,
            )
            .await?;
        self.ensure_snapshot(params.thread_id).await?;
        Ok(result)
    }

    async fn create_thread(&self, params: CreateThreadParams) -> ThreadStoreResult<()> {
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
        let items = vec![RolloutItem::SessionMeta(SessionMetaLine {
            meta: session_meta,
            git: None,
        })];
        let head = ThreadHeadRecord {
            schema: HEAD_SCHEMA.to_string(),
            namespace: self.config.namespace.clone(),
            thread_id: params.thread_id,
            session_id: params.session_id,
            extra_config: params.extra_config,
            config_snapshot: params.config_snapshot,
            forked_from_id: params.forked_from_id,
            parent_thread_id: params.parent_thread_id,
            history_mode: params.history_mode,
            head_seq: items.len() as u64,
            metadata: ThreadMetadataRecord {
                preview: String::new(),
                name: None,
                model_provider: params.metadata.model_provider,
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
                memory_mode: params.metadata.memory_mode,
            },
            archived_at: None,
            deleted_at: None,
            latest_snapshot: None,
        };
        let request_sha256 = sha256_json(&items)?;
        let payload = self
            .put_commit_payload(
                params.thread_id,
                items,
                1,
                head.head_seq,
                "create-thread-session-meta",
            )
            .await?;
        let result = append_result(1, head.head_seq, "create-thread-session-meta", false);
        let idempotency = IdempotencyRecord {
            schema: IDEMPOTENCY_SCHEMA.to_string(),
            namespace: self.config.namespace.clone(),
            thread_id: params.thread_id,
            operation: "append".to_string(),
            idempotency_key: "create-thread-session-meta".to_string(),
            request_sha256,
            payload_sha256: payload.payload_sha256.clone(),
            result,
            created_at: now,
        };
        let clients = self.clients().await?;
        clients
            .dynamodb
            .transact_write_items()
            .transact_items(put_item_tx(
                self.config.table_name.as_str(),
                head_item(&self.config, &head)?,
                "attribute_not_exists(pk)",
                HashMap::new(),
            )?)
            .transact_items(put_item_tx(
                self.config.table_name.as_str(),
                commit_item(&self.config, &payload)?,
                "attribute_not_exists(pk)",
                HashMap::new(),
            )?)
            .transact_items(put_item_tx(
                self.config.table_name.as_str(),
                idempotency_item(&self.config, &idempotency)?,
                "attribute_not_exists(pk)",
                HashMap::new(),
            )?)
            .send()
            .await
            .map_err(|err| ThreadStoreError::Conflict {
                message: format!(
                    "failed to create AWS object-log thread {}: {err}",
                    params.thread_id
                ),
            })?;
        self.ensure_snapshot(params.thread_id).await
    }

    async fn resume_thread(&self, params: ResumeThreadParams) -> ThreadStoreResult<()> {
        match self
            .read_head(params.thread_id, params.include_archived)
            .await
        {
            Ok(_) => Ok(()),
            Err(ThreadStoreError::ThreadNotFound { .. }) => {
                let Some(history) = params.history else {
                    return Err(ThreadStoreError::ThreadNotFound {
                        thread_id: params.thread_id,
                    });
                };
                self.import_history(params.thread_id, history.as_slice())
                    .await
            }
            Err(err) => Err(err),
        }
    }

    async fn append_canonical_items(
        &self,
        thread_id: ThreadId,
        items: Vec<RolloutItem>,
        idempotency_key: String,
        expected_next_seq: Option<u64>,
    ) -> ThreadStoreResult<AwsObjectLogAppendResult> {
        let request_sha256 = sha256_json(&items)?;
        if let Some(record) = self
            .read_idempotency_record(thread_id, idempotency_key.as_str())
            .await?
        {
            if record.request_sha256 != request_sha256 {
                return Err(ThreadStoreError::Conflict {
                    message: format!(
                        "idempotency key reused with a different append for {thread_id}"
                    ),
                });
            }
            let mut result = record.result;
            result.idempotent_replay = true;
            return Ok(result);
        }
        let mut head = self.read_head(thread_id, /*include_archived*/ true).await?;
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
        let payload = self
            .put_commit_payload(
                thread_id,
                items,
                first_seq,
                last_seq,
                idempotency_key.as_str(),
            )
            .await?;
        let result = append_result(first_seq, last_seq, payload.commit_id.as_str(), false);
        let idempotency = IdempotencyRecord {
            schema: IDEMPOTENCY_SCHEMA.to_string(),
            namespace: self.config.namespace.clone(),
            thread_id,
            operation: "append".to_string(),
            idempotency_key: idempotency_key.clone(),
            request_sha256,
            payload_sha256: payload.payload_sha256.clone(),
            result: result.clone(),
            created_at: Utc::now(),
        };
        let previous_head_seq = head.head_seq;
        head.head_seq = last_seq;
        head.metadata.updated_at = Utc::now();
        let clients = self.clients().await?;
        let tx_result = clients
            .dynamodb
            .transact_write_items()
            .transact_items(put_item_tx(
                self.config.table_name.as_str(),
                head_item(&self.config, &head)?,
                "attribute_exists(pk) AND head_seq = :previous_head_seq",
                HashMap::from([(":previous_head_seq".to_string(), av_n(previous_head_seq))]),
            )?)
            .transact_items(put_item_tx(
                self.config.table_name.as_str(),
                commit_item(&self.config, &payload)?,
                "attribute_not_exists(pk)",
                HashMap::new(),
            )?)
            .transact_items(put_item_tx(
                self.config.table_name.as_str(),
                idempotency_item(&self.config, &idempotency)?,
                "attribute_not_exists(pk)",
                HashMap::new(),
            )?)
            .send()
            .await;
        match tx_result {
            Ok(_) => Ok(result),
            Err(err) => {
                if let Some(record) = self
                    .read_idempotency_record(thread_id, idempotency_key.as_str())
                    .await?
                    && record.request_sha256 == idempotency.request_sha256
                {
                    let mut result = record.result;
                    result.idempotent_replay = true;
                    return Ok(result);
                }
                Err(ThreadStoreError::Conflict {
                    message: format!(
                        "failed to append AWS object-log commit for {thread_id}: {err}"
                    ),
                })
            }
        }
    }

    async fn load_history(
        &self,
        params: LoadThreadHistoryParams,
    ) -> ThreadStoreResult<StoredThreadHistory> {
        let started_at = Instant::now();
        let head = self
            .read_head(params.thread_id, params.include_archived)
            .await?;
        let head_seq = head.head_seq;
        let history_started_at = Instant::now();
        let items = self.load_items_for_head(&head).await?;
        tracing::info!(
            thread_id = %params.thread_id,
            head_seq,
            history_item_count = items.len(),
            history_load_elapsed_ms = elapsed_ms(history_started_at),
            elapsed_ms = elapsed_ms(started_at),
            "AWS object-log load_history completed"
        );
        Ok(StoredThreadHistory {
            thread_id: params.thread_id,
            items,
        })
    }

    async fn read_thread(&self, params: ReadThreadParams) -> ThreadStoreResult<StoredThread> {
        let started_at = Instant::now();
        let head = self
            .read_head(params.thread_id, params.include_archived)
            .await?;
        let head_seq = head.head_seq;
        let stored_thread_started_at = Instant::now();
        let thread = self
            .stored_thread_from_head(head, params.include_history)
            .await?;
        tracing::info!(
            thread_id = %params.thread_id,
            include_history = params.include_history,
            head_seq,
            stored_thread_elapsed_ms = elapsed_ms(stored_thread_started_at),
            history_item_count = thread.history.as_ref().map(|history| history.items.len()),
            elapsed_ms = elapsed_ms(started_at),
            "AWS object-log read_thread completed"
        );
        Ok(thread)
    }

    async fn list_threads(&self, params: ListThreadsParams) -> ThreadStoreResult<ThreadPage> {
        let state = if params.archived {
            "archived"
        } else {
            "active"
        };
        let index_name = match params.sort_key {
            ThreadSortKey::CreatedAt => self.config.gsi_created_index_name.as_str(),
            ThreadSortKey::UpdatedAt | ThreadSortKey::RecencyAt => {
                self.config.gsi_updated_index_name.as_str()
            }
        };
        let clients = self.clients().await?;
        let mut query = clients
            .dynamodb
            .query()
            .table_name(self.config.table_name.as_str())
            .index_name(index_name)
            .scan_index_forward(matches!(params.sort_direction, SortDirection::Asc))
            .limit(params.page_size as i32);
        query = if matches!(params.sort_key, ThreadSortKey::CreatedAt) {
            query
                .key_condition_expression("gsi2pk = :index_pk")
                .expression_attribute_values(":index_pk", av_s(index_pk(&self.config, state)))
        } else {
            query
                .key_condition_expression("gsi1pk = :index_pk")
                .expression_attribute_values(":index_pk", av_s(index_pk(&self.config, state)))
        };
        let output = query
            .send()
            .await
            .map_err(internal_aws_error("failed to list AWS object-log threads"))?;
        let mut items = output
            .items
            .unwrap_or_default()
            .into_iter()
            .map(|item| decode_head_item(&item))
            .collect::<ThreadStoreResult<Vec<_>>>()?
            .into_iter()
            .filter(|head| params.archived == head.archived_at.is_some())
            .filter(|head| head.deleted_at.is_none())
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
            .map(|head| stored_thread_from_head_record(head, None))
            .collect::<Vec<_>>();
        apply_relation_filter(&mut items, params.relation_filter);
        if matches!(params.sort_key, ThreadSortKey::RecencyAt) {
            sort_threads(&mut items, params.sort_key, params.sort_direction);
        }
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
        let mut head = self
            .read_head(params.thread_id, params.include_archived)
            .await?;
        apply_metadata_patch(&mut head.metadata, params.patch);
        self.put_existing_head(&head).await?;
        Ok(stored_thread_from_head_record(head, None))
    }

    async fn archive_thread(&self, params: ArchiveThreadParams) -> ThreadStoreResult<()> {
        let mut head = self.read_head(params.thread_id, true).await?;
        head.archived_at.get_or_insert_with(Utc::now);
        self.put_existing_head(&head).await
    }

    async fn unarchive_thread(
        &self,
        params: ArchiveThreadParams,
    ) -> ThreadStoreResult<StoredThread> {
        let mut head = self.read_head(params.thread_id, true).await?;
        head.archived_at = None;
        self.put_existing_head(&head).await?;
        Ok(stored_thread_from_head_record(head, None))
    }

    async fn delete_thread(&self, params: DeleteThreadParams) -> ThreadStoreResult<()> {
        let mut head = self.read_head(params.thread_id, true).await?;
        head.deleted_at = Some(Utc::now());
        self.put_existing_head(&head).await
    }

    async fn import_history(
        &self,
        thread_id: ThreadId,
        history: &[RolloutItem],
    ) -> ThreadStoreResult<()> {
        let history_mode = canonical_history_mode_from_rollout_items(history);
        let session_meta = history.iter().find_map(|item| match item {
            RolloutItem::SessionMeta(meta_line) => Some(&meta_line.meta),
            _ => None,
        });
        let now = Utc::now();
        let head = ThreadHeadRecord {
            schema: HEAD_SCHEMA.to_string(),
            namespace: self.config.namespace.clone(),
            thread_id,
            session_id: session_meta
                .map(|meta| meta.session_id)
                .unwrap_or(thread_id.into()),
            extra_config: None,
            config_snapshot: None,
            forked_from_id: session_meta.and_then(|meta| meta.forked_from_id),
            parent_thread_id: session_meta.and_then(|meta| meta.parent_thread_id),
            history_mode,
            head_seq: 0,
            metadata: ThreadMetadataRecord {
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
                memory_mode: ThreadMemoryMode::Enabled,
            },
            archived_at: None,
            deleted_at: None,
            latest_snapshot: None,
        };
        let clients = self.clients().await?;
        clients
            .dynamodb
            .put_item()
            .table_name(self.config.table_name.as_str())
            .set_item(Some(head_item(&self.config, &head)?))
            .condition_expression("attribute_not_exists(pk)")
            .send()
            .await
            .map_err(|err| ThreadStoreError::Conflict {
                message: format!("failed to import thread {thread_id}: {err}"),
            })?;
        let items = persisted_rollout_items(history);
        if !items.is_empty() {
            self.append_canonical_items(thread_id, items, "import-history".to_string(), Some(1))
                .await?;
            self.ensure_snapshot(thread_id).await?;
        }
        Ok(())
    }

    async fn put_commit_payload(
        &self,
        thread_id: ThreadId,
        items: Vec<RolloutItem>,
        start_seq: u64,
        end_seq: u64,
        idempotency_key: &str,
    ) -> ThreadStoreResult<CommitPointerRecord> {
        let commit_id = stable_commit_id(start_seq, end_seq, idempotency_key);
        let sequenced_items = items
            .into_iter()
            .enumerate()
            .map(|(offset, item)| SequencedRolloutItem {
                seq: start_seq + offset as u64,
                item,
            })
            .collect::<Vec<_>>();
        let envelope = CommitPayloadEnvelope {
            schema: COMMIT_PAYLOAD_SCHEMA.to_string(),
            namespace: self.config.namespace.clone(),
            thread_id,
            commit_id: commit_id.clone(),
            start_seq,
            end_seq,
            items: sequenced_items,
        };
        let payload_bytes = json_bytes(&envelope)?;
        let payload_sha256 = sha256_hex(&payload_bytes);
        let key = commit_s3_key(
            &self.config,
            thread_id,
            start_seq,
            end_seq,
            commit_id.as_str(),
        );
        let clients = self.clients().await?;
        let mut put = clients
            .s3
            .put_object()
            .bucket(self.config.bucket_name.as_str())
            .key(key.as_str())
            .body(ByteStream::from(payload_bytes.clone()));
        if let Some(kms_key_id) = self.config.kms_key_id.as_ref() {
            put = put
                .server_side_encryption(aws_sdk_s3::types::ServerSideEncryption::AwsKms)
                .ssekms_key_id(kms_key_id);
        }
        put.send().await.map_err(internal_aws_error(
            "failed to put AWS object-log commit payload",
        ))?;
        Ok(CommitPointerRecord {
            schema: COMMIT_SCHEMA.to_string(),
            namespace: self.config.namespace.clone(),
            thread_id,
            commit_id,
            start_seq,
            end_seq,
            item_count: (end_seq - start_seq + 1) as usize,
            bucket: self.config.bucket_name.clone(),
            key,
            payload_len: payload_bytes.len(),
            payload_sha256,
            compression: self.config.append_payload_compression,
            created_at: Utc::now(),
        })
    }

    async fn read_head(
        &self,
        thread_id: ThreadId,
        include_archived: bool,
    ) -> ThreadStoreResult<ThreadHeadRecord> {
        let started_at = Instant::now();
        let clients = self.clients().await?;
        let output = clients
            .dynamodb
            .get_item()
            .table_name(self.config.table_name.as_str())
            .key("pk", av_s(thread_pk(&self.config, thread_id)))
            .key("sk", av_s(HEAD_SK))
            .consistent_read(self.config.consistent_reads)
            .send()
            .await
            .map_err(internal_aws_error(
                "failed to read AWS object-log thread head",
            ))?;
        let item = output
            .item
            .ok_or(ThreadStoreError::ThreadNotFound { thread_id })?;
        let head = decode_head_item(&item)?;
        if head.deleted_at.is_some() {
            return Err(ThreadStoreError::ThreadNotFound { thread_id });
        }
        if head.archived_at.is_some() && !include_archived {
            return Err(ThreadStoreError::InvalidRequest {
                message: format!("thread {thread_id} is archived"),
            });
        }
        tracing::info!(
            %thread_id,
            include_archived,
            head_seq = head.head_seq,
            has_snapshot = head.latest_snapshot.is_some(),
            elapsed_ms = elapsed_ms(started_at),
            "AWS object-log read_head completed"
        );
        Ok(head)
    }

    async fn read_idempotency_record(
        &self,
        thread_id: ThreadId,
        idempotency_key: &str,
    ) -> ThreadStoreResult<Option<IdempotencyRecord>> {
        let clients = self.clients().await?;
        let output = clients
            .dynamodb
            .get_item()
            .table_name(self.config.table_name.as_str())
            .key("pk", av_s(thread_pk(&self.config, thread_id)))
            .key("sk", av_s(idempotency_sk(idempotency_key)))
            .consistent_read(true)
            .send()
            .await
            .map_err(internal_aws_error(
                "failed to read AWS object-log idempotency record",
            ))?;
        output
            .item
            .map(|item| decode_record(&item, "idempotency"))
            .transpose()
    }

    async fn put_existing_head(&self, head: &ThreadHeadRecord) -> ThreadStoreResult<()> {
        let clients = self.clients().await?;
        clients
            .dynamodb
            .put_item()
            .table_name(self.config.table_name.as_str())
            .set_item(Some(head_item(&self.config, head)?))
            .condition_expression("attribute_exists(pk) AND head_seq = :head_seq")
            .expression_attribute_values(":head_seq", av_n(head.head_seq))
            .send()
            .await
            .map(|_| ())
            .map_err(|err| ThreadStoreError::Conflict {
                message: format!("failed to update thread {} metadata: {err}", head.thread_id),
            })
    }

    async fn stored_thread_from_head(
        &self,
        head: ThreadHeadRecord,
        include_history: bool,
    ) -> ThreadStoreResult<StoredThread> {
        let history = if include_history {
            Some(StoredThreadHistory {
                thread_id: head.thread_id,
                items: self.load_items_for_head(&head).await?,
            })
        } else {
            None
        };
        Ok(stored_thread_from_head_record(head, history))
    }

    async fn load_items_for_head(
        &self,
        head: &ThreadHeadRecord,
    ) -> ThreadStoreResult<Vec<RolloutItem>> {
        let started_at = Instant::now();
        let mut items = Vec::new();
        let mut expected_seq = 1;
        let mut snapshot_item_count = 0;
        let mut snapshot_elapsed_ms = None;
        if let Some(snapshot) = head.latest_snapshot.as_ref() {
            let snapshot_started_at = Instant::now();
            let snapshot_items = self.get_snapshot_payload(head.thread_id, snapshot).await?;
            snapshot_item_count = snapshot_items.len();
            snapshot_elapsed_ms = Some(elapsed_ms(snapshot_started_at));
            expected_seq = snapshot.seq + 1;
            items.extend(snapshot_items);
        }
        let query_started_at = Instant::now();
        let commits = self.query_commit_pointers(head.thread_id).await?;
        let query_elapsed_ms = elapsed_ms(query_started_at);
        let total_commit_count = commits.len();
        let mut skipped_commit_count = 0;
        let mut loaded_commit_count = 0;
        let mut loaded_commit_item_count = 0;
        let mut commit_payload_elapsed_ms = 0;
        for commit in commits {
            if commit.start_seq < expected_seq {
                skipped_commit_count += 1;
                continue;
            }
            if commit.start_seq != expected_seq {
                return Err(ThreadStoreError::Internal {
                    message: format!(
                        "thread {} has commit gap at seq {expected_seq}, found {}",
                        head.thread_id, commit.start_seq
                    ),
                });
            }
            let commit_started_at = Instant::now();
            let payload_items = self.get_commit_payload(&commit).await?;
            loaded_commit_count += 1;
            loaded_commit_item_count += payload_items.len();
            commit_payload_elapsed_ms += elapsed_ms(commit_started_at);
            for item in payload_items {
                if item.seq != expected_seq {
                    return Err(ThreadStoreError::Internal {
                        message: format!(
                            "thread {} expected seq {expected_seq}, found {}",
                            head.thread_id, item.seq
                        ),
                    });
                }
                items.push(item.item);
                expected_seq += 1;
            }
            if commit.end_seq + 1 != expected_seq {
                return Err(ThreadStoreError::Internal {
                    message: format!(
                        "thread {} commit range mismatch ending at {}",
                        head.thread_id, commit.end_seq
                    ),
                });
            }
        }
        if head.head_seq + 1 != expected_seq {
            return Err(ThreadStoreError::Internal {
                message: format!(
                    "thread {} expected history through seq {}, found through {}",
                    head.thread_id,
                    head.head_seq,
                    expected_seq.saturating_sub(1)
                ),
            });
        }
        tracing::info!(
            thread_id = %head.thread_id,
            head_seq = head.head_seq,
            has_snapshot = head.latest_snapshot.is_some(),
            snapshot_item_count,
            snapshot_elapsed_ms,
            total_commit_count,
            skipped_commit_count,
            loaded_commit_count,
            loaded_commit_item_count,
            query_commit_pointers_elapsed_ms = query_elapsed_ms,
            commit_payloads_elapsed_ms = commit_payload_elapsed_ms,
            history_item_count = items.len(),
            elapsed_ms = elapsed_ms(started_at),
            "AWS object-log load_items_for_head completed"
        );
        Ok(items)
    }

    async fn query_commit_pointers(
        &self,
        thread_id: ThreadId,
    ) -> ThreadStoreResult<Vec<CommitPointerRecord>> {
        let started_at = Instant::now();
        let clients = self.clients().await?;
        let output = clients
            .dynamodb
            .query()
            .table_name(self.config.table_name.as_str())
            .key_condition_expression("pk = :pk AND begins_with(sk, :commit_prefix)")
            .expression_attribute_values(":pk", av_s(thread_pk(&self.config, thread_id)))
            .expression_attribute_values(":commit_prefix", av_s("COMMIT#"))
            .consistent_read(self.config.consistent_reads)
            .send()
            .await
            .map_err(internal_aws_error("failed to query AWS object-log commits"))?;
        let mut commits = output
            .items
            .unwrap_or_default()
            .into_iter()
            .map(|item| decode_record(&item, "commit"))
            .collect::<ThreadStoreResult<Vec<CommitPointerRecord>>>()?;
        commits.sort_by_key(|commit| commit.start_seq);
        tracing::info!(
            %thread_id,
            commit_count = commits.len(),
            elapsed_ms = elapsed_ms(started_at),
            "AWS object-log query_commit_pointers completed"
        );
        Ok(commits)
    }

    async fn get_commit_payload(
        &self,
        commit: &CommitPointerRecord,
    ) -> ThreadStoreResult<Vec<SequencedRolloutItem>> {
        let started_at = Instant::now();
        let bytes = self
            .get_s3_object(commit.bucket.as_str(), commit.key.as_str())
            .await?;
        let byte_count = bytes.len();
        verify_sha256(&bytes, commit.payload_sha256.as_str(), commit.key.as_str())?;
        let envelope: CommitPayloadEnvelope =
            serde_json::from_slice(&bytes).map_err(|err| ThreadStoreError::Internal {
                message: format!("failed to decode commit payload {}: {err}", commit.key),
            })?;
        if envelope.schema != COMMIT_PAYLOAD_SCHEMA {
            return Err(ThreadStoreError::Internal {
                message: format!("unsupported commit payload schema {}", envelope.schema),
            });
        }
        if envelope.items.len() != commit.item_count {
            return Err(ThreadStoreError::Internal {
                message: format!("commit payload item count mismatch for {}", commit.key),
            });
        }
        tracing::info!(
            thread_id = %commit.thread_id,
            start_seq = commit.start_seq,
            end_seq = commit.end_seq,
            item_count = envelope.items.len(),
            byte_count,
            elapsed_ms = elapsed_ms(started_at),
            "AWS object-log get_commit_payload completed"
        );
        Ok(envelope.items)
    }

    async fn get_snapshot_payload(
        &self,
        thread_id: ThreadId,
        snapshot: &SnapshotPointerRecord,
    ) -> ThreadStoreResult<Vec<RolloutItem>> {
        let started_at = Instant::now();
        let bytes = self
            .get_s3_object(snapshot.bucket.as_str(), snapshot.key.as_str())
            .await?;
        let byte_count = bytes.len();
        verify_sha256(
            &bytes,
            snapshot.payload_sha256.as_str(),
            snapshot.key.as_str(),
        )?;
        let envelope: SnapshotPayloadEnvelope =
            serde_json::from_slice(&bytes).map_err(|err| ThreadStoreError::Internal {
                message: format!("failed to decode snapshot payload {}: {err}", snapshot.key),
            })?;
        if envelope.schema != SNAPSHOT_PAYLOAD_SCHEMA {
            return Err(ThreadStoreError::Internal {
                message: format!("unsupported snapshot payload schema {}", envelope.schema),
            });
        }
        if envelope.thread_id != thread_id {
            return Err(ThreadStoreError::Internal {
                message: format!(
                    "snapshot payload {} belongs to another thread",
                    snapshot.key
                ),
            });
        }
        let items = envelope
            .items
            .into_iter()
            .map(|item| item.item)
            .collect::<Vec<_>>();
        tracing::info!(
            %thread_id,
            snapshot_seq = snapshot.seq,
            item_count = items.len(),
            byte_count,
            elapsed_ms = elapsed_ms(started_at),
            "AWS object-log get_snapshot_payload completed"
        );
        Ok(items)
    }

    async fn get_s3_object(&self, bucket: &str, key: &str) -> ThreadStoreResult<Vec<u8>> {
        let started_at = Instant::now();
        let clients = self.clients().await?;
        let output = clients
            .s3
            .get_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .map_err(internal_aws_error("failed to get AWS object-log payload"))?;
        let bytes = output.body.collect().await.map_err(internal_aws_error(
            "failed to read AWS object-log payload body",
        ))?;
        let bytes = bytes.into_bytes().to_vec();
        tracing::info!(
            bucket,
            key,
            byte_count = bytes.len(),
            elapsed_ms = elapsed_ms(started_at),
            "AWS object-log get_s3_object completed"
        );
        Ok(bytes)
    }

    async fn ensure_snapshot(&self, thread_id: ThreadId) -> ThreadStoreResult<()> {
        let mut head = self.read_head(thread_id, /*include_archived*/ true).await?;
        if head
            .latest_snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.seq == head.head_seq)
        {
            return Ok(());
        }
        let commits = self.query_commit_pointers(thread_id).await?;
        let mut sequenced_items = Vec::new();
        let mut expected_seq = 1;
        for commit in commits {
            if commit.start_seq != expected_seq {
                return Err(ThreadStoreError::Internal {
                    message: format!(
                        "thread {thread_id} has commit gap while snapshotting at seq {expected_seq}"
                    ),
                });
            }
            let payload_items = self.get_commit_payload(&commit).await?;
            for item in payload_items {
                if item.seq != expected_seq {
                    return Err(ThreadStoreError::Internal {
                        message: format!(
                            "thread {thread_id} has snapshot seq mismatch at {expected_seq}"
                        ),
                    });
                }
                sequenced_items.push(item);
                expected_seq += 1;
            }
        }
        if head.head_seq + 1 != expected_seq {
            return Err(ThreadStoreError::Internal {
                message: format!("cannot snapshot incomplete thread {thread_id}"),
            });
        }
        let envelope = SnapshotPayloadEnvelope {
            schema: SNAPSHOT_PAYLOAD_SCHEMA.to_string(),
            namespace: self.config.namespace.clone(),
            thread_id,
            seq: head.head_seq,
            items: sequenced_items,
        };
        let bytes = json_bytes(&envelope)?;
        let payload_sha256 = sha256_hex(&bytes);
        let key = snapshot_s3_key(
            &self.config,
            thread_id,
            head.head_seq,
            payload_sha256.as_str(),
        );
        let clients = self.clients().await?;
        clients
            .s3
            .put_object()
            .bucket(self.config.bucket_name.as_str())
            .key(key.as_str())
            .body(ByteStream::from(bytes.clone()))
            .send()
            .await
            .map_err(internal_aws_error("failed to put AWS object-log snapshot"))?;
        head.latest_snapshot = Some(SnapshotPointerRecord {
            seq: head.head_seq,
            bucket: self.config.bucket_name.clone(),
            key,
            payload_len: bytes.len(),
            payload_sha256,
            compression: self.config.append_payload_compression,
            created_at: Utc::now(),
        });
        self.put_existing_head(&head).await
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

    fn persist_thread(&self, thread_id: ThreadId) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async move {
            self.read_head(thread_id, /*include_archived*/ true)
                .await
                .map(|_| ())
        })
    }

    fn flush_thread(&self, thread_id: ThreadId) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async move {
            self.read_head(thread_id, /*include_archived*/ true)
                .await
                .map(|_| ())
        })
    }

    fn shutdown_thread(&self, thread_id: ThreadId) -> ThreadStoreFuture<'_, ()> {
        self.flush_thread(thread_id)
    }

    fn discard_thread(&self, _thread_id: ThreadId) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async { Ok(()) })
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

fn head_item(
    config: &AwsObjectLogThreadStoreConfig,
    head: &ThreadHeadRecord,
) -> ThreadStoreResult<HashMap<String, AttributeValue>> {
    let state = if head.deleted_at.is_some() {
        "deleted"
    } else if head.archived_at.is_some() {
        "archived"
    } else {
        "active"
    };
    let mut item = HashMap::from([
        ("pk".to_string(), av_s(thread_pk(config, head.thread_id))),
        ("sk".to_string(), av_s(HEAD_SK)),
        ("record_type".to_string(), av_s("HEAD")),
        (RECORD_JSON_ATTR.to_string(), av_s(json_string(head)?)),
        ("namespace".to_string(), av_s(config.namespace.clone())),
        ("thread_id".to_string(), av_s(head.thread_id.to_string())),
        ("head_seq".to_string(), av_n(head.head_seq)),
        (
            "updated_at".to_string(),
            av_s(head.metadata.updated_at.to_rfc3339()),
        ),
        ("gsi1pk".to_string(), av_s(index_pk(config, state))),
        (
            "gsi1sk".to_string(),
            av_s(format!(
                "UPDATED#{:020}#THREAD#{}",
                timestamp_millis(head.metadata.updated_at),
                head.thread_id
            )),
        ),
        ("gsi2pk".to_string(), av_s(index_pk(config, state))),
        (
            "gsi2sk".to_string(),
            av_s(format!(
                "CREATED#{:020}#THREAD#{}",
                timestamp_millis(head.metadata.created_at),
                head.thread_id
            )),
        ),
    ]);
    if head.deleted_at.is_some() {
        item.insert("deleted_at".to_string(), av_s("true"));
    }
    Ok(item)
}

fn commit_item(
    config: &AwsObjectLogThreadStoreConfig,
    commit: &CommitPointerRecord,
) -> ThreadStoreResult<HashMap<String, AttributeValue>> {
    Ok(HashMap::from([
        ("pk".to_string(), av_s(thread_pk(config, commit.thread_id))),
        ("sk".to_string(), av_s(commit_sk(commit.start_seq))),
        ("record_type".to_string(), av_s("COMMIT")),
        (RECORD_JSON_ATTR.to_string(), av_s(json_string(commit)?)),
        ("namespace".to_string(), av_s(config.namespace.clone())),
        ("thread_id".to_string(), av_s(commit.thread_id.to_string())),
        ("start_seq".to_string(), av_n(commit.start_seq)),
        ("end_seq".to_string(), av_n(commit.end_seq)),
    ]))
}

fn idempotency_item(
    config: &AwsObjectLogThreadStoreConfig,
    record: &IdempotencyRecord,
) -> ThreadStoreResult<HashMap<String, AttributeValue>> {
    Ok(HashMap::from([
        ("pk".to_string(), av_s(thread_pk(config, record.thread_id))),
        (
            "sk".to_string(),
            av_s(idempotency_sk(record.idempotency_key.as_str())),
        ),
        ("record_type".to_string(), av_s("IDEMPOTENCY")),
        (RECORD_JSON_ATTR.to_string(), av_s(json_string(record)?)),
        (
            "request_sha256".to_string(),
            av_s(record.request_sha256.clone()),
        ),
    ]))
}

fn put_item_tx(
    table_name: &str,
    item: HashMap<String, AttributeValue>,
    condition_expression: &str,
    expression_attribute_values: HashMap<String, AttributeValue>,
) -> ThreadStoreResult<TransactWriteItem> {
    let mut put = Put::builder()
        .table_name(table_name)
        .set_item(Some(item))
        .condition_expression(condition_expression);
    if !expression_attribute_values.is_empty() {
        put = put.set_expression_attribute_values(Some(expression_attribute_values));
    }
    let put = put.build().map_err(|err| ThreadStoreError::Internal {
        message: format!("failed to build DynamoDB put transaction: {err}"),
    })?;
    Ok(TransactWriteItem::builder().put(put).build())
}

fn decode_head_item(item: &HashMap<String, AttributeValue>) -> ThreadStoreResult<ThreadHeadRecord> {
    let head: ThreadHeadRecord = decode_record(item, "head")?;
    if head.schema != HEAD_SCHEMA {
        return Err(ThreadStoreError::Internal {
            message: format!("unsupported head schema {}", head.schema),
        });
    }
    Ok(head)
}

fn decode_record<T: serde::de::DeserializeOwned>(
    item: &HashMap<String, AttributeValue>,
    record_name: &str,
) -> ThreadStoreResult<T> {
    let json = item
        .get(RECORD_JSON_ATTR)
        .and_then(|value| match value {
            AttributeValue::S(value) => Some(value.as_str()),
            _ => None,
        })
        .ok_or_else(|| ThreadStoreError::Internal {
            message: format!("missing {record_name} record JSON"),
        })?;
    serde_json::from_str(json).map_err(|err| ThreadStoreError::Internal {
        message: format!("failed to decode {record_name} record JSON: {err}"),
    })
}

fn stored_thread_from_head_record(
    head: ThreadHeadRecord,
    history: Option<StoredThreadHistory>,
) -> StoredThread {
    StoredThread {
        thread_id: head.thread_id,
        extra_config: head.extra_config,
        config_snapshot: head.config_snapshot,
        rollout_path: None,
        forked_from_id: head.forked_from_id,
        parent_thread_id: head.parent_thread_id,
        preview: head.metadata.preview,
        name: head.metadata.name,
        model_provider: head.metadata.model_provider,
        model: head.metadata.model,
        reasoning_effort: head.metadata.reasoning_effort,
        created_at: head.metadata.created_at,
        updated_at: head.metadata.updated_at,
        recency_at: head.metadata.recency_at,
        archived_at: head.archived_at,
        cwd: head.metadata.cwd,
        cli_version: head.metadata.cli_version,
        source: head.metadata.source,
        history_mode: head.history_mode,
        thread_source: head.metadata.thread_source,
        agent_nickname: head.metadata.agent_nickname,
        agent_role: head.metadata.agent_role,
        agent_path: head.metadata.agent_path,
        git_info: head.metadata.git_info,
        approval_mode: head.metadata.approval_mode,
        permission_profile: head.metadata.permission_profile,
        token_usage: head.metadata.token_usage,
        first_user_message: head.metadata.first_user_message,
        history,
    }
}

fn apply_metadata_patch(metadata: &mut ThreadMetadataRecord, patch: ThreadMetadataPatch) {
    if let Some(name) = patch.name {
        metadata.name = name;
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
    if let Some(memory_mode) = patch.memory_mode {
        metadata.memory_mode = memory_mode;
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

fn append_result(
    first_seq: u64,
    last_seq: u64,
    commit_id: &str,
    idempotent_replay: bool,
) -> AwsObjectLogAppendResult {
    AwsObjectLogAppendResult {
        first_seq,
        last_seq,
        committed_item_count: (last_seq - first_seq + 1) as usize,
        commit_id: commit_id.to_string(),
        idempotent_replay,
    }
}

fn thread_pk(config: &AwsObjectLogThreadStoreConfig, thread_id: ThreadId) -> String {
    format!("NS#{}#THREAD#{thread_id}", config.namespace)
}

fn index_pk(config: &AwsObjectLogThreadStoreConfig, state: &str) -> String {
    format!("NS#{}#STATE#{state}", config.namespace)
}

fn commit_sk(start_seq: u64) -> String {
    format!("COMMIT#{start_seq:020}")
}

fn idempotency_sk(idempotency_key: &str) -> String {
    format!("IDEMP#APPEND#{idempotency_key}")
}

fn stable_commit_id(start_seq: u64, end_seq: u64, idempotency_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(idempotency_key.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("{start_seq:020}-{end_seq:020}-{}", &digest[..16])
}

fn commit_s3_key(
    config: &AwsObjectLogThreadStoreConfig,
    thread_id: ThreadId,
    start_seq: u64,
    end_seq: u64,
    commit_id: &str,
) -> String {
    format!(
        "{}/namespaces/{}/threads/{thread_id}/commits/{start_seq:020}-{end_seq:020}-{commit_id}.json",
        config.key_prefix, config.namespace
    )
}

fn snapshot_s3_key(
    config: &AwsObjectLogThreadStoreConfig,
    thread_id: ThreadId,
    seq: u64,
    payload_sha256: &str,
) -> String {
    format!(
        "{}/namespaces/{}/threads/{thread_id}/snapshots/{seq:020}-{}.json",
        config.key_prefix,
        config.namespace,
        &payload_sha256[..16]
    )
}

fn av_s(value: impl Into<String>) -> AttributeValue {
    AttributeValue::S(value.into())
}

fn av_n(value: u64) -> AttributeValue {
    AttributeValue::N(value.to_string())
}

fn json_string<T: serde::Serialize>(value: &T) -> ThreadStoreResult<String> {
    serde_json::to_string(value).map_err(|err| ThreadStoreError::Internal {
        message: format!("failed to serialize AWS object-log record: {err}"),
    })
}

fn json_bytes<T: serde::Serialize>(value: &T) -> ThreadStoreResult<Vec<u8>> {
    serde_json::to_vec(value).map_err(|err| ThreadStoreError::Internal {
        message: format!("failed to serialize AWS object-log payload: {err}"),
    })
}

fn sha256_json<T: serde::Serialize>(value: &T) -> ThreadStoreResult<String> {
    json_bytes(value).map(|bytes| sha256_hex(&bytes))
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn verify_sha256(bytes: &[u8], expected: &str, label: &str) -> ThreadStoreResult<()> {
    let actual = sha256_hex(bytes);
    if actual != expected {
        return Err(ThreadStoreError::Internal {
            message: format!(
                "payload hash mismatch for {label}: expected {expected}, got {actual}"
            ),
        });
    }
    Ok(())
}

fn timestamp_millis(timestamp: DateTime<Utc>) -> i64 {
    timestamp.timestamp_millis()
}

fn elapsed_ms(started_at: Instant) -> u64 {
    let elapsed_ms = started_at.elapsed().as_millis();
    elapsed_ms.min(u128::from(u64::MAX)) as u64
}

fn internal_aws_error<E: Debug>(context: &'static str) -> impl FnOnce(E) -> ThreadStoreError {
    move |err| ThreadStoreError::Internal {
        message: format!("{context}: {}", sanitized_aws_error(err)),
    }
}

fn sanitized_aws_error<E: Debug>(err: E) -> String {
    let mut message = format!("{err:?}");
    redact_xml_element(&mut message, "Token-0");
    if message.len() > 2048 {
        message.truncate(2048);
        message.push_str("...");
    }
    message
}

fn redact_xml_element(message: &mut String, element: &str) {
    let open = format!("<{element}>");
    let close = format!("</{element}>");
    while let Some(start) = message.find(open.as_str()) {
        let value_start = start + open.len();
        let Some(relative_end) = message[value_start..].find(close.as_str()) else {
            return;
        };
        let value_end = value_start + relative_end;
        message.replace_range(value_start..value_end, "[REDACTED]");
    }
}
