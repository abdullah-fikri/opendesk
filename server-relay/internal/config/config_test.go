package config

import (
	"os"
	"path/filepath"
	"testing"
)

func TestLoadDefaultsAndOverrides(t *testing.T) {
	t.Setenv("DB_HOST", "db")
	t.Setenv("DB_PORT", "6543")
	t.Setenv("OPENDESK_TLS_PROXY", "1")
	cfg, err := Load(filepath.Join(t.TempDir(), "missing.json"))
	if err != nil || cfg.Port != 8088 || cfg.DBHost != "db" || cfg.DBPort != 6543 || !cfg.TLSProxy {
		t.Fatalf("unexpected config %+v %v", cfg, err)
	}
}

func TestLoadFailsClosed(t *testing.T) {
	t.Setenv("OPENDESK_DEVICE_CREDENTIALS", "{not json")
	if _, err := Load("missing.json"); err == nil {
		t.Fatal("malformed credentials must stop the relay instead of admitting every device")
	}
	t.Setenv("OPENDESK_DEVICE_CREDENTIALS", "")
	bad := filepath.Join(t.TempDir(), "config.json")
	if err := os.WriteFile(bad, []byte("{"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := Load(bad); err == nil {
		t.Fatal("malformed config file accepted")
	}
}

func TestUDPRelaySettings(t *testing.T) {
	missing := filepath.Join(t.TempDir(), "missing.json")
	cfg, err := Load(missing)
	if err != nil || cfg.UDPRelayPublic != "" || cfg.UDPRelayAddr != "0.0.0.0:8089" || cfg.UDPRelayMbps != 50 {
		t.Fatalf("defaults: %+v %v", cfg, err)
	}
	t.Setenv("OPENDESK_UDP_RELAY_PUBLIC", "203.0.113.5:8089")
	t.Setenv("OPENDESK_UDP_RELAY_MBPS", "80")
	if cfg, err = Load(missing); err != nil || cfg.UDPRelayPublic != "203.0.113.5:8089" || cfg.UDPRelayMbps != 80 {
		t.Fatalf("overrides: %+v %v", cfg, err)
	}
	t.Setenv("OPENDESK_UDP_RELAY_PUBLIC", "relay.example.com:8089")
	if _, err := Load(missing); err == nil {
		t.Fatal("a host name was accepted; clients need an IP address")
	}
}
