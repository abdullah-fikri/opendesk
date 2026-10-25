package usecase

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/hex"
	"slices"
	"sync"
	"time"

	"opendesk/server-relay/internal/model"
)

// Device security: administrators keep an allowlist of desk IDs from their client. While it is enforced, only
// listed devices may register. A listed device that has no token yet receives one the first time it connects
// (trust on first use) and must present it from then on, so clients need no configuration besides the relay
// URL. Credentials from OPENDESK_DEVICE_CREDENTIALS keep working and take precedence over the allowlist.

// security is the in-memory allowlist. Storage is write-through; reads never touch the database.
type security struct {
	mu       sync.Mutex
	enforced bool
	devices  map[string]model.AllowedDevice
}

// authResult is the outcome of authenticate. Token is set when the device was just enrolled and must be
// sent to it.
type authResult struct {
	allowed, admin bool
	token          string
}

// SecurityChange lists what the caller must deliver after an allowlist change.
type SecurityChange struct {
	// Issued are connected devices that just enrolled; each must receive its token.
	Issued []IssuedToken
	// Kicked are connected devices no longer admitted; each must be told and disconnected.
	Kicked []*Peer
}

// IssuedToken is a fresh enrollment token for a connected peer.
type IssuedToken struct {
	Peer  *Peer
	Token string
}

// SecurityView is what administrators see on the security page.
type SecurityView struct {
	Enforced bool
	// ManagedByEnv is set when OPENDESK_DEVICE_CREDENTIALS is configured; those devices always need their token.
	ManagedByEnv bool
	Devices      []model.AllowedDevice
}

// LoadSecurity reads the allowlist from storage; call once at startup.
func (r *Relay) LoadSecurity(ctx context.Context) error {
	if r.securityRepo == nil {
		return nil
	}
	ctx, cancel := context.WithTimeout(ctx, storageTimeout)
	defer cancel()
	state, err := r.securityRepo.LoadSecurity(ctx)
	if err != nil {
		return err
	}
	r.sec.mu.Lock()
	defer r.sec.mu.Unlock()
	r.sec.enforced = state.Enforced
	r.sec.devices = make(map[string]model.AllowedDevice, len(state.Devices))
	for _, d := range state.Devices {
		r.sec.devices[d.CleanID] = d
	}
	r.log.Info("device allowlist loaded", "enforced", state.Enforced, "devices", len(state.Devices))
	return nil
}

// authenticate decides whether a registration is admitted and with which role.
//
//   - A device listed in OPENDESK_DEVICE_CREDENTIALS needs its exact token and gets the configured role.
//   - Otherwise, with neither those credentials nor the allowlist enforced, every device is admitted with its
//     claimed role (the open default).
//   - Otherwise only allowlisted devices are admitted: an unenrolled one gets a new token, an enrolled one must
//     present the token it was given.
func (r *Relay) authenticate(clean, token string, claimedAdmin bool) authResult {
	if cred, ok := r.credentials[clean]; ok {
		if len(cred.Token) < model.MinDeviceTokenLength || len(token) != len(cred.Token) ||
			subtle.ConstantTimeCompare([]byte(token), []byte(cred.Token)) != 1 {
			return authResult{}
		}
		return authResult{allowed: true, admin: cred.Admin}
	}
	r.sec.mu.Lock()
	defer r.sec.mu.Unlock()
	if len(r.credentials) == 0 && !r.sec.enforced {
		return authResult{allowed: true, admin: claimedAdmin}
	}
	dev, ok := r.sec.devices[clean]
	if !ok {
		return authResult{}
	}
	if !dev.Enrolled() {
		issued := r.enrollLocked(&dev)
		return authResult{allowed: true, admin: dev.Admin, token: issued}
	}
	sum := sha256.Sum256([]byte(token))
	if token == "" || subtle.ConstantTimeCompare([]byte(hex.EncodeToString(sum[:])), []byte(dev.TokenHash)) != 1 {
		return authResult{}
	}
	return authResult{allowed: true, admin: dev.Admin}
}

