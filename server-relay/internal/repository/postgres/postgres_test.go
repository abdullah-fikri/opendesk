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
}
