package usecase

import (
	"context"
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
	// Security persists the administrator-managed device allowlist; nil keeps it in memory only.
	Security SecurityRepository
	// UDPRelay advertises the udp-relay capability (the UDP forwarder runs).
	UDPRelay bool
	// MoveCoalescingDisabled queues every relayed mouse move in its own slot instead of letting a newer move
	// replace a queued one (an emergency switch; see SendInput).
	MoveCoalescingDisabled bool
	// ResumeGrace is how long a lost connection's sessions wait for it; zero means DefaultResumeGrace.
	ResumeGrace time.Duration
}

// Relay is the in-memory session broker. All exported methods are safe for concurrent use.
type Relay struct {
	mu     sync.RWMutex
	peers  map[string]*Peer      // keyed by clean desk ID; guarded by mu
	grants map[string]*fileGrant // guarded by mu
	// monitors holds the monitor-wall feeds keyed by host clean ID; guarded by mu.
	monitors map[string]*monitorFeed

	hosts       HostRepository
	sessions    SessionRepository
	metrics     FrameMetrics
	credentials map[string]model.DeviceCredential
	// sec is the device allowlist (see security.go); securityRepo may be nil.
	sec          security
	securityRepo SecurityRepository
	caps         []string
	p2pDisabled  bool
	mergeMoves   bool
	resumeGrace  time.Duration
	log          *slog.Logger
}

