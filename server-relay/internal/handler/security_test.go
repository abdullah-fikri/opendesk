package handler

import (
	"testing"
	"time"

	"opendesk/server-relay/internal/usecase"
)

// The whole zero-configuration flow over WebSocket: an administrator lists a device and turns the allowlist on;
// the device enrolls on its next connection and from then on needs the token it was given.
func TestClientTokenFlow(t *testing.T) {
	h := newTestHandler(usecase.Options{}, Options{})
	dial := dialer(t, h)

	admin := dial("111111111", map[string]any{"isAdmin": true})
	readJSON(t, admin, "registered-ack")
	stranger := dial("333333333", nil)
	readJSON(t, stranger, "registered-ack")

	_ = admin.WriteJSON(map[string]any{"type": "security-allow", "targetId": "222 222 222"})
	if m := readJSON(t, admin, "security-state"); m["enabled"] != false || len(m["devices"].([]any)) != 1 {
		t.Fatalf("after allow: %v", m)
	}
	_ = admin.WriteJSON(map[string]any{"type": "security-set", "enabled": true})
	adminToken := readJSON(t, admin, "device-credential")["token"].(string)
	if m := readJSON(t, stranger, "error"); m["message"] != msgInvalidCredentials {
		t.Fatalf("unlisted device: %v", m)
	}
	waitFor(t, func() bool { return h.relay.Lookup("333333333") == nil }, "unlisted device disconnected")

	// First connection of the listed device: admitted, token handed over.
	dev := dial("222222222", nil)
	readJSON(t, dev, "registered-ack")
	token := readJSON(t, dev, "device-credential")["token"].(string)
	_ = dev.Close()
	waitFor(t, func() bool { return h.relay.Lookup("222222222") == nil }, "device gone")

	if m := readJSON(t, dial("222222222", nil), "error"); m["message"] != msgInvalidCredentials {
		t.Fatalf("enrolled device admitted without its token: %v", m)
	}
	again := dial("222222222", map[string]any{"deviceToken": token})
	readJSON(t, again, "registered-ack")

	// The administrator's own reconnect needs its token too.
	_ = admin.Close()
	readJSON(t, dial("111111111", map[string]any{"isAdmin": true, "deviceToken": adminToken}), "registered-ack")
}

func TestHostRenameNeedsAnAdministrator(t *testing.T) {
	h := newTestHandler(usecase.Options{}, Options{})
	dial := dialer(t, h)
	user := dial("222222222", nil)
	readJSON(t, user, "registered-ack")
	_ = user.WriteJSON(map[string]any{"type": "security-set", "enabled": true})
	_ = user.WriteJSON(map[string]any{"type": "host-rename", "targetClean": "222222222", "alias": "x"})
	time.Sleep(100 * time.Millisecond)
	if v, _ := h.relay.Security(&usecase.Peer{IsAdmin: true}); v.Enforced {
		t.Fatal("a non-administrator turned the allowlist on")
	}
}

func waitFor(t *testing.T, cond func() bool, what string) {
	t.Helper()
	deadline := time.Now().Add(2 * time.Second)
	for !cond() {
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting: %s", what)
		}
		time.Sleep(10 * time.Millisecond)
	}
}
