package usecase

import (
	"context"
	"crypto/subtle"
	"log/slog"
	"slices"
	"sync"
	"time"

	"opendesk/server-relay/internal/model"
)

// storageTimeout bounds every repository call so a slow database cannot stall a connection.
const storageTimeout = 5 * time.Second

// Options configures a Relay.
type Options struct {
	// Credentials enables per-device tokens when non-empty; then only the stored admin flag grants admin rights.
	Credentials map[string]model.DeviceCredential
	// P2PDisabled withdraws the p2p-signal capability and drops P2P signaling.
	P2PDisabled bool
}

// Relay is the in-memory session broker. All exported methods are safe for concurrent use.
type Relay struct {
	mu     sync.RWMutex
	peers  map[string]*Peer      // keyed by clean desk ID; guarded by mu
	grants map[string]*fileGrant // guarded by mu

	hosts       HostRepository
	sessions    SessionRepository
	metrics     FrameMetrics
	credentials map[string]model.DeviceCredential
	caps        []string
	p2pDisabled bool
	log         *slog.Logger
}

// New builds a Relay on top of the given storage.
func New(hosts HostRepository, sessions SessionRepository, m FrameMetrics, opts Options, log *slog.Logger) *Relay {
	caps := []string{model.CapFileGrant, model.CapFileOffer, model.CapP2PSignal, model.CapCaps, model.CapMultiSession}
	if opts.P2PDisabled {
		caps = slices.DeleteFunc(caps, func(c string) bool { return c == model.CapP2PSignal })
	}
	creds := opts.Credentials
	if creds == nil {
		creds = map[string]model.DeviceCredential{}
	}
	return &Relay{
		peers:       make(map[string]*Peer),
		grants:      make(map[string]*fileGrant),
		hosts:       hosts,
		sessions:    sessions,
		metrics:     m,
		credentials: creds,
		caps:        caps,
		p2pDisabled: opts.P2PDisabled,
		log:         log.With("component", "relay"),
	}
}

// Capabilities lists the features this relay advertises in registered-ack.
func (r *Relay) Capabilities() []string { return r.caps }

// P2PEnabled reports whether P2P signaling may be forwarded.
func (r *Relay) P2PEnabled() bool { return !r.p2pDisabled }

// ResetStaleState marks every host offline and closes every open session left over from a previous run.
func (r *Relay) ResetStaleState(ctx context.Context) error {
	if err := r.hosts.MarkAllOffline(ctx, time.Now()); err != nil {
		return err
	}
	return r.sessions.CloseAllOpen(ctx)
}

// authenticate admits every device when no credentials are configured (the default) and trusts its claimed
// role. With credentials configured, only a matching token is admitted and only the stored admin flag counts.
func (r *Relay) authenticate(clean, token string, claimedAdmin bool) (allowed, admin bool) {
	if len(r.credentials) == 0 {
		return true, claimedAdmin
	}
	cred, ok := r.credentials[clean]
	if !ok || len(cred.Token) < model.MinDeviceTokenLength || len(token) != len(cred.Token) ||
		subtle.ConstantTimeCompare([]byte(token), []byte(cred.Token)) != 1 {
		return false, false
	}
	return true, cred.Admin
}

// Register admits a new connection. It returns the new peer and, when it replaced an older connection with the
// same ID, the session partners of that connection, which the caller must notify.
func (r *Relay) Register(conn Conn, info PeerInfo, token string) (*Peer, []*Peer, error) {
	if !model.IsValidDeskID(info.CleanID) {
		return nil, nil, ErrInvalidDeskID
	}
	allowed, admin := r.authenticate(info.CleanID, token, info.IsAdmin)
	if !allowed {
		return nil, nil, ErrUnauthorized
	}
	info.IsAdmin = admin
	info.Caps = model.SanitizeCapabilities(info.Caps)
	peer := newPeer(info, model.FormatDeskID(info.CleanID), slices.Contains(info.Caps, model.CapMultiSession), conn, r.metrics)

	r.mu.Lock()
	var orphans []*Peer
	if old, ok := r.peers[peer.CleanID]; ok && old != peer {
		orphans = r.detachPartnersLocked(old)
		r.dropGrantsLocked(old, time.Now())
		old.close()
		_ = old.conn.Close()
	}
	r.peers[peer.CleanID] = peer
	r.metrics.ObservePeers(len(r.peers))
	r.mu.Unlock()

	r.store("upsert host", func(ctx context.Context) error {
		return r.hosts.Upsert(ctx, model.Host{
			CleanID: peer.CleanID, DeskID: peer.DeskID, DeviceName: peer.DeviceName, Platform: peer.Platform,
			IsAdmin: peer.IsAdmin, Online: true, RemoteAddr: info.RemoteAddr, LastSeen: time.Now(),
		})
	})
	return peer, orphans, nil
}

