package usecase

import (
	"testing"

	"opendesk/server-relay/internal/model"
)

func TestAllowlistEnrollsOnFirstConnectAndThenNeedsTheToken(t *testing.T) {
	r := newTestRelay(Options{})
	admin := addPeer(r, "111111111", false)
	admin.IsAdmin = true
	stranger := addPeer(r, "333333333", false)

	if _, err := r.SetEnforced(stranger, true); err != ErrNotAdmin {
		t.Fatalf("non-administrator switched the allowlist: %v", err)
	}
	if _, err := r.AllowDevice(admin, "222222222", false); err != nil {
		t.Fatal(err)
	}
	change, err := r.SetEnforced(admin, true)
	if err != nil {
		t.Fatal(err)
	}
	// The enabling administrator is listed and enrolled at once; the unlisted device is disconnected.
	if len(change.Issued) != 1 || change.Issued[0].Peer != admin || len(change.Kicked) != 1 || change.Kicked[0] != stranger {
		t.Fatalf("unexpected change: issued=%d kicked=%v", len(change.Issued), change.Kicked)
	}
	if a := r.authenticate("333333333", "", false); a.allowed {
		t.Fatal("unlisted device admitted")
	}
	if a := r.authenticate(admin.CleanID, change.Issued[0].Token, true); !a.allowed || !a.admin {
		t.Fatal("enabling administrator locked out")
	}

	first := r.authenticate("222222222", "", true)
	if !first.allowed || first.admin || len(first.token) != 64 {
		t.Fatalf("first connection of a listed device: %+v", first)
	}
	if r.authenticate("222222222", "", false).allowed || r.authenticate("222222222", "guess", false).allowed {
		t.Fatal("enrolled device admitted without its token")
	}
	if !r.authenticate("222222222", first.token, false).allowed {
		t.Fatal("enrolled device rejected with its token")
	}

	if _, err := r.ResetDevice(admin, "222222222"); err != nil {
		t.Fatal(err)
	}
	again := r.authenticate("222222222", "", false)
	if !again.allowed || again.token == "" || again.token == first.token {
		t.Fatal("reset device did not enroll again with a new token")
	}

	if _, err := r.RemoveDevice(admin, "222222222"); err != nil {
		t.Fatal(err)
	}
	if r.authenticate("222222222", again.token, false).allowed {
		t.Fatal("removed device still admitted")
	}

	if _, err := r.SetEnforced(admin, false); err != nil {
		t.Fatal(err)
	}
	if !r.authenticate("444444444", "", false).allowed {
		t.Fatal("open relay rejected a device")
	}
}

func TestAllowConnectedListsEveryOnlineDevice(t *testing.T) {
	r := newTestRelay(Options{})
	admin := addPeer(r, "111111111", false)
	admin.IsAdmin = true
	addPeer(r, "222222222", false)
	if _, err := r.AllowConnected(admin); err != nil {
		t.Fatal(err)
	}
	change, _ := r.SetEnforced(admin, true)
	if len(change.Kicked) != 0 || len(change.Issued) != 2 {
		t.Fatalf("issued=%d kicked=%d, want 2 and 0", len(change.Issued), len(change.Kicked))
	}
	view, _ := r.Security(admin)
	if len(view.Devices) != 2 || !view.Devices[0].Admin && !view.Devices[1].Admin {
		t.Fatalf("allowlist: %+v", view.Devices)
	}
}

func TestEnvironmentCredentialsTakePrecedenceOverTheAllowlist(t *testing.T) {
	token := "0123456789abcdef0123456789abcdef"
	r := newTestRelay(Options{Credentials: map[string]model.DeviceCredential{"123456789": {Token: token}}})
	admin := addPeer(r, "111111111", false)
	admin.IsAdmin = true
	if _, err := r.AllowDevice(admin, "123456789", true); err != nil {
		t.Fatal(err)
	}
	if a := r.authenticate("123456789", "", false); a.allowed || a.token != "" {
		t.Fatal("allowlist enrollment bypassed a configured credential")
	}
	if a := r.authenticate("123456789", token, false); !a.allowed || a.admin {
		t.Fatal("configured credential not honored")
	}
	// With credentials configured the allowlist applies to everyone else even when not switched on.
	if a := r.authenticate("222222222", "", false); a.allowed {
		t.Fatal("unlisted device admitted on a provisioned relay")
	}
}
