use std::error::Error;
use std::path::PathBuf;
use std::time::Duration;

use aws_config::BehaviorVersion;
use aws_config::Region;
use aws_sdk_dynamodb::Client as DynamoDbClient;
use aws_sdk_dynamodb::types::AttributeDefinition;
use aws_sdk_dynamodb::types::BillingMode;
use aws_sdk_dynamodb::types::GlobalSecondaryIndex;
use aws_sdk_dynamodb::types::KeySchemaElement;
use aws_sdk_dynamodb::types::KeyType;
use aws_sdk_dynamodb::types::Projection;
use aws_sdk_dynamodb::types::ProjectionType;
use aws_sdk_dynamodb::types::ScalarAttributeType;
use aws_sdk_dynamodb::types::TableStatus;
use aws_sdk_s3::Client as S3Client;
use codex_protocol::ThreadId;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Settings;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::PermissionProfile;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SessionContextWindow;
use codex_protocol::protocol::SessionMeta;
use codex_protocol::protocol::SessionMetaLine;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_protocol::protocol::ThreadMemoryMode;
use codex_thread_store::AppendThreadItemsParams;
use codex_thread_store::ArchiveThreadParams;
use codex_thread_store::AwsObjectLogAppendOptions;
use codex_thread_store::AwsObjectLogThreadStore;
use codex_thread_store::AwsObjectLogThreadStoreConfig;
use codex_thread_store::CreateThreadParams;
use codex_thread_store::ListThreadsParams;
use codex_thread_store::LoadThreadHistoryParams;
use codex_thread_store::ReadThreadParams;
use codex_thread_store::SortDirection;
use codex_thread_store::StoredThreadConfigSnapshot;
use codex_thread_store::StoredThreadConfigSnapshotVersion;
use codex_thread_store::ThreadPersistenceMetadata;
use codex_thread_store::ThreadSortKey;
use codex_thread_store::ThreadStore;
use codex_thread_store::ThreadStoreError;
use pretty_assertions::assert_eq;
use testcontainers::ContainerAsync;
use testcontainers::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::localstack::LocalStack;
use tokio::time::sleep;
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const REGION: &str = "us-east-1";
const EDGE_PORT: u16 = 4566;
const GSI_UPDATED: &str = "gsi1";
const GSI_CREATED: &str = "gsi2";

struct LocalStackThreadStore {
    _container: ContainerAsync<LocalStack>,
    dynamodb: DynamoDbClient,
    s3: S3Client,
    config: AwsObjectLogThreadStoreConfig,
}

impl LocalStackThreadStore {
    async fn start() -> TestResult<Self> {
        let container = LocalStack::default()
            .with_env_var("SERVICES", "dynamodb,s3")
            .start()
            .await?;
        let host = container.get_host().await?;
        let port = container.get_host_port_ipv4(EDGE_PORT).await?;
        let endpoint_url = format!("http://{host}:{port}");
        let dynamodb = dynamodb_client(endpoint_url.as_str());
        let s3 = s3_client(endpoint_url.as_str());

        let suffix = Uuid::new_v4().to_string();
        let table_name = format!("codex-thread-store-{suffix}");
        let bucket_name = format!("codex-thread-store-{suffix}");
        create_table(&dynamodb, table_name.as_str()).await?;
        create_bucket(&s3, bucket_name.as_str()).await?;

        let config = AwsObjectLogThreadStoreConfig::new(
            table_name,
            bucket_name,
            "test-namespace".to_string(),
            Some("test-prefix".to_string()),
        );
        Ok(Self {
            _container: container,
            dynamodb,
            s3,
            config,
        })
    }

    fn store(&self) -> AwsObjectLogThreadStore {
        AwsObjectLogThreadStore::from_clients(
            self.config.clone(),
            self.dynamodb.clone(),
            self.s3.clone(),
        )
    }
}

