package handler

import (
	"testing"
	"time"

	"opendesk/server-relay/internal/usecase"
)

func TestSessionResumeOverWebSocket(t *testing.T) {
	h := newTestHandler(usecase.Options{}, Options{})
	dial := dialer(t, h)
	caps := map[string]any{"caps": []string{"session-resume"}}

	viewer := dial("111111111", caps)
	readJSON(t, viewer, "registered-ack")
	host := dial("222222222", caps)
	ack := readJSON(t, host, "registered-ack")
	token, _ := ack["resumeToken"].(string)
	if len(token) != 32 {
		t.Fatalf("ack without a resume token: %v", ack)
	}
	_ = viewer.WriteJSON(map[string]string{"type": "connect-request", "targetId": "222222222"})
	readJSON(t, viewer, "session-connected")
	readJSON(t, host, "session-connected")

	host.Close()
	if m := readJSON(t, viewer, "session-suspended"); m["targetClean"] != "222222222" {
		t.Fatalf("suspended: %v", m)
	}
	if !h.relay.Reconnecting("222222222") {
		t.Fatal("suspended host must be reconnecting")
	}

	back := dial("222222222", map[string]any{"caps": []string{"session-resume"}, "resumeToken": token})
	ack = readJSON(t, back, "registered-ack")
	if resumed, _ := ack["resumed"].([]any); len(resumed) != 1 || resumed[0] != "111111111" {
		t.Fatalf("ack.resumed: %v", ack)
	}
	if m := readJSON(t, viewer, "session-resumed"); m["targetClean"] != "222222222" {
		t.Fatalf("resumed: %v", m)
	}
	_ = viewer.WriteJSON(map[string]any{"type": "relay-input", "event": map[string]any{"action": "mousemove"}})
	readJSON(t, back, "input-command")
}

func TestSessionResumeExpiry(t *testing.T) {
	h := newTestHandler(usecase.Options{ResumeGrace: 50 * time.Millisecond}, Options{})
	dial := dialer(t, h)
	caps := map[string]any{"caps": []string{"session-resume"}}
	viewer := dial("111111111", caps)
	readJSON(t, viewer, "registered-ack")
	host := dial("222222222", caps)
	readJSON(t, host, "registered-ack")
	_ = viewer.WriteJSON(map[string]string{"type": "connect-request", "targetId": "222222222"})
	readJSON(t, viewer, "session-connected")

	host.Close()
	readJSON(t, viewer, "session-suspended")
	time.Sleep(60 * time.Millisecond)
	for _, e := range h.relay.ExpireSuspended(time.Now()) {
		h.endSuspension(e)
	}
	if m := readJSON(t, viewer, "session-closed"); m["message"] != msgPeerOffline {
		t.Fatalf("closed: %v", m)
	}
}

func TestOldViewerKeepsOldBehavior(t *testing.T) {
	h := newTestHandler(usecase.Options{}, Options{})
	dial := dialer(t, h)
	viewer := dial("111111111", map[string]any{"caps": []string{}})
	if ack := readJSON(t, viewer, "registered-ack"); ack["resumeToken"] != nil {
		t.Fatalf("token for an old client: %v", ack)
	}
	host := dial("222222222", map[string]any{"caps": []string{"session-resume"}})
	readJSON(t, host, "registered-ack")
	_ = viewer.WriteJSON(map[string]string{"type": "connect-request", "targetId": "222222222"})
	readJSON(t, viewer, "session-connected")
	host.Close()
	readJSON(t, viewer, "session-closed")
}
