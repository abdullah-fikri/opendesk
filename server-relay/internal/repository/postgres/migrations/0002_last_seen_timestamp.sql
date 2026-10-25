-- last_seen used to hold a display string ("Online" or "15:04 • 02/01"). Store a timestamp instead and let
-- clients format it; updated_at is the closest existing value for old rows.
ALTER TABLE hosts_registry ALTER COLUMN last_seen TYPE TIMESTAMPTZ USING updated_at;
ALTER TABLE hosts_registry ALTER COLUMN last_seen SET DEFAULT NOW();
