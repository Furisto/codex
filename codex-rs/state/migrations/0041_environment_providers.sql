CREATE TABLE environment_providers (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    normalized_name TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    url TEXT NOT NULL,
    auth_kind TEXT NOT NULL,
    auth_ciphertext_version INTEGER NOT NULL
        CHECK(auth_ciphertext_version >= 0 AND auth_ciphertext_version <= 4294967295),
    auth_ciphertext BLOB NOT NULL
);

CREATE INDEX idx_environment_providers_name_id
    ON environment_providers(normalized_name, id);
