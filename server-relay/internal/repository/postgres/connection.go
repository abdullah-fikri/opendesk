// Package postgres implements the usecase storage ports on PostgreSQL.
package postgres

import (
	"context"
	"database/sql"
	"embed"
	"fmt"
	"io/fs"
	"log/slog"
	"sort"
	"strconv"
	"strings"
	"time"

	_ "github.com/lib/pq" // registers the "postgres" driver
)

//go:embed migrations/*.sql
var migrations embed.FS

// Options are the connection parameters.
type Options struct {
	Host, User, Password, Name string
	Port                       int
}

// Open connects to PostgreSQL and applies pending migrations.
//
// When Host is localhost the relay may be running inside Docker while the database runs on the Docker host,
// so host.docker.internal is tried as a fallback.
func Open(ctx context.Context, opts Options, log *slog.Logger) (*sql.DB, error) {
	candidates := []string{opts.Host}
	if opts.Host == "localhost" || opts.Host == "127.0.0.1" {
		candidates = append(candidates, "host.docker.internal")
	}

	var lastErr error
	for _, host := range candidates {
		dsn := fmt.Sprintf("host=%s port=%d user=%s password=%s dbname=%s sslmode=disable connect_timeout=5",
			host, opts.Port, opts.User, opts.Password, opts.Name)
		db, err := sql.Open("postgres", dsn)
		if err != nil {
			lastErr = err
			continue
		}
		db.SetMaxOpenConns(25)
		db.SetMaxIdleConns(10)
		db.SetConnMaxLifetime(30 * time.Minute)
		if err := db.PingContext(ctx); err != nil {
			_ = db.Close()
			lastErr = err
			continue
		}
		log.Info("connected to PostgreSQL", "component", "postgres", "host", host, "port", opts.Port, "db", opts.Name)
		if err := migrate(ctx, db, log); err != nil {
			_ = db.Close()
			return nil, err
		}
		return db, nil
	}
	return nil, fmt.Errorf("connect to postgres %s:%d/%s: %w", opts.Host, opts.Port, opts.Name, lastErr)
}

// migrate applies every embedded migration newer than the recorded schema version, each in its own
// transaction. Migration files are append-only: never edit one that has shipped.
func migrate(ctx context.Context, db *sql.DB, log *slog.Logger) error {
	if _, err := db.ExecContext(ctx, `CREATE TABLE IF NOT EXISTS schema_migrations (
		version    INTEGER PRIMARY KEY,
		applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
	)`); err != nil {
		return fmt.Errorf("create schema_migrations: %w", err)
	}

	names, err := fs.Glob(migrations, "migrations/*.sql")
	if err != nil {
		return err
	}
	sort.Strings(names)
	for _, name := range names {
		version, err := migrationVersion(name)
		if err != nil {
			return err
		}
		var applied bool
		if err := db.QueryRowContext(ctx, `SELECT EXISTS (SELECT 1 FROM schema_migrations WHERE version = $1)`, version).Scan(&applied); err != nil {
			return fmt.Errorf("check migration %d: %w", version, err)
		}
		if applied {
			continue
		}
		body, err := migrations.ReadFile(name)
		if err != nil {
			return err
		}
		if err := applyMigration(ctx, db, version, string(body)); err != nil {
			return fmt.Errorf("apply %s: %w", name, err)
		}
		log.Info("applied migration", "component", "postgres", "file", name)
	}
	return nil
}

func applyMigration(ctx context.Context, db *sql.DB, version int, body string) error {
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer func() { _ = tx.Rollback() }()
	if _, err := tx.ExecContext(ctx, body); err != nil {
		return err
	}
	if _, err := tx.ExecContext(ctx, `INSERT INTO schema_migrations (version) VALUES ($1)`, version); err != nil {
		return err
	}
	return tx.Commit()
}

// migrationVersion parses the numeric prefix of "migrations/0002_name.sql".
func migrationVersion(name string) (int, error) {
	base := strings.TrimPrefix(name, "migrations/")
	prefix, _, ok := strings.Cut(base, "_")
	if !ok {
		return 0, fmt.Errorf("migration %s: missing version prefix", name)
	}
	return strconv.Atoi(prefix)
}
