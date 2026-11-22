package usecase

import (
	"context"
	"errors"
	"log/slog"
	"strings"
	"testing"

	"opendesk/server-relay/internal/metrics"
	"opendesk/server-relay/internal/model"
	"opendesk/server-relay/internal/repository/memory"
)

func newCommunityRelay(t *testing.T) (*Relay, *memory.Store) {
	t.Helper()
	store := memory.New()
	r := New(store, nopStore{}, metrics.New(), Options{Communities: store}, slog.New(slog.DiscardHandler))
	return r, store
}

func registerPeer(t *testing.T, r *Relay, id string, admin bool) *Peer {
	t.Helper()
	p, _, err := r.Register(nopConn{}, PeerInfo{CleanID: id, IsAdmin: admin, DeviceName: "PC " + id}, "")
	if err != nil {
		t.Fatal(err)
	}
	return p
}

func hostIDs(t *testing.T, r *Relay) []string {
	t.Helper()
	hosts, err := r.HostList(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	var out []string
	for _, h := range hosts {
		out = append(out, h.CleanID)
	}
	return out
}

func TestDeleteHostsOnlyOfflineAndNotABlock(t *testing.T) {
	r, _ := newCommunityRelay(t)
	admin := registerPeer(t, r, "111111111", true)
	user := registerPeer(t, r, "222222222", false)
	gone := registerPeer(t, r, "333333333", false)
	r.Leave(gone)

	if _, err := r.DeleteHosts(user, []string{"333333333"}); !errors.Is(err, ErrNotAdmin) {
		t.Fatalf("non-administrator: %v", err)
	}
	if res, err := r.DeleteHosts(admin, []string{"222222222"}); !errors.Is(err, ErrDeviceOnline) || res.Deleted != 0 ||
		len(res.Online) != 1 {
		t.Fatalf("online device: %+v %v", res, err)
	}
	if _, err := r.DeleteHosts(admin, []string{"12"}); !errors.Is(err, ErrInvalidDeskID) {
		t.Fatalf("invalid ID: %v", err)
	}
	// Mixed request: the offline device goes, the online one is reported.
	res, err := r.DeleteHosts(admin, []string{"333 333 333", "222222222", "333333333"})
	if err != nil || res.Deleted != 1 || len(res.Online) != 1 || res.Online[0] != "222222222" {
		t.Fatalf("mixed delete: %+v %v", res, err)
	}
	if ids := hostIDs(t, r); strings.Contains(strings.Join(ids, ","), "333333333") || len(ids) != 2 {
		t.Fatalf("after delete: %v", ids)
	}
	// Not a block: the device registers again and is listed again.
	registerPeer(t, r, "333333333", false)
	if ids := hostIDs(t, r); len(ids) != 3 {
		t.Fatalf("re-registered device missing: %v", ids)
	}
}

func TestCommunityLifecycle(t *testing.T) {
	r, _ := newCommunityRelay(t)
	admin := registerPeer(t, r, "111111111", true)
	user := registerPeer(t, r, "222222222", false)
	dev := registerPeer(t, r, "333333333", false)

	if !strings.Contains(strings.Join(r.Capabilities(), ","), model.CapCommunities) {
		t.Fatal("communities capability not advertised")
	}
	if _, err := r.CreateCommunity(user, CommunityInput{Name: "Shops"}); !errors.Is(err, ErrNotAdmin) {
		t.Fatalf("non-administrator create: %v", err)
	}
	for _, bad := range []CommunityInput{{Name: "   "}, {Name: strings.Repeat("x", 49)}, {Name: "A", Color: "neon"}, {Name: "A", Icon: "abc"}} {
		if _, err := r.CreateCommunity(admin, bad); !errors.Is(err, ErrInvalidName) {
			t.Fatalf("invalid input %+v: %v", bad, err)
		}
	}
	cid, err := r.CreateCommunity(admin, CommunityInput{Name: "  Shops\n ", Icon: "🏪", Color: "blue"})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := r.CreateCommunity(admin, CommunityInput{Name: "shops"}); !errors.Is(err, ErrNameTaken) {
		t.Fatalf("duplicate name: %v", err)
	}
	gid, err := r.CreateGroup(admin, cid, "Front desk")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := r.CreateGroup(admin, "missing", "x"); !errors.Is(err, ErrNotFound) {
		t.Fatalf("group in missing community: %v", err)
	}
	if _, err := r.CreateGroup(admin, cid, "front DESK"); !errors.Is(err, ErrNameTaken) {
		t.Fatalf("duplicate group: %v", err)
	}
	// Unregistered devices are skipped; a device may be in several groups.
	other, _ := r.CreateGroup(admin, cid, "Kitchen")
	if err := r.SetGroupMembers(admin, gid, []string{"333333333", "222 222 222", "999999999"}, nil); err != nil {
		t.Fatal(err)
	}
	if err := r.SetGroupMembers(admin, other, []string{"333333333"}, nil); err != nil {
		t.Fatal(err)
	}
	if err := r.SetGroupMembers(user, gid, nil, []string{"333333333"}); !errors.Is(err, ErrNotAdmin) {
		t.Fatalf("non-administrator members: %v", err)
	}
	all, _ := r.Communities(admin)
	if len(all) != 1 || all[0].Name != "Shops" || all[0].Icon != "🏪" || len(all[0].Groups) != 2 ||
		strings.Join(all[0].Groups[0].Members, ",") != "222222222,333333333" || len(all[0].Groups[1].Members) != 1 {
		t.Fatalf("communities: %+v", all)
	}

	if err := r.RenameGroup(admin, gid, "Counter"); err != nil {
		t.Fatal(err)
	}
	if err := r.UpdateCommunity(admin, cid, CommunityInput{Name: "Stores", Color: "green"}); err != nil {
		t.Fatal(err)
	}
	// Deleting an offline device drops its memberships.
	r.Leave(dev)
	if _, err := r.DeleteHosts(admin, []string{"333333333"}); err != nil {
		t.Fatal(err)
	}
	all, _ = r.Communities(admin)
	if all[0].Name != "Stores" || all[0].Groups[0].Name != "Counter" || strings.Join(all[0].Groups[0].Members, ",") != "222222222" ||
		len(all[0].Groups[1].Members) != 0 {
		t.Fatalf("after rename and delete: %+v", all)
	}
	// Deleting a group keeps its devices registered.
	if err := r.DeleteGroup(admin, gid); err != nil {
		t.Fatal(err)
	}
	if ids := hostIDs(t, r); len(ids) != 2 {
		t.Fatalf("group delete touched the registry: %v", ids)
	}
	if err := r.DeleteCommunity(admin, cid); err != nil {
		t.Fatal(err)
	}
	if all, _ = r.Communities(admin); len(all) != 0 {
		t.Fatalf("community not deleted: %+v", all)
	}
	if err := r.DeleteCommunity(admin, cid); !errors.Is(err, ErrNotFound) {
		t.Fatalf("second delete: %v", err)
	}
}

func TestCommunitiesNeedStorage(t *testing.T) {
	r := newTestRelay(Options{})
	admin := addPeer(r, "111111111", false)
	admin.IsAdmin = true
	if _, err := r.CreateCommunity(admin, CommunityInput{Name: "x"}); !errors.Is(err, ErrUnsupported) {
		t.Fatalf("without storage: %v", err)
	}
	if strings.Contains(strings.Join(r.Capabilities(), ","), model.CapCommunities) {
		t.Fatal("capability advertised without storage")
	}
}
