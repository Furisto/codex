use super::StateRuntime;
use sqlx::Row;
use uuid::Uuid;

/// Encrypted authentication columns stored with an environment provider definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentProviderAuthenticationRecord {
    pub kind: String,
    pub ciphertext_version: i64,
    pub ciphertext: Vec<u8>,
}

/// A dynamic environment provider row in the state database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentProviderRecord {
    pub id: String,
    pub name: String,
    pub normalized_name: String,
    pub kind: String,
    pub url: String,
    pub authentication: EnvironmentProviderAuthenticationRecord,
}

/// Values needed to insert a dynamic environment provider row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateEnvironmentProviderRecord {
    pub name: String,
    pub normalized_name: String,
    pub kind: String,
    pub url: String,
    pub authentication: EnvironmentProviderAuthenticationRecord,
}

/// Display and normalized forms of an environment provider name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentProviderNameRecord {
    pub name: String,
    pub normalized_name: String,
}

/// Mutable columns of a dynamic environment provider row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvironmentProviderUpdate {
    pub name: Option<EnvironmentProviderNameRecord>,
    pub authentication: Option<EnvironmentProviderAuthenticationRecord>,
}

/// Keyset cursor used to list dynamic environment provider rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentProviderListCursor {
    pub normalized_name: String,
    pub id: String,
}

/// Result of an atomic dynamic environment provider update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateEnvironmentProviderRecordOutcome {
    Updated(EnvironmentProviderRecord),
    NotFound,
    NameConflict,
}

impl StateRuntime {
    /// Inserts a dynamic environment provider and returns its generated opaque ID.
    ///
    /// Returns `None` when the normalized name is already present.
    pub async fn create_environment_provider(
        &self,
        record: CreateEnvironmentProviderRecord,
    ) -> anyhow::Result<Option<EnvironmentProviderRecord>> {
        let record = EnvironmentProviderRecord {
            id: Uuid::new_v4().to_string(),
            name: record.name,
            normalized_name: record.normalized_name,
            kind: record.kind,
            url: record.url,
            authentication: record.authentication,
        };
        let result = sqlx::query(
            r#"
INSERT INTO environment_providers (
    id,
    name,
    normalized_name,
    kind,
    url,
    auth_kind,
    auth_ciphertext_version,
    auth_ciphertext
) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(normalized_name) DO NOTHING
            "#,
        )
        .bind(&record.id)
        .bind(&record.name)
        .bind(&record.normalized_name)
        .bind(&record.kind)
        .bind(&record.url)
        .bind(&record.authentication.kind)
        .bind(record.authentication.ciphertext_version)
        .bind(&record.authentication.ciphertext)
        .execute(self.pool.as_ref())
        .await?;

        Ok((result.rows_affected() == 1).then_some(record))
    }

    /// Reads a dynamic environment provider by ID.
    pub async fn read_environment_provider(
        &self,
        provider_id: &str,
    ) -> anyhow::Result<Option<EnvironmentProviderRecord>> {
        sqlx::query(
            r#"
SELECT id, name, normalized_name, kind, url, auth_kind, auth_ciphertext_version,
    auth_ciphertext
FROM environment_providers
WHERE id = ?
            "#,
        )
        .bind(provider_id)
        .fetch_optional(self.pool.as_ref())
        .await?
        .map(environment_provider_from_row)
        .transpose()
    }

    /// Lists dynamic environment providers after an optional keyset cursor.
    pub async fn list_environment_providers(
        &self,
        cursor: Option<&EnvironmentProviderListCursor>,
        limit: i64,
    ) -> anyhow::Result<Vec<EnvironmentProviderRecord>> {
        let rows = match cursor {
            Some(cursor) => {
                sqlx::query(
                    r#"
SELECT id, name, normalized_name, kind, url, auth_kind, auth_ciphertext_version,
    auth_ciphertext
FROM environment_providers
WHERE normalized_name > ? OR (normalized_name = ? AND id > ?)
ORDER BY normalized_name ASC, id ASC
LIMIT ?
                    "#,
                )
                .bind(&cursor.normalized_name)
                .bind(&cursor.normalized_name)
                .bind(&cursor.id)
                .bind(limit)
                .fetch_all(self.pool.as_ref())
                .await?
            }
            None => {
                sqlx::query(
                    r#"
SELECT id, name, normalized_name, kind, url, auth_kind, auth_ciphertext_version,
    auth_ciphertext
FROM environment_providers
ORDER BY normalized_name ASC, id ASC
LIMIT ?
                    "#,
                )
                .bind(limit)
                .fetch_all(self.pool.as_ref())
                .await?
            }
        };

        rows.into_iter()
            .map(environment_provider_from_row)
            .collect()
    }

    /// Atomically updates mutable environment provider columns.
    pub async fn update_environment_provider(
        &self,
        provider_id: &str,
        update: EnvironmentProviderUpdate,
    ) -> anyhow::Result<UpdateEnvironmentProviderRecordOutcome> {
        let mut tx = self.pool.begin().await?;
        let Some(row) = sqlx::query(
            r#"
SELECT id, name, normalized_name, kind, url, auth_kind, auth_ciphertext_version,
    auth_ciphertext
FROM environment_providers
WHERE id = ?
            "#,
        )
        .bind(provider_id)
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(UpdateEnvironmentProviderRecordOutcome::NotFound);
        };
        let mut record = environment_provider_from_row(row)?;

        if let Some(name) = update.name {
            record.name = name.name;
            record.normalized_name = name.normalized_name;
        }
        if let Some(authentication) = update.authentication {
            record.authentication = authentication;
        }

        let result = sqlx::query(
            r#"
UPDATE OR IGNORE environment_providers
SET name = ?,
    normalized_name = ?,
    auth_kind = ?,
    auth_ciphertext_version = ?,
    auth_ciphertext = ?
WHERE id = ?
            "#,
        )
        .bind(&record.name)
        .bind(&record.normalized_name)
        .bind(&record.authentication.kind)
        .bind(record.authentication.ciphertext_version)
        .bind(&record.authentication.ciphertext)
        .bind(provider_id)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(UpdateEnvironmentProviderRecordOutcome::NameConflict);
        }
        tx.commit().await?;

        Ok(UpdateEnvironmentProviderRecordOutcome::Updated(record))
    }

    /// Deletes a dynamic environment provider by ID and reports whether it existed.
    pub async fn delete_environment_provider(&self, provider_id: &str) -> anyhow::Result<bool> {
        let result = sqlx::query("DELETE FROM environment_providers WHERE id = ?")
            .bind(provider_id)
            .execute(self.pool.as_ref())
            .await?;
        Ok(result.rows_affected() == 1)
    }
}

fn environment_provider_from_row(
    row: sqlx::sqlite::SqliteRow,
) -> anyhow::Result<EnvironmentProviderRecord> {
    Ok(EnvironmentProviderRecord {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        normalized_name: row.try_get("normalized_name")?,
        kind: row.try_get("kind")?,
        url: row.try_get("url")?,
        authentication: EnvironmentProviderAuthenticationRecord {
            kind: row.try_get("auth_kind")?,
            ciphertext_version: row.try_get("auth_ciphertext_version")?,
            ciphertext: row.try_get("auth_ciphertext")?,
        },
    })
}
