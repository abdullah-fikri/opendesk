-- Names administrators give devices, and the device allowlist with per-device enrollment tokens.
ALTER TABLE hosts_registry ADD COLUMN IF NOT EXISTS alias VARCHAR(255) NOT NULL DEFAULT '';

CREATE TABLE IF NOT EXISTS relay_settings (
    key        VARCHAR(64) PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- token_hash is the hex SHA-256 of the token issued at enrollment; empty until the device first connects.
CREATE TABLE IF NOT EXISTS allowed_devices (
    clean_id    VARCHAR(32) PRIMARY KEY,
    is_admin    BOOLEAN NOT NULL DEFAULT FALSE,
    token_hash  VARCHAR(64) NOT NULL DEFAULT '',
    added_by    VARCHAR(32) NOT NULL DEFAULT '',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    enrolled_at TIMESTAMPTZ
);
