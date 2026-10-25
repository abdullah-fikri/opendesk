package model

import (
	"strings"
	"testing"
)

func TestDeskID(t *testing.T) {
	if got := CleanDeskID("123 456-789"); got != "123456789" {
		t.Fatalf("clean: %q", got)
	}
	if got := FormatDeskID("123456789"); got != "123 456 789" {
		t.Fatalf("format: %q", got)
	}
	for id, want := range map[string]bool{"123456789": true, "012345678": false, "12345678": false, "1234567890": false, "12345678a": false} {
		if IsValidDeskID(id) != want {
			t.Fatalf("IsValidDeskID(%q) != %v", id, want)
		}
	}
}

func TestSanitizeCapabilities(t *testing.T) {
	got := SanitizeCapabilities([]string{"file-ack", "BAD", "", strings.Repeat("x", 33), "p2p.v1"})
	if len(got) != 2 || got[0] != "file-ack" || got[1] != "p2p.v1" {
		t.Fatalf("unexpected caps: %v", got)
	}
	if SanitizeCapabilities(nil) == nil {
		t.Fatal("result must never be nil (encodes as [] for peerCaps)")
	}
}