// New builds a Relay on top of the given storage.
func New(hosts HostRepository, sessions SessionRepository, m FrameMetrics, opts Options, log *slog.Logger) *Relay {
	caps := []string{model.CapFileGrant, model.CapFileOffer, model.CapP2PSignal, model.CapCaps, model.CapMultiSession, model.CapMonitor,
		model.CapDeviceSecurity, model.CapHostAlias, model.CapFileBinary, model.CapSessionResume}
	if opts.P2PDisabled {
		caps = slices.DeleteFunc(caps, func(c string) bool { return c == model.CapP2PSignal })
	}
	if opts.UDPRelay && !opts.P2PDisabled {
		caps = append(caps, model.CapUDPRelay)
	}
	if opts.ResumeGrace <= 0 {
		opts.ResumeGrace = DefaultResumeGrace
	}
	creds := opts.Credentials
	if creds == nil {
		creds = map[string]model.DeviceCredential{}
	}
	return &Relay{
		peers:        make(map[string]*Peer),
		grants:       make(map[string]*fileGrant),
		monitors:     make(map[string]*monitorFeed),
		hosts:        hosts,
		sessions:     sessions,
		metrics:      m,
		credentials:  creds,
		sec:          security{devices: make(map[string]model.AllowedDevice)},
		securityRepo: opts.Security,
		caps:         caps,
		p2pDisabled:  opts.P2PDisabled,
		mergeMoves:   !opts.MoveCoalescingDisabled,
		resumeGrace:  opts.ResumeGrace,
		log:          log.With("component", "relay"),
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

// Register admits a new connection. It returns the new peer and, when it replaced an older connection with the
// same ID, the session partners of that connection, which the caller must notify.
func (r *Relay) Register(conn Conn, info PeerInfo, token string) (*Peer, []*Peer, error) {
	res, err := r.RegisterResume(conn, info, token, "")
	return res.Peer, res.Orphans, err
}

// RegisterResult describes a registration (see RegisterResume).
type RegisterResult struct {
	Peer *Peer
	// Orphans are partners of a replaced connection whose sessions ended; each must receive session-closed.
	Orphans []*Peer
	// Resumed lists the partners (clean IDs) whose sessions this connection took over, for registered-ack.
	Resumed []string
	// Back are the connected partners among them, to tell session-resumed; Waiting are the suspended ones, for
	// which the new connection receives session-suspended right after its ack.
	Back    []*Peer
	Waiting []string
}

// RegisterResume admits a new connection. With the resume token of the previous registration of the same ID
// (suspended within its grace or still connected) it takes over that registration's resumable sessions; the
// caller must then queue registered-ack and call Release. Otherwise an older connection is replaced as before.
func (r *Relay) RegisterResume(conn Conn, info PeerInfo, token, resumeToken string) (RegisterResult, error) {
	if !model.IsValidDeskID(info.CleanID) {
		return RegisterResult{}, ErrInvalidDeskID
	}
	auth := r.authenticate(info.CleanID, token, info.IsAdmin)
	if !auth.allowed {
		return RegisterResult{}, ErrUnauthorized
	}
	info.IsAdmin = auth.admin
	info.Caps = model.SanitizeCapabilities(info.Caps)
	peer := newPeer(info, model.FormatDeskID(info.CleanID), slices.Contains(info.Caps, model.CapMultiSession), conn, r.metrics)
	peer.EnrollToken = auth.token
	if peer.Resume {
		peer.ResumeToken = newResumeToken()
	}
	now := time.Now()

	r.mu.Lock()
	res := RegisterResult{Peer: peer}
	if old, ok := r.peers[peer.CleanID]; ok && old != peer {
		if canResume(old, peer, resumeToken, now) {
			r.takeOverLocked(old, peer, &res)
		} else {
			res.Orphans = r.detachPartnersLocked(old)
		}
		r.dropGrantsLocked(old, now)
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
	return res, nil
}

// Unregister removes p if it is still the current connection for its ID. It returns the partners to notify;
// a legacy partner named by sessionWith is notified even when it no longer points back. A session-resume peer
// may be suspended instead (see Leave); then removed is false.
func (r *Relay) Unregister(p *Peer) (removed bool, partners []*Peer) {
	res := r.Leave(p)
	return res.Removed, res.Closed
}

// LeaveResult describes what happened to a connection that ended (see Leave).
type LeaveResult struct {
	// Removed: the device went offline. Suspended: its sessions wait for a resume instead.
	Removed, Suspended bool
	// Closed are partners whose session ended (send session-closed); Waiting are partners whose session is
	// suspended (send session-suspended).
	Closed, Waiting []*Peer
}

// Leave runs when p's socket ended. If p is still current, its resumable sessions (see resume.go) are
// suspended for the grace and every other session ends; a peer left without a suspended session is removed.
func (r *Relay) Leave(p *Peer) LeaveResult {
	now := time.Now()
	var res LeaveResult
	r.mu.Lock()
	if r.peers[p.CleanID] != p || !p.suspendedUntil.IsZero() {
		r.mu.Unlock()
		return res
	}
	p.close()
	if p.Resume {
		var keep []string
		keep, res.Closed = r.splitLocked(p)
		if len(keep) > 0 {
			p.suspendedUntil, p.held = now.Add(r.resumeGrace), false
			for _, id := range keep {
				res.Waiting = append(res.Waiting, r.peers[id])
			}
			r.dropGrantsLocked(p, now)
			r.mu.Unlock()
			res.Suspended = true
			return res
		}
	}
	legacy := r.peers[p.sessionWith]
	partners := r.detachPartnersLocked(p)
	if legacy != nil && (len(partners) == 0 || partners[0] != legacy) {
		partners = append([]*Peer{legacy}, partners...)
	}
	res.Closed = append(res.Closed, partners...)
	r.dropGrantsLocked(p, now)
	delete(r.peers, p.CleanID)
	r.mu.Unlock()

	r.markGone(p.CleanID)
	res.Removed = true
	return res
}

// markGone records that a device went offline and its sessions ended.
func (r *Relay) markGone(clean string) {
	r.store("mark host offline", func(ctx context.Context) error { return r.hosts.MarkOffline(ctx, clean, time.Now()) })
	r.store("close peer sessions", func(ctx context.Context) error { return r.sessions.CloseForPeer(ctx, clean) })
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