#[tokio::test]
async fn aws_object_log_persists_history_and_config_in_localstack() -> TestResult {
    let localstack = match LocalStackThreadStore::start().await {
        Ok(localstack) => localstack,
        Err(err) if is_docker_unavailable(err.as_ref()) => {
            eprintln!("skipping LocalStack thread-store test because Docker is unavailable: {err}");
            return Ok(());
        }
        Err(err) => return Err(err),
    };
    let store = localstack.store();
    let replacement_store = localstack.store();
    let thread_id = ThreadId::new();
    let snapshot = stored_config_snapshot();

    let mut create_params = create_thread_params(thread_id);
    create_params.config_snapshot = Some(snapshot.clone());
    store.create_thread(create_params).await?;

    let append_item = session_meta_item(thread_id, "append-window");
    let first_append = store
        .append_items_with_options(
            AppendThreadItemsParams {
                thread_id,
                idempotency_key: Some("append-1".to_string()),
                expected_next_seq: Some(2),
                items: vec![append_item.clone()],
            },
            AwsObjectLogAppendOptions::default(),
        )
        .await?;
    assert_eq!(
        first_append,
        codex_thread_store::AwsObjectLogAppendResult {
            first_seq: 2,
            last_seq: 2,
            committed_item_count: 1,
            commit_id: first_append.commit_id.clone(),
            idempotent_replay: false,
        }
    );

    let replay = replacement_store
        .append_items_with_options(
            AppendThreadItemsParams {
                thread_id,
                idempotency_key: Some("append-1".to_string()),
                expected_next_seq: Some(2),
                items: vec![append_item],
            },
            AwsObjectLogAppendOptions::default(),
        )
        .await?;
    assert_eq!(
        replay,
        codex_thread_store::AwsObjectLogAppendResult {
            idempotent_replay: true,
            ..first_append
        }
    );

    let stored_thread = replacement_store
        .read_thread(ReadThreadParams {
            thread_id,
            include_archived: false,
            include_history: true,
        })
        .await?;
    assert_eq!(stored_thread.config_snapshot, Some(snapshot));
    assert_eq!(
        stored_thread
            .history
            .as_ref()
            .expect("history should be loaded")
            .items
            .len(),
        2
    );

    let history = replacement_store
        .load_history(LoadThreadHistoryParams {
            thread_id,
            include_archived: false,
        })
        .await?;
    assert_eq!(history.items.len(), 2);

    assert!(matches!(
        replacement_store
            .append_items_with_options(
                AppendThreadItemsParams {
                    thread_id,
                    idempotency_key: Some("append-1".to_string()),
                    expected_next_seq: Some(2),
                    items: vec![session_meta_item(thread_id, "different-window")],
                },
                AwsObjectLogAppendOptions::default(),
            )
            .await
            .expect_err("reusing an idempotency key with a different payload should conflict"),
        ThreadStoreError::Conflict { .. }
    ));

    assert!(matches!(
        replacement_store
            .append_items_with_options(
                AppendThreadItemsParams {
                    thread_id,
                    idempotency_key: Some("append-2".to_string()),
                    expected_next_seq: Some(4),
                    items: vec![session_meta_item(thread_id, "stale-window")],
                },
                AwsObjectLogAppendOptions::default(),
            )
            .await
            .expect_err("stale expected sequence should conflict"),
        ThreadStoreError::Conflict { .. }
    ));

    assert_list_contains(&replacement_store, thread_id, /*archived*/ false).await?;
    replacement_store
        .archive_thread(ArchiveThreadParams { thread_id })
        .await?;
    assert_list_contains(&replacement_store, thread_id, /*archived*/ true).await
}

fn is_docker_unavailable(err: &(dyn Error + Send + Sync)) -> bool {
    let message = err.to_string();
    message.contains("SocketNotFoundError")
        || message.contains("No such file or directory")
        || message.contains("Cannot connect to the Docker daemon")
}