// Unregister removes p if it is still the current connection for its ID. It returns the partners to notify;
// a legacy partner named by sessionWith is notified even when it no longer points back.
func (r *Relay) Unregister(p *Peer) (removed bool, partners []*Peer) {
	r.mu.Lock()
	if r.peers[p.CleanID] != p {
		r.mu.Unlock()
		return false, nil
	}
	p.close()
	legacy := r.peers[p.sessionWith]
	partners = r.detachPartnersLocked(p)
	if legacy != nil && (len(partners) == 0 || partners[0] != legacy) {
		partners = append([]*Peer{legacy}, partners...)
	}
	r.dropGrantsLocked(p, time.Now())
	delete(r.peers, p.CleanID)
	r.mu.Unlock()

	r.store("mark host offline", func(ctx context.Context) error { return r.hosts.MarkOffline(ctx, p.CleanID, time.Now()) })
	r.store("close peer sessions", func(ctx context.Context) error { return r.sessions.CloseForPeer(ctx, p.CleanID) })
	return true, partners
}

// IsCurrent reports whether p is still the registered connection for its ID (not replaced or removed).
func (r *Relay) IsCurrent(p *Peer) bool {
	r.mu.RLock()
	defer r.mu.RUnlock()
	return p != nil && r.peers[p.CleanID] == p
}

// Lookup returns the current connection for cleanID, or nil.
func (r *Relay) Lookup(cleanID string) *Peer {
	r.mu.RLock()
	defer r.mu.RUnlock()
	return r.peers[cleanID]
}

// PeerCount is the number of registered connections.
func (r *Relay) PeerCount() int {
	r.mu.RLock()
	defer r.mu.RUnlock()
	return len(r.peers)
}

// Peers returns a snapshot of every registered connection.
func (r *Relay) Peers() []*Peer {
	r.mu.RLock()
	defer r.mu.RUnlock()
	out := make([]*Peer, 0, len(r.peers))
	for _, p := range r.peers {
		out = append(out, p)
	}
	return out
}

// Admins returns a snapshot of the connected administrators.
func (r *Relay) Admins() []*Peer {
	r.mu.RLock()
	defer r.mu.RUnlock()
	var out []*Peer
	for _, p := range r.peers {
		if p.IsAdmin {
			out = append(out, p)
		}
	}
	return out
}

// HostList returns the device registry, online devices first.
func (r *Relay) HostList(ctx context.Context) ([]model.Host, error) {
	ctx, cancel := context.WithTimeout(ctx, storageTimeout)
	defer cancel()
	started := time.Now()
	hosts, err := r.hosts.List(ctx)
	r.metrics.ObserveHostList(time.Since(started))
	return hosts, err
}

// RunJanitor drops expired file grants every interval until ctx ends.
func (r *Relay) RunJanitor(ctx context.Context, interval time.Duration) {
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case now := <-ticker.C:
			r.mu.Lock()
			r.dropGrantsLocked(nil, now)
			r.mu.Unlock()
		}
	}
}

// store runs one repository write with a timeout. Storage is an audit trail, so a failure is logged and the
// live session continues.
func (r *Relay) store(op string, fn func(context.Context) error) {
	ctx, cancel := context.WithTimeout(context.Background(), storageTimeout)
	defer cancel()
	if err := fn(ctx); err != nil {
		r.log.Error("storage write failed", "op", op, "err", err)
	}
}
