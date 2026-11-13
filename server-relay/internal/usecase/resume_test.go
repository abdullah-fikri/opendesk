package usecase

import (
	"slices"
	"testing"
	"time"

	"opendesk/server-relay/internal/model"
)

var resumeCaps = []string{model.CapSessionResume}

func register(t *testing.T, r *Relay, id string, caps []string, resumeToken string) RegisterResult {
	t.Helper()
	res, err := r.RegisterResume(nopConn{}, PeerInfo{CleanID: id, Caps: caps}, "", resumeToken)
	if err != nil {
		t.Fatal(err)
	}
	return res
}

// pair registers a viewer and a host and opens an active session between them.
func pair(t *testing.T, r *Relay, viewerCaps, hostCaps []string) (viewer, host *Peer) {
	t.Helper()
	viewer = register(t, r, "111111111", viewerCaps, "").Peer
	host = register(t, r, "222222222", hostCaps, "").Peer
	if _, err := r.Connect(viewer, host.CleanID); err != nil {
		t.Fatal(err)
	}
	return viewer, host
}

func TestResumeWithinGrace(t *testing.T) {
	r := newTestRelay(Options{})
	viewer, host := pair(t, r, resumeCaps, resumeCaps)
	if host.ResumeToken == "" || len(host.ResumeToken) != 32 {
		t.Fatalf("resume token %q", host.ResumeToken)
	}
	left := r.Leave(host)
	if !left.Suspended || left.Removed || len(left.Waiting) != 1 || left.Waiting[0] != viewer || len(left.Closed) != 0 {
		t.Fatalf("leave = %+v", left)
	}
	if !r.Reconnecting(host.CleanID) || r.ActivePartner(viewer, "") != nil {
		t.Fatal("a suspended session must stay registered and forward nothing")
	}
	other := register(t, r, "333333333", nil, "").Peer
	if _, err := r.Connect(other, host.CleanID); err != ErrTargetUnavailable {
		t.Fatalf("connect to a suspended device: %v", err)
	}

	res := register(t, r, host.CleanID, resumeCaps, host.ResumeToken)
	if !slices.Equal(res.Resumed, []string{viewer.CleanID}) || len(res.Back) != 1 || res.Back[0] != viewer || len(res.Orphans) != 0 {
		t.Fatalf("resume = %+v", res)
	}
	if res.Peer.ResumeToken == host.ResumeToken {
		t.Fatal("every registration needs a fresh token")
	}
	if r.ActivePartner(viewer, "") != nil {
		t.Fatal("traffic must wait until the ack is queued")
	}
	r.Release(res.Peer)
	if r.ActivePartner(viewer, "") != res.Peer || r.ActivePartner(res.Peer, "") != viewer || r.Reconnecting(host.CleanID) {
		t.Fatal("the resumed session must route again")
	}
	// The token is accepted once.
	again := register(t, r, host.CleanID, resumeCaps, host.ResumeToken)
	if len(again.Resumed) != 0 || len(again.Orphans) != 1 {
		t.Fatalf("reused token resumed: %+v", again)
	}
}

func TestResumeGraceExpires(t *testing.T) {
	r := newTestRelay(Options{})
	viewer, host := pair(t, r, resumeCaps, resumeCaps)
	r.Leave(host)
	if got := r.ExpireSuspended(time.Now()); len(got) != 0 {
		t.Fatal("expired before the grace")
	}
	got := r.ExpireSuspended(time.Now().Add(DefaultResumeGrace + time.Second))
	if len(got) != 1 || got[0].Peer != host || len(got[0].Partners) != 1 || got[0].Partners[0] != viewer {
		t.Fatalf("expired = %+v", got)
	}
	if r.Lookup(host.CleanID) != nil {
		t.Fatal("expired device must go offline")
	}
	if exists, _ := viewer.sessionLocked(host.CleanID); exists {
		t.Fatal("the partner's session must end")
	}
}

func TestResumeAfterDeadlineRefused(t *testing.T) {
	r := newTestRelay(Options{ResumeGrace: time.Millisecond})
	viewer, host := pair(t, r, resumeCaps, resumeCaps)
	r.Leave(host)
	time.Sleep(5 * time.Millisecond)
	res := register(t, r, host.CleanID, resumeCaps, host.ResumeToken)
	if len(res.Resumed) != 0 || len(res.Orphans) != 1 || res.Orphans[0] != viewer {
		t.Fatalf("late resume = %+v", res)
	}
}

func TestResumeBeforeOldSocketNoticed(t *testing.T) {
	r := newTestRelay(Options{})
	viewer, host := pair(t, r, resumeCaps, resumeCaps)
	res := register(t, r, host.CleanID, resumeCaps, host.ResumeToken)
	if !slices.Equal(res.Resumed, []string{viewer.CleanID}) || len(res.Orphans) != 0 {
		t.Fatalf("early resume = %+v", res)
	}
	r.Release(res.Peer)
	if left := r.Leave(host); left.Removed || left.Suspended {
		t.Fatalf("the replaced socket must leave silently: %+v", left)
	}
	if r.ActivePartner(viewer, "") != res.Peer {
		t.Fatal("session must belong to the new connection")
	}
}

