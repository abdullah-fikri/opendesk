package handler

import (
	"io"
	"log/slog"
	"slices"
	"testing"
	"time"

	"opendesk/server-relay/internal/udprelay"
	"opendesk/server-relay/internal/usecase"
)

func TestUDPRelayOfferNeedsBothClients(t *testing.T) {
	fwd, err := udprelay.Listen("127.0.0.1:0", 10, slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err != nil {
		t.Fatal(err)
	}
	h := newTestHandler(usecase.Options{UDPRelay: true}, Options{UDPRelay: fwd, UDPRelayPublic: "203.0.113.5:8089"})
	dial := dialer(t, h)
	both := map[string]any{"caps": []any{"p2p.v1", "udp-relay"}}

	viewer := dial("111111111", both)
	ack := readJSON(t, viewer, "registered-ack")
	if caps, _ := ack["caps"].([]any); !slices.Contains(caps, any("udp-relay")) {
		t.Fatalf("relay does not advertise udp-relay: %v", ack["caps"])
	}
	host := dial("222222222", both)
	readJSON(t, host, "registered-ack")
	_ = viewer.WriteJSON(map[string]string{"type": "connect-request", "targetId": "222222222"})
	readJSON(t, viewer, "session-connected")
	ov := readJSON(t, viewer, "udp-relay-offer")
	oh := readJSON(t, host, "udp-relay-offer")
	if ov["targetClean"] != "222222222" || oh["targetClean"] != "111111111" || ov["addr"] != "203.0.113.5:8089" {
		t.Fatalf("offers: %v / %v", ov, oh)
	}
	if tv, _ := ov["token"].(string); len(tv) != 32 || tv == oh["token"] {
		t.Fatalf("tokens: %v / %v", ov["token"], oh["token"])
	}

	// A host without the capability: a session, but no offer.
	legacy := dial("333333333", map[string]any{"caps": []any{"p2p.v1"}})
	readJSON(t, legacy, "registered-ack")
	viewer2 := dial("444444444", both)
	readJSON(t, viewer2, "registered-ack")
	_ = viewer2.WriteJSON(map[string]string{"type": "connect-request", "targetId": "333333333"})
	readJSON(t, viewer2, "session-connected")
	_ = viewer2.SetReadDeadline(time.Now().Add(300 * time.Millisecond))
	for {
		var m map[string]any
		if err := viewer2.ReadJSON(&m); err != nil {
			break
		}
		if m["type"] == "udp-relay-offer" {
			t.Fatal("offer sent to a session whose host cannot use it")
		}
	}
}
