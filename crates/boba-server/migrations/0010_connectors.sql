-- Connectors: MCP servers agents can use. Secret values are AES-256-GCM encrypted
-- (key: BOBA_SECRET_KEY or <data>/secret.key) and never leave the server.
CREATE TABLE connectors (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    kind          TEXT NOT NULL,
    transport     TEXT NOT NULL,           -- JSON McpTransport
    allowed_tools TEXT NOT NULL DEFAULT '[]', -- JSON string[]
    created_by    TEXT,
    created_at    TEXT NOT NULL
);

CREATE TABLE connector_secrets (
    connector_id TEXT NOT NULL REFERENCES connectors(id) ON DELETE CASCADE,
    key          TEXT NOT NULL,
    value_enc    TEXT NOT NULL,            -- base64(nonce || ciphertext)
    PRIMARY KEY (connector_id, key)
);