func TestResumeNeedsBothCapabilities(t *testing.T) {
	r := newTestRelay(Options{})
	viewer, host := pair(t, r, nil, resumeCaps)
	left := r.Leave(host)
	if !left.Removed || left.Suspended || len(left.Closed) != 1 || left.Closed[0] != viewer {
		t.Fatalf("old viewer must see the old behavior: %+v", left)
	}
	if r.Lookup(host.CleanID) != nil {
		t.Fatal("device must go offline")
	}
}

func TestResumeRequiresToken(t *testing.T) {
	for _, token := range []string{"", "00000000000000000000000000000000"} {
		r := newTestRelay(Options{})
		viewer, host := pair(t, r, resumeCaps, resumeCaps)
		r.Leave(host)
		res := register(t, r, host.CleanID, resumeCaps, token)
		if len(res.Resumed) != 0 || len(res.Orphans) != 1 || res.Orphans[0] != viewer {
			t.Fatalf("token %q: %+v", token, res)
		}
	}
	// A different credential for the same ID is refused before the token is looked at.
	devToken := "0123456789abcdef0123456789abcdef"
	r := newTestRelay(Options{Credentials: map[string]model.DeviceCredential{"222222222": {Token: devToken}}})
	host, _ := r.RegisterResume(nopConn{}, PeerInfo{CleanID: "222222222", Caps: resumeCaps}, devToken, "")
	r.Leave(host.Peer)
	if _, err := r.RegisterResume(nopConn{}, PeerInfo{CleanID: "222222222", Caps: resumeCaps}, "wrong", host.Peer.ResumeToken); err != ErrUnauthorized {
		t.Fatalf("other credentials: %v", err)
	}
}

func TestSuspendedPartnerDisconnects(t *testing.T) {
	r := newTestRelay(Options{})
	viewer, host := pair(t, r, resumeCaps, resumeCaps)
	r.Leave(host)
	if r.Disconnect(viewer, host.CleanID) != host {
		t.Fatal("partner must be able to end a suspended session")
	}
	got := r.ExpireSuspended(time.Now())
	if len(got) != 1 || len(got[0].Partners) != 0 || r.Lookup(host.CleanID) != nil {
		t.Fatalf("a suspension without sessions must end at once: %+v", got)
	}
}

func TestResumeMultiSessionMixed(t *testing.T) {
	r := newTestRelay(Options{})
	multi := []string{model.CapMultiSession, model.CapSessionResume}
	viewer := register(t, r, "111111111", multi, "").Peer
	h1 := register(t, r, "222222222", resumeCaps, "").Peer
	h2 := register(t, r, "333333333", nil, "").Peer
	for _, h := range []*Peer{h1, h2} {
		if _, err := r.Connect(viewer, h.CleanID); err != nil {
			t.Fatal(err)
		}
	}
	left := r.Leave(viewer)
	if !left.Suspended || len(left.Waiting) != 1 || left.Waiting[0] != h1 || len(left.Closed) != 1 || left.Closed[0] != h2 {
		t.Fatalf("leave = %+v", left)
	}
	// A registration that disagrees on multi-session cannot resume.
	if res := register(t, r, viewer.CleanID, resumeCaps, viewer.ResumeToken); len(res.Resumed) != 0 {
		t.Fatalf("resumed across multi-session: %+v", res)
	}

	r = newTestRelay(Options{})
	viewer = register(t, r, "111111111", multi, "").Peer
	h1 = register(t, r, "222222222", resumeCaps, "").Peer
	h3 := register(t, r, "444444444", resumeCaps, "").Peer
	for _, h := range []*Peer{h1, h3} {
		if _, err := r.Connect(viewer, h.CleanID); err != nil {
			t.Fatal(err)
		}
	}
	r.Leave(viewer)
	r.Leave(h3) // both sides away: the session waits for both
	res := register(t, r, viewer.CleanID, multi, viewer.ResumeToken)
	slices.Sort(res.Resumed)
	if !slices.Equal(res.Resumed, []string{h1.CleanID, h3.CleanID}) || !slices.Equal(res.Waiting, []string{h3.CleanID}) ||
		len(res.Back) != 1 || res.Back[0] != h1 {
		t.Fatalf("resume = %+v", res)
	}
	r.Release(res.Peer)
	if r.ActivePartner(res.Peer, h1.CleanID) != h1 || r.ActivePartner(res.Peer, h3.CleanID) != nil {
		t.Fatal("only the session with a connected host may route")
	}
}

func TestEndSuspension(t *testing.T) {
	r := newTestRelay(Options{})
	viewer, host := pair(t, r, resumeCaps, resumeCaps)
	if _, ok := r.EndSuspension(host); ok {
		t.Fatal("a connected device is not suspended")
	}
	r.Leave(host)
	e, ok := r.EndSuspension(host)
	if !ok || len(e.Partners) != 1 || e.Partners[0] != viewer || r.Lookup(host.CleanID) != nil {
		t.Fatalf("end suspension = %+v %v", e, ok)
	}
}
