package database

import (
	"database/sql"
	"fmt"
	"time"

	"opendesk/server-relay/internal/config"
	"opendesk/server-relay/internal/logger"

	_ "github.com/lib/pq"
)

type HostRecord struct {
	CleanID    string `json:"clean_id"`
	DeskID     string `json:"desk_id"`
	DeviceName string `json:"device_name"`
	Platform   string `json:"platform"`
	Online     bool   `json:"online"`
	LastSeen   string `json:"last_seen"`
}

type Store struct {
	DB *sql.DB
}

func Connect(cfg config.Config) (*Store, error) {
	hostsToTry := []string{cfg.DBHost}
	if cfg.DBHost == "localhost" || cfg.DBHost == "127.0.0.1" {
		hostsToTry = append(hostsToTry, "host.docker.internal")
	}

	var db *sql.DB
	var lastErr error
	var connectedHost string

	for _, h := range hostsToTry {
		dsn := fmt.Sprintf(
			"host=%s port=%d user=%s password=%s dbname=%s sslmode=disable connect_timeout=5",
			h, cfg.DBPort, cfg.DBUser, cfg.DBPassword, cfg.DBName,
		)
		conn, err := sql.Open("postgres", dsn)
		if err != nil {
			lastErr = err
			continue
		}
		conn.SetMaxOpenConns(25)
		conn.SetMaxIdleConns(10)
		conn.SetConnMaxLifetime(30 * time.Minute)

		if err := conn.Ping(); err != nil {
			_ = conn.Close()
			lastErr = err
			continue
		}

		db = conn
		connectedHost = h
		break
	}

	if db == nil {
		return nil, fmt.Errorf("failed to connect to postgres (%s:%d/%s): %w", cfg.DBHost, cfg.DBPort, cfg.DBName, lastErr)
	}

	logger.Info("db.postgres", "Connected to PostgreSQL at %s:%d/%s (user=%s)", connectedHost, cfg.DBPort, cfg.DBName, cfg.DBUser)

	store := &Store{DB: db}
	if err := store.initSchema(); err != nil {
		_ = db.Close()
		return nil, err
	}

	return store, nil
}

func (s *Store) initSchema() error {
	schema := `
	CREATE TABLE IF NOT EXISTS hosts_registry (
		clean_id    VARCHAR(32) PRIMARY KEY,
		desk_id     VARCHAR(32) NOT NULL,
		device_name VARCHAR(255) NOT NULL,
		platform    VARCHAR(64) NOT NULL,
		is_admin    BOOLEAN NOT NULL DEFAULT FALSE,
		online      BOOLEAN NOT NULL DEFAULT FALSE,
		remote_addr VARCHAR(128) NOT NULL DEFAULT '',
		last_seen   VARCHAR(64) NOT NULL,
		updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
	);

	CREATE INDEX IF NOT EXISTS idx_hosts_online ON hosts_registry(online DESC, device_name ASC);

	CREATE TABLE IF NOT EXISTS remote_sessions (
		id              BIGSERIAL PRIMARY KEY,
		viewer_clean_id VARCHAR(32) NOT NULL,
		viewer_desk_id  VARCHAR(32) NOT NULL,
		host_clean_id   VARCHAR(32) NOT NULL,
		host_desk_id    VARCHAR(32) NOT NULL,
		status          VARCHAR(32) NOT NULL DEFAULT 'active',
		started_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
		ended_at        TIMESTAMPTZ
	);

	CREATE INDEX IF NOT EXISTS idx_sessions_active_viewer ON remote_sessions(viewer_clean_id, status);
	CREATE INDEX IF NOT EXISTS idx_sessions_active_host ON remote_sessions(host_clean_id, status);
	`
	if _, err := s.DB.Exec(schema); err != nil {
		return fmt.Errorf("failed to migrate schema: %w", err)
	}

	// Reset any stale online flags or active sessions from previous server lifecycle
	nowStr := time.Now().Format("15:04 • 02/01")
	_, _ = s.DB.Exec(`UPDATE hosts_registry SET online = FALSE, last_seen = $1 WHERE online = TRUE`, nowStr)
	_, _ = s.DB.Exec(`UPDATE remote_sessions SET status = 'closed', ended_at = NOW() WHERE status IN ('active', 'pending')`)

	logger.Info("db.postgres", "Database tables initialized (hosts_registry, remote_sessions)")
	return nil
}

