/// Payload compression used for AWS object-log payload objects.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AwsObjectLogCompression {
    /// Store payload JSON without compression.
    #[default]
    None,
}

/// Configuration for the durable AWS object-log DynamoDB + S3 thread store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AwsObjectLogThreadStoreConfig {
    pub table_name: String,
    pub bucket_name: String,
    pub namespace: String,
    pub key_prefix: String,
    pub aws_region: Option<String>,
    pub endpoint_url: Option<String>,
    pub kms_key_id: Option<String>,
    pub consistent_reads: bool,
    pub append_payload_compression: AwsObjectLogCompression,
    pub gsi_updated_index_name: String,
    pub gsi_created_index_name: String,
}

impl AwsObjectLogThreadStoreConfig {
    pub fn new(
        table_name: String,
        bucket_name: String,
        namespace: String,
        key_prefix: Option<String>,
    ) -> Self {
        Self {
            table_name,
            bucket_name,
            key_prefix: key_prefix.unwrap_or_else(|| "codex-thread-store".to_string()),
            namespace,
            aws_region: None,
            endpoint_url: None,
            kms_key_id: None,
            consistent_reads: true,
            append_payload_compression: AwsObjectLogCompression::None,
            gsi_updated_index_name: "gsi1".to_string(),
            gsi_created_index_name: "gsi2".to_string(),
        }
    }
}