// enrollLocked gives dev a fresh token, stores its hash and returns the token. Caller holds sec.mu.
func (r *Relay) enrollLocked(dev *model.AllowedDevice) string {
	var raw [32]byte
	_, _ = rand.Read(raw[:])
	token := hex.EncodeToString(raw[:])
	sum := sha256.Sum256([]byte(token))
	dev.TokenHash = hex.EncodeToString(sum[:])
	dev.EnrolledAt = time.Now().UTC()
	r.sec.devices[dev.CleanID] = *dev
	r.storeSecurity("enroll device", func(ctx context.Context, repo SecurityRepository) error {
		return repo.PutAllowedDevice(ctx, *dev)
	})
	r.log.Info("device enrolled", "id", model.FormatDeskID(dev.CleanID), "admin", dev.Admin)
	return token
}

// Security returns the allowlist for an administrator.
func (r *Relay) Security(admin *Peer) (SecurityView, error) {
	if admin == nil || !admin.IsAdmin {
		return SecurityView{}, ErrNotAdmin
	}
	r.sec.mu.Lock()
	defer r.sec.mu.Unlock()
	view := SecurityView{Enforced: r.sec.enforced, ManagedByEnv: len(r.credentials) > 0}
	for _, d := range r.sec.devices {
		view.Devices = append(view.Devices, d)
	}
	slices.SortFunc(view.Devices, func(a, b model.AllowedDevice) int { return a.CreatedAt.Compare(b.CreatedAt) })
	return view, nil
}

// SetEnforced turns the allowlist on or off. Turning it on lists the administrator who did it (so it cannot
// lock itself out), enrolls every connected listed device and disconnects every connected unlisted one.
func (r *Relay) SetEnforced(admin *Peer, on bool) (SecurityChange, error) {
	if admin == nil || !admin.IsAdmin {
		return SecurityChange{}, ErrNotAdmin
	}
	r.sec.mu.Lock()
	defer r.sec.mu.Unlock()
	r.sec.enforced = on
	r.storeSecurity("set allowlist switch", func(ctx context.Context, repo SecurityRepository) error {
		return repo.SetEnforced(ctx, on)
	})
	r.log.Info("device allowlist switched", "enforced", on, "by", admin.DeskID)
	if !on {
		return SecurityChange{}, nil
	}
	if _, ok := r.sec.devices[admin.CleanID]; !ok {
		r.putDeviceLocked(model.AllowedDevice{CleanID: admin.CleanID, Admin: true, AddedBy: admin.CleanID})
	}
	return r.reconcileLocked(), nil
}

// AllowDevice lists clean (replacing its role when it is already listed). A connected device enrolls at once.
func (r *Relay) AllowDevice(admin *Peer, clean string, isAdmin bool) (SecurityChange, error) {
	if admin == nil || !admin.IsAdmin {
		return SecurityChange{}, ErrNotAdmin
	}
	if !model.IsValidDeskID(clean) {
		return SecurityChange{}, ErrInvalidDeskID
	}
	r.sec.mu.Lock()
	defer r.sec.mu.Unlock()
	dev, ok := r.sec.devices[clean]
	if !ok {
		dev = model.AllowedDevice{CleanID: clean, AddedBy: admin.CleanID}
	}
	dev.Admin = isAdmin
	r.putDeviceLocked(dev)
	return r.reconcileLocked(), nil
}

// AllowConnected lists every device connected right now, so an administrator can turn the allowlist on
// without typing IDs. Administrators keep their role.
func (r *Relay) AllowConnected(admin *Peer) (SecurityChange, error) {
	if admin == nil || !admin.IsAdmin {
		return SecurityChange{}, ErrNotAdmin
	}
	peers := r.Peers()
	r.sec.mu.Lock()
	defer r.sec.mu.Unlock()
	for _, p := range peers {
		if _, ok := r.sec.devices[p.CleanID]; !ok {
			r.putDeviceLocked(model.AllowedDevice{CleanID: p.CleanID, Admin: p.IsAdmin, AddedBy: admin.CleanID})
		}
	}
	return r.reconcileLocked(), nil
}

