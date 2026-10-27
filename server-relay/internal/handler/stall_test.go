package handler

import (
	"strings"
	"testing"
	"time"

	"opendesk/server-relay/internal/usecase"
)

// A multi-session viewer controls hosts a and b; a stops reading. Large clipboard texts to a fill its socket and
// then its relay queue, while input to b must keep arriving promptly: a stalled receiver may not freeze its
// sender's read loop. a is closed once it falls too far behind.
func TestStalledReceiverDoesNotBlockSender(t *testing.T) {
	h := newTestHandler(usecase.Options{}, Options{})
	dial := dialer(t, h)
	viewer := dial("111111111", map[string]any{"caps": []string{"multi-session"}})
	readJSON(t, viewer, "registered-ack")
	a := dial("222222222", nil)
	readJSON(t, a, "registered-ack")
	b := dial("333333333", nil)
	readJSON(t, b, "registered-ack")
	for _, id := range []string{"222222222", "333333333"} {
		_ = viewer.WriteJSON(map[string]string{"type": "connect-request", "targetId": id})
		readJSON(t, viewer, "session-connected")
	}
	readJSON(t, a, "session-connected")
	readJSON(t, b, "session-connected")
	// From here on a never reads.

	const rounds = 160 // 40 MiB: more than a's socket buffers plus its 16 MiB control queue
	text := strings.Repeat("x", 256<<10)
	go func() {
		for i := range rounds {
			if viewer.WriteJSON(map[string]string{"type": "relay-clipboard", "targetClean": "222222222", "text": text}) != nil ||
				viewer.WriteJSON(map[string]any{"type": "relay-input", "targetClean": "333333333", "event": map[string]any{"action": "mousemove", "seq": i}}) != nil {
				return
			}
		}
	}()

	last, worst := time.Now(), time.Duration(0)
	for i := range rounds {
		_ = b.SetReadDeadline(time.Now().Add(5 * time.Second))
		var m map[string]any
		if err := b.ReadJSON(&m); err != nil {
			t.Fatalf("input %d to the healthy host: %v", i, err)
		}
		gap := time.Since(last)
		if gap > time.Second {
			t.Fatalf("input %d to the healthy host waited %v behind the stalled one", i, gap)
		}
		worst, last = max(worst, gap), time.Now()
	}
	t.Logf("longest gap between inputs to the healthy host: %v", worst)

	deadline := time.Now().Add(5 * time.Second)
	for h.relay.Lookup("222222222") != nil {
		if time.Now().After(deadline) {
			t.Fatal("stalled receiver was never closed")
		}
		time.Sleep(10 * time.Millisecond)
	}
	// What a had buffered is still readable; then its connection ends instead of hanging.
	_ = a.SetReadDeadline(time.Now().Add(5 * time.Second))
	for {
		if _, _, err := a.ReadMessage(); err != nil {
			if strings.Contains(err.Error(), "timeout") {
				t.Fatalf("stalled receiver not disconnected: %v", err)
			}
			return
		}
	}
}
