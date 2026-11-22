package handler

import (
	"log/slog"
	"testing"
	"time"

	"github.com/gorilla/websocket"

	"opendesk/server-relay/internal/metrics"
	"opendesk/server-relay/internal/repository/memory"
	"opendesk/server-relay/internal/usecase"
)

// newCommunityHandler is a handler on an in-memory registry and community store.
func newCommunityHandler() *Handler {
	log := slog.New(slog.DiscardHandler)
	m := metrics.New()
	store := memory.New()
	relay := usecase.New(store, nopStore{}, m, usecase.Options{Communities: store}, log)
	return New(relay, m, Options{ReadBufferSize: 4096, WriteBufferSize: 4096}, log)
}

// hostCleans reads host-list messages until one satisfies want, and returns its clean IDs.
func waitHostList(t *testing.T, ws *websocket.Conn, want func([]string) bool) []string {
	t.Helper()
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		m := readJSON(t, ws, "host-list")
		var ids []string
		for _, h := range m["hosts"].([]any) {
			ids = append(ids, h.(map[string]any)["clean_id"].(string))
		}
		if want(ids) {
			return ids
		}
	}
	t.Fatal("expected host list never arrived")
	return nil
}

func contains(ids []string, id string) bool {
	for _, x := range ids {
		if x == id {
			return true
		}
	}
	return false
}

func TestHostDeleteOverWebSocket(t *testing.T) {
	h := newCommunityHandler()
	dial := dialer(t, h)
	admin := dial("111111111", map[string]any{"isAdmin": true})
	if caps := readJSON(t, admin, "registered-ack")["caps"].([]any); !containsAny(caps, "host-delete") || !containsAny(caps, "communities") {
		t.Fatalf("capabilities: %v", caps)
	}
	user := dial("222222222", nil)
	readJSON(t, user, "registered-ack")
	dev := dial("333333333", nil)
	readJSON(t, dev, "registered-ack")
	_ = dev.Close()
	waitFor(t, func() bool { return h.relay.Lookup("333333333") == nil }, "device offline")

	// A non-administrator is ignored.
	_ = user.WriteJSON(map[string]any{"type": "host-delete", "targetCleans": []string{"333333333"}})
	// An online device is refused with a message.
	_ = admin.WriteJSON(map[string]any{"type": "host-delete", "targetCleans": []string{"222222222"}})
	readJSON(t, admin, "error")
	_ = admin.WriteJSON(map[string]any{"type": "get-hosts"})
	if ids := waitHostList(t, admin, func(ids []string) bool { return len(ids) == 3 }); !contains(ids, "333333333") {
		t.Fatalf("non-administrator deleted a device: %v", ids)
	}

	_ = admin.WriteJSON(map[string]any{"type": "host-delete", "targetCleans": []string{"333333333"}})
	waitHostList(t, admin, func(ids []string) bool { return len(ids) == 2 && !contains(ids, "333333333") })

	// The deleted device simply registers again.
	readJSON(t, dial("333333333", nil), "registered-ack")
	waitHostList(t, admin, func(ids []string) bool { return contains(ids, "333333333") })
}

func containsAny(list []any, s string) bool {
	for _, x := range list {
		if x == s {
			return true
		}
	}
	return false
}

func TestCommunityEditsOverWebSocket(t *testing.T) {
	h := newCommunityHandler()
	dial := dialer(t, h)
	admin := dial("111111111", map[string]any{"isAdmin": true})
	readJSON(t, admin, "registered-ack")
	user := dial("222222222", nil)
	readJSON(t, user, "registered-ack")

	_ = user.WriteJSON(map[string]any{"type": "community-create", "name": "Evil"})
	_ = admin.WriteJSON(map[string]any{"type": "community-create", "name": "  Shops ", "icon": "🏪", "color": "blue"})
	m := readJSON(t, admin, "community-list")
	list := m["communities"].([]any)
	if len(list) != 1 || list[0].(map[string]any)["name"] != "Shops" {
		t.Fatalf("after create: %v", m)
	}
	cid := list[0].(map[string]any)["id"].(string)
	if groups := list[0].(map[string]any)["groups"].([]any); len(groups) != 0 {
		t.Fatalf("groups must be an empty list: %v", m)
	}

	_ = admin.WriteJSON(map[string]any{"type": "community-create", "name": ""})
	if e := readJSON(t, admin, "error"); e["message"] != msgInvalidName {
		t.Fatalf("empty name: %v", e)
	}
	_ = admin.WriteJSON(map[string]any{"type": "group-create", "communityId": cid, "name": "Front desk"})
	m = readJSON(t, admin, "community-list")
	gid := m["communities"].([]any)[0].(map[string]any)["groups"].([]any)[0].(map[string]any)["id"].(string)
	_ = admin.WriteJSON(map[string]any{"type": "group-members", "groupId": gid, "add": []string{"222222222"}})
	m = readJSON(t, admin, "community-list")
	g := m["communities"].([]any)[0].(map[string]any)["groups"].([]any)[0].(map[string]any)
	if members := g["members"].([]any); len(members) != 1 || members[0] != "222222222" {
		t.Fatalf("members: %v", g)
	}
	// A non-administrator's get-communities is not answered; the administrator's is.
	_ = user.WriteJSON(map[string]any{"type": "get-communities"})
	_ = admin.WriteJSON(map[string]any{"type": "get-communities"})
	readJSON(t, admin, "community-list")
	all, _ := h.relay.Communities(h.relay.Lookup("111111111"))
	if len(all) != 1 {
		t.Fatalf("non-administrator created a community: %+v", all)
	}
}

func TestEditBucketLimitsBursts(t *testing.T) {
	var b editBucket
	now := time.Now()
	for i := 0; i < editBurst; i++ {
		if !b.take(now) {
			t.Fatalf("burst refused at %d", i)
		}
	}
	if b.take(now) {
		t.Fatal("burst not limited")
	}
	if !b.take(now.Add(editInterval)) {
		t.Fatal("bucket did not refill")
	}
}