// RemoveDevice takes clean off the allowlist; while it is enforced the device is disconnected.
func (r *Relay) RemoveDevice(admin *Peer, clean string) (SecurityChange, error) {
	if admin == nil || !admin.IsAdmin {
		return SecurityChange{}, ErrNotAdmin
	}
	r.sec.mu.Lock()
	defer r.sec.mu.Unlock()
	if _, ok := r.sec.devices[clean]; !ok {
		return SecurityChange{}, nil
	}
	delete(r.sec.devices, clean)
	r.storeSecurity("delete allowed device", func(ctx context.Context, repo SecurityRepository) error {
		return repo.DeleteAllowedDevice(ctx, clean)
	})
	r.log.Info("device removed from allowlist", "id", model.FormatDeskID(clean), "by", admin.DeskID)
	return r.reconcileLocked(), nil
}

// ResetDevice forgets clean's token: a reinstalled device (which lost its token) can enroll again. A connected
// device receives its new token at once.
func (r *Relay) ResetDevice(admin *Peer, clean string) (SecurityChange, error) {
	if admin == nil || !admin.IsAdmin {
		return SecurityChange{}, ErrNotAdmin
	}
	r.sec.mu.Lock()
	defer r.sec.mu.Unlock()
	dev, ok := r.sec.devices[clean]
	if !ok {
		return SecurityChange{}, nil
	}
	dev.TokenHash, dev.EnrolledAt = "", time.Time{}
	r.putDeviceLocked(dev)
	r.log.Info("device token reset", "id", model.FormatDeskID(clean), "by", admin.DeskID)
	return r.reconcileLocked(), nil
}

// putDeviceLocked stores dev in memory and in storage. Caller holds sec.mu.
func (r *Relay) putDeviceLocked(dev model.AllowedDevice) {
	if dev.CreatedAt.IsZero() {
		dev.CreatedAt = time.Now().UTC()
	}
	r.sec.devices[dev.CleanID] = dev
	r.storeSecurity("store allowed device", func(ctx context.Context, repo SecurityRepository) error {
		return repo.PutAllowedDevice(ctx, dev)
	})
}

// reconcileLocked applies the allowlist to the connected devices: listed ones without a token enroll, unlisted
// ones are disconnected. Devices with OPENDESK_DEVICE_CREDENTIALS are left alone. Caller holds sec.mu.
func (r *Relay) reconcileLocked() SecurityChange {
	var change SecurityChange
	if !r.sec.enforced && len(r.credentials) == 0 {
		return change
	}
	for _, p := range r.Peers() {
		if _, ok := r.credentials[p.CleanID]; ok {
			continue
		}
		dev, listed := r.sec.devices[p.CleanID]
		switch {
		case !listed:
			change.Kicked = append(change.Kicked, p)
		case !dev.Enrolled():
			change.Issued = append(change.Issued, IssuedToken{Peer: p, Token: r.enrollLocked(&dev)})
		}
	}
	return change
}

// storeSecurity runs one allowlist write when storage is configured.
func (r *Relay) storeSecurity(op string, fn func(context.Context, SecurityRepository) error) {
	if r.securityRepo == nil {
		return
	}
	r.store(op, func(ctx context.Context) error { return fn(ctx, r.securityRepo) })
}

// SetAlias names a registered device for every administrator. Names are trimmed to model.MaxAliasLength runes.
func (r *Relay) SetAlias(admin *Peer, clean, alias string) error {
	if admin == nil || !admin.IsAdmin {
		return ErrNotAdmin
	}
	if !model.IsValidDeskID(clean) {
		return ErrInvalidDeskID
	}
	if runes := []rune(alias); len(runes) > model.MaxAliasLength {
		alias = string(runes[:model.MaxAliasLength])
	}
	ctx, cancel := context.WithTimeout(context.Background(), storageTimeout)
	defer cancel()
	return r.hosts.SetAlias(ctx, clean, alias)
}