async fn assert_list_contains(
    store: &AwsObjectLogThreadStore,
    thread_id: ThreadId,
    archived: bool,
) -> TestResult {
    for _ in 0..20 {
        let page = store
            .list_threads(ListThreadsParams {
                page_size: 10,
                cursor: None,
                sort_key: ThreadSortKey::UpdatedAt,
                sort_direction: SortDirection::Desc,
                allowed_sources: Vec::new(),
                model_providers: None,
                cwd_filters: None,
                archived,
                search_term: None,
                relation_filter: None,
                use_state_db_only: true,
            })
            .await?;
        if page
            .items
            .iter()
            .any(|thread| thread.thread_id == thread_id)
        {
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }
    panic!("thread {thread_id} was not visible in archived={archived} list");
}

fn dynamodb_client(endpoint_url: &str) -> DynamoDbClient {
    let credentials =
        aws_sdk_dynamodb::config::Credentials::new("test", "test", None, None, "localstack");
    let config = aws_sdk_dynamodb::config::Builder::default()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new(REGION))
        .credentials_provider(credentials)
        .endpoint_url(endpoint_url)
        .build();
    DynamoDbClient::from_conf(config)
}

fn s3_client(endpoint_url: &str) -> S3Client {
    let credentials =
        aws_sdk_s3::config::Credentials::new("test", "test", None, None, "localstack");
    let config = aws_sdk_s3::config::Builder::default()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new(REGION))
        .credentials_provider(credentials)
        .endpoint_url(endpoint_url)
        .force_path_style(true)
        .build();
    S3Client::from_conf(config)
}

async fn create_table(client: &DynamoDbClient, table_name: &str) -> TestResult {
    client
        .create_table()
        .table_name(table_name)
        .billing_mode(BillingMode::PayPerRequest)
        .attribute_definitions(string_attr("pk"))
        .attribute_definitions(string_attr("sk"))
        .attribute_definitions(string_attr("gsi1pk"))
        .attribute_definitions(string_attr("gsi1sk"))
        .attribute_definitions(string_attr("gsi2pk"))
        .attribute_definitions(string_attr("gsi2sk"))
        .key_schema(key_schema("pk", KeyType::Hash))
        .key_schema(key_schema("sk", KeyType::Range))
        .global_secondary_indexes(gsi(GSI_UPDATED, "gsi1pk", "gsi1sk"))
        .global_secondary_indexes(gsi(GSI_CREATED, "gsi2pk", "gsi2sk"))
        .send()
        .await?;
    wait_for_table(client, table_name).await
}

async fn wait_for_table(client: &DynamoDbClient, table_name: &str) -> TestResult {
    for _ in 0..50 {
        let output = client
            .describe_table()
            .table_name(table_name)
            .send()
            .await?;
        if output
            .table()
            .and_then(|table| table.table_status())
            .is_some_and(|status| matches!(status, TableStatus::Active))
        {
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }
    panic!("DynamoDB table {table_name} did not become active");
}

async fn create_bucket(client: &S3Client, bucket_name: &str) -> TestResult {
    client.create_bucket().bucket(bucket_name).send().await?;
    Ok(())
}

fn string_attr(name: &str) -> AttributeDefinition {
    AttributeDefinition::builder()
        .attribute_name(name)
        .attribute_type(ScalarAttributeType::S)
        .build()
        .expect("valid attribute definition")
}

fn key_schema(name: &str, key_type: KeyType) -> KeySchemaElement {
    KeySchemaElement::builder()
        .attribute_name(name)
        .key_type(key_type)
        .build()
        .expect("valid key schema")
}

fn gsi(index_name: &str, hash_key: &str, range_key: &str) -> GlobalSecondaryIndex {
    GlobalSecondaryIndex::builder()
        .index_name(index_name)
        .key_schema(key_schema(hash_key, KeyType::Hash))
        .key_schema(key_schema(range_key, KeyType::Range))
        .projection(
            Projection::builder()
                .projection_type(ProjectionType::All)
                .build(),
        )
        .build()
        .expect("valid global secondary index")
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

fn session_meta_item(thread_id: ThreadId, window_id: &str) -> RolloutItem {
    RolloutItem::SessionMeta(SessionMetaLine {
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

fn stored_config_snapshot() -> StoredThreadConfigSnapshot {
    let cwd = PathBuf::from("/workspace/project");
    StoredThreadConfigSnapshot {
        version: StoredThreadConfigSnapshotVersion::V1,
        model: "snapshot-model".to_string(),
        model_provider_id: "snapshot-provider".to_string(),
        service_tier: Some("priority".to_string()),
        approval_policy: AskForApproval::OnRequest,
        approvals_reviewer: ApprovalsReviewer::User,
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
