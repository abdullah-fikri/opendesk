package postgres

import (
	"context"
	"log/slog"
	"os"
	"strconv"
	"testing"
	"time"

	"opendesk/server-relay/internal/model"
)

// Runs against a throwaway database only: OPENDESK_TEST_PG_PORT=55432 with user/password/db "postgres".
// docker run --rm -d -p 55432:5432 -e POSTGRES_PASSWORD=postgres postgres:16-alpine
func TestRepositoriesAgainstPostgres(t *testing.T) {
	port, err := strconv.Atoi(os.Getenv("OPENDESK_TEST_PG_PORT"))
	if err != nil {
		t.Skip("OPENDESK_TEST_PG_PORT not set")
	}
	ctx := context.Background()
	db, err := Open(ctx, Options{Host: "127.0.0.1", Port: port, User: "postgres", Password: "postgres", Name: "postgres"},
		slog.New(slog.DiscardHandler))
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()

	hosts := NewHostRepository(db)
	now := time.Now().UTC().Truncate(time.Second)
	if err := hosts.Upsert(ctx, model.Host{CleanID: "123456789", DeskID: "123 456 789", DeviceName: "PC", Platform: "windows-pc", Online: true, LastSeen: now}); err != nil {
		t.Fatal(err)
	}
	if err := hosts.SetAlias(ctx, "123456789", "Kasir 1"); err != nil {
		t.Fatal(err)
	}
	// A new registration keeps the administrator's name.
	if err := hosts.Upsert(ctx, model.Host{CleanID: "123456789", DeskID: "123 456 789", DeviceName: "PC-2", Platform: "windows-pc", Online: true, LastSeen: now}); err != nil {
		t.Fatal(err)
	}
	list, err := hosts.List(ctx)
	if err != nil || len(list) == 0 || list[0].Alias != "Kasir 1" || list[0].DeviceName != "PC-2" {
		t.Fatalf("host list: %+v %v", list, err)
	}

	sec := NewSecurityRepository(db)
	if state, err := sec.LoadSecurity(ctx); err != nil || state.Enforced {
		t.Fatalf("fresh switch must be off: %+v %v", state, err)
	}
	dev := model.AllowedDevice{CleanID: "123456789", AddedBy: "111111111", CreatedAt: now}
	if err := sec.PutAllowedDevice(ctx, dev); err != nil {
		t.Fatal(err)
	}
	dev.TokenHash, dev.EnrolledAt, dev.Admin = "abc", now, true
	if err := sec.PutAllowedDevice(ctx, dev); err != nil {
		t.Fatal(err)
	}
	if err := sec.SetEnforced(ctx, true); err != nil {
		t.Fatal(err)
	}
	state, err := sec.LoadSecurity(ctx)
	if err != nil || !state.Enforced || len(state.Devices) != 1 || state.Devices[0].TokenHash != "abc" || !state.Devices[0].Admin ||
		!state.Devices[0].EnrolledAt.Equal(now) {
		t.Fatalf("stored allowlist: %+v %v", state, err)
	}
	dev.TokenHash, dev.EnrolledAt = "", time.Time{}
	if err := sec.PutAllowedDevice(ctx, dev); err != nil {
		t.Fatal(err)
	}
	if state, _ := sec.LoadSecurity(ctx); state.Devices[0].Enrolled() || !state.Devices[0].EnrolledAt.IsZero() {
		t.Fatalf("reset not stored: %+v", state.Devices[0])
	}
	if err := sec.DeleteAllowedDevice(ctx, "123456789"); err != nil {
		t.Fatal(err)
	}
	if state, _ := sec.LoadSecurity(ctx); len(state.Devices) != 0 {
		t.Fatal("device not deleted")
	}

	testCommunities(t, ctx, hosts, NewCommunityRepository(db), now)
}

// testCommunities covers migration 0004: groups and memberships cascade, deleting a device drops its memberships.
func testCommunities(t *testing.T, ctx context.Context, hosts *HostRepository, repo *CommunityRepository, now time.Time) {
	for _, id := range []string{"222222222", "333333333"} {
		if err := hosts.Upsert(ctx, model.Host{CleanID: id, DeskID: id, DeviceName: "PC", Platform: "windows-pc", Online: id == "222222222", LastSeen: now}); err != nil {
			t.Fatal(err)
		}
	}
	c := model.Community{ID: "c1test", Name: "Shops", Icon: "🏪", Color: "blue", CreatedBy: "111111111", CreatedAt: now}
	if err := repo.CreateCommunity(ctx, c); err != nil {
		t.Fatal(err)
	}
	for _, g := range []model.Group{{ID: "g1test", CommunityID: c.ID, Name: "Front", CreatedAt: now}, {ID: "g2test", CommunityID: c.ID, Name: "Back", CreatedAt: now.Add(time.Second)}} {
		if err := repo.CreateGroup(ctx, g); err != nil {
			t.Fatal(err)
		}
	}
	// 999999999 is not registered and is skipped; adding twice is harmless.
	for i := 0; i < 2; i++ {
		if err := repo.AddMembers(ctx, "g1test", []string{"222222222", "333333333", "999999999"}); err != nil {
			t.Fatal(err)
		}
	}
	if err := repo.AddMembers(ctx, "g2test", []string{"333333333"}); err != nil {
		t.Fatal(err)
	}
	if err := repo.UpdateCommunity(ctx, c.ID, "Stores", "🛒", "green"); err != nil {
		t.Fatal(err)
	}
	if err := repo.RenameGroup(ctx, "g2test", "Kitchen"); err != nil {
		t.Fatal(err)
	}
	all, err := repo.ListCommunities(ctx)
	if err != nil || len(all) != 1 || all[0].Name != "Stores" || all[0].Icon != "🛒" || len(all[0].Groups) != 2 ||
		all[0].Groups[1].Name != "Kitchen" || len(all[0].Groups[0].Members) != 2 {
		t.Fatalf("communities: %+v %v", all, err)
	}

	n, err := hosts.Delete(ctx, []string{"333333333"})
	if err != nil || n != 1 {
		t.Fatalf("delete offline: %d %v", n, err)
	}
	all, _ = repo.ListCommunities(ctx)
	if m := all[0].Groups[0].Members; len(m) != 1 || m[0] != "222222222" || len(all[0].Groups[1].Members) != 0 {
		t.Fatalf("memberships after delete: %+v", all[0].Groups)
	}
	if err := repo.RemoveMembers(ctx, "g1test", []string{"222222222"}); err != nil {
		t.Fatal(err)
	}
	if err := repo.DeleteGroup(ctx, "g1test"); err != nil {
		t.Fatal(err)
	}
	if err := repo.DeleteCommunity(ctx, c.ID); err != nil {
		t.Fatal(err)
	}
	if all, _ = repo.ListCommunities(ctx); len(all) != 0 {
		t.Fatalf("community not deleted: %+v", all)
	}
	var left int
	if err := repo.db.QueryRowContext(ctx, `SELECT COUNT(*) FROM community_groups`).Scan(&left); err != nil || left != 0 {
		t.Fatalf("groups did not cascade: %d %v", left, err)
	}
}
