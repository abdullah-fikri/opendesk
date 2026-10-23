// Package config loads relay settings from an optional JSON file and environment variables.
// It is the only package that reads the process environment.
package config

import (
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"strconv"
	"time"

	"opendesk/server-relay/internal/model"
)

// Config holds every tunable of the relay. File values override defaults; environment variables override both.
type Config struct {
	Host                string `json:"host"`
	Port                int    `json:"port"`
	PingIntervalSeconds int    `json:"ping_interval_seconds"`
	ReadBufferSize      int    `json:"read_buffer_size"`
	WriteBufferSize     int    `json:"write_buffer_size"`
	DBHost              string `json:"db_host"`
	DBPort              int    `json:"db_port"`
	DBUser              string `json:"db_user"`
	DBPassword          string `json:"db_password"`
	DBName              string `json:"db_name"`

	// DeviceCredentials enables per-device tokens when non-empty (OPENDESK_DEVICE_CREDENTIALS).
	DeviceCredentials map[string]model.DeviceCredential `json:"-"`
	// MetricsToken protects GET /metrics; shorter than 32 characters disables the endpoint (OPENDESK_METRICS_TOKEN).
	MetricsToken string `json:"-"`
	// TLSProxy trusts plaintext WebSocket from non-loopback addresses because TLS ends at a proxy (OPENDESK_TLS_PROXY=1).
	TLSProxy bool `json:"-"`
	// P2PDisabled withdraws the p2p-signal capability so every session stays on the relay (OPENDESK_P2P_DISABLED=1).
	P2PDisabled bool `json:"-"`
}

// PingInterval is the WebSocket keep-alive period.
func (c Config) PingInterval() time.Duration {
	return time.Duration(c.PingIntervalSeconds) * time.Second
}

// Address is the host:port the HTTP server listens on.
func (c Config) Address() string {
	return fmt.Sprintf("%s:%d", c.Host, c.Port)
}

func defaults() Config {
	return Config{
		Host:                "0.0.0.0",
		Port:                8088,
		PingIntervalSeconds: 15,
		ReadBufferSize:      32768,
		WriteBufferSize:     32768,
		DBHost:              "localhost",
		DBPort:              5432,
		DBUser:              "postgres",
		DBPassword:          "postgres",
		DBName:              "opendesk",
	}
}

// Load reads path (a missing file is fine) and applies environment overrides. It fails on malformed input
// instead of silently falling back, so a broken credential list can never open the relay to everyone.
func Load(path string) (Config, error) {
	cfg := defaults()

	data, err := os.ReadFile(path)
	switch {
	case err == nil:
		if err := json.Unmarshal(data, &cfg); err != nil {
			return Config{}, fmt.Errorf("parse %s: %w", path, err)
		}
	case !errors.Is(err, fs.ErrNotExist):
		return Config{}, fmt.Errorf("read %s: %w", path, err)
	}

	overrideString(&cfg.DBHost, "DB_HOST")
	overrideString(&cfg.DBUser, "DB_USER")
	overrideString(&cfg.DBPassword, "DB_PASSWORD")
	overrideString(&cfg.DBName, "DB_NAME")
	if err := overrideInt(&cfg.DBPort, "DB_PORT"); err != nil {
		return Config{}, err
	}

	cfg.MetricsToken = os.Getenv("OPENDESK_METRICS_TOKEN")
	cfg.TLSProxy = os.Getenv("OPENDESK_TLS_PROXY") == "1"
	cfg.P2PDisabled = os.Getenv("OPENDESK_P2P_DISABLED") == "1"

	cfg.DeviceCredentials = map[string]model.DeviceCredential{}
	if raw := os.Getenv("OPENDESK_DEVICE_CREDENTIALS"); raw != "" {
		if err := json.Unmarshal([]byte(raw), &cfg.DeviceCredentials); err != nil {
			return Config{}, fmt.Errorf("parse OPENDESK_DEVICE_CREDENTIALS: %w", err)
		}
	}

	if cfg.PingIntervalSeconds <= 0 {
		return Config{}, fmt.Errorf("ping_interval_seconds must be positive, got %d", cfg.PingIntervalSeconds)
	}
	return cfg, nil
}

func overrideString(dst *string, key string) {
	if v := os.Getenv(key); v != "" {
		*dst = v
	}
}

func overrideInt(dst *int, key string) error {
	v := os.Getenv(key)
	if v == "" {
		return nil
	}
	n, err := strconv.Atoi(v)
	if err != nil {
		return fmt.Errorf("parse %s: %w", key, err)
	}
	*dst = n
	return nil
}