func (s *Store) UpsertHost(cleanID, deskID, deviceName, platform, remoteAddr string, isAdmin, online bool) {
	lastSeen := "Online"
	if !online {
		lastSeen = time.Now().Format("15:04 • 02/01")
	}

	query := `
	INSERT INTO hosts_registry (clean_id, desk_id, device_name, platform, is_admin, online, remote_addr, last_seen, updated_at)
	VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW())
	ON CONFLICT (clean_id) DO UPDATE SET
		desk_id     = EXCLUDED.desk_id,
		device_name = CASE WHEN EXCLUDED.device_name <> '' THEN EXCLUDED.device_name ELSE hosts_registry.device_name END,
		platform    = CASE WHEN EXCLUDED.platform <> '' THEN EXCLUDED.platform ELSE hosts_registry.platform END,
		is_admin    = EXCLUDED.is_admin,
		online      = EXCLUDED.online,
		remote_addr = CASE WHEN EXCLUDED.remote_addr <> '' THEN EXCLUDED.remote_addr ELSE hosts_registry.remote_addr END,
		last_seen   = EXCLUDED.last_seen,
		updated_at  = NOW();
	`
	if _, err := s.DB.Exec(query, cleanID, deskID, deviceName, platform, isAdmin, online, remoteAddr, lastSeen); err != nil {
		logger.Error("db.postgres", "UpsertHost failed for %s: %v", cleanID, err)
	}
}

func (s *Store) SetHostOffline(cleanID string) {
	nowStr := time.Now().Format("15:04 • 02/01")
	query := `
	UPDATE hosts_registry
	SET online = FALSE, last_seen = $2, updated_at = NOW()
	WHERE clean_id = $1;
	`
	if _, err := s.DB.Exec(query, cleanID, nowStr); err != nil {
		logger.Error("db.postgres", "SetHostOffline failed for %s: %v", cleanID, err)
	}
}

func (s *Store) GetAllHosts() []HostRecord {
	query := `
	SELECT clean_id, desk_id, device_name, platform, online, last_seen
	FROM hosts_registry
	ORDER BY online DESC, device_name ASC;
	`
	rows, err := s.DB.Query(query)
	if err != nil {
		logger.Error("db.postgres", "GetAllHosts query failed: %v", err)
		return []HostRecord{}
	}
	defer rows.Close()

	list := make([]HostRecord, 0)
	for rows.Next() {
		var r HostRecord
		if err := rows.Scan(&r.CleanID, &r.DeskID, &r.DeviceName, &r.Platform, &r.Online, &r.LastSeen); err == nil {
			list = append(list, r)
		}
	}
	return list
}

func (s *Store) StartSession(viewerClean, viewerDesk, hostClean, hostDesk, status string) {
	// Close any previous dangling sessions for these two peers first
	_, _ = s.DB.Exec(`
		UPDATE remote_sessions
		SET status = 'closed', ended_at = NOW()
		WHERE status IN ('active', 'pending')
		  AND (viewer_clean_id IN ($1, $2) OR host_clean_id IN ($1, $2));
	`, viewerClean, hostClean)

	query := `
	INSERT INTO remote_sessions (viewer_clean_id, viewer_desk_id, host_clean_id, host_desk_id, status, started_at)
	VALUES ($1, $2, $3, $4, $5, NOW());
	`
	if _, err := s.DB.Exec(query, viewerClean, viewerDesk, hostClean, hostDesk, status); err != nil {
		logger.Error("db.postgres", "StartSession failed (%s -> %s): %v", viewerClean, hostClean, err)
	}
}

func (s *Store) UpdateSessionStatus(viewerClean, hostClean, status string) {
	if status == "closed" || status == "rejected" {
		_, _ = s.DB.Exec(`
			UPDATE remote_sessions
			SET status = $3, ended_at = NOW()
			WHERE viewer_clean_id = $1 AND host_clean_id = $2 AND status IN ('active', 'pending');
		`, viewerClean, hostClean, status)
	} else {
		_, _ = s.DB.Exec(`
			UPDATE remote_sessions
			SET status = $3
			WHERE viewer_clean_id = $1 AND host_clean_id = $2 AND status = 'pending';
		`, viewerClean, hostClean, status)
	}
}

func (s *Store) CloseSessionsForPeer(cleanID string) string {
	var partnerID string
	row := s.DB.QueryRow(`
		SELECT CASE WHEN viewer_clean_id = $1 THEN host_clean_id ELSE viewer_clean_id END
		FROM remote_sessions
		WHERE (viewer_clean_id = $1 OR host_clean_id = $1) AND status IN ('active', 'pending')
		ORDER BY started_at DESC
		LIMIT 1;
	`, cleanID)
	_ = row.Scan(&partnerID)

	_, _ = s.DB.Exec(`
		UPDATE remote_sessions
		SET status = 'closed', ended_at = NOW()
		WHERE (viewer_clean_id = $1 OR host_clean_id = $1) AND status IN ('active', 'pending');
	`, cleanID)

	return partnerID
}
