package usecase

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"slices"
	"strings"
	"time"

	"opendesk/server-relay/internal/model"
)

// Administrator registry management: deleting offline devices, and communities with groups of devices (see
// docs/features/communities.md at the repository root). Every method checks the administrator flag itself:
// the client hiding a button is not authorization. Storage is the source of truth; nothing is cached, since
// these change rarely and a community-list read is three small queries.

// HostDeletion is the outcome of DeleteHosts.
type HostDeletion struct {
	// Deleted counts the registry rows removed.
	Deleted int
	// Online are requested devices that are connected or reconnecting; they were not deleted.
	Online []string
}

// DeleteHosts removes offline devices from the registry. It is not a block: a deleted device that connects
// again registers as usual and is listed again. Connected (and suspended, reconnecting) devices are refused
// and reported in Online; the rest are deleted.
func (r *Relay) DeleteHosts(admin *Peer, cleanIDs []string) (HostDeletion, error) {
	if admin == nil || !admin.IsAdmin {
		return HostDeletion{}, ErrNotAdmin
	}
	ids, err := cleanIDList(cleanIDs)
	if err != nil {
		return HostDeletion{}, err
	}
	var res HostDeletion
	offline := make([]string, 0, len(ids))
	r.mu.RLock()
	for _, id := range ids {
		if _, live := r.peers[id]; live {
			res.Online = append(res.Online, id)
		} else {
			offline = append(offline, id)
		}
	}
	r.mu.RUnlock()
	if len(offline) == 0 {
		if len(res.Online) > 0 {
			return res, ErrDeviceOnline
		}
		return res, nil
	}
	ctx, cancel := context.WithTimeout(context.Background(), storageTimeout)
	defer cancel()
	// Live connections decide what is offline (the stored flag can lag behind them, see HostList).
	res.Deleted, err = r.hosts.Delete(ctx, offline)
	if err != nil {
		return res, err
	}
	// A device that registered between the check above and the delete would have lost the row its upsert
	// wrote: write it again, so a connected device never disappears from the list.
	for _, id := range offline {
		if p := r.Lookup(id); p != nil {
			if err := r.hosts.Upsert(ctx, model.Host{CleanID: p.CleanID, DeskID: p.DeskID, DeviceName: p.DeviceName,
				Platform: p.Platform, IsAdmin: p.IsAdmin, Online: true, LastSeen: time.Now()}); err != nil {
				r.log.Error("re-register host after delete failed", "id", p.DeskID, "err", err)
			}
		}
	}
	r.log.Info("devices deleted from registry", "count", res.Deleted, "refused_online", len(res.Online), "by", admin.DeskID)
	return res, nil
}

// CommunitiesEnabled reports whether community storage is configured.
func (r *Relay) CommunitiesEnabled() bool { return r.communities != nil }

// Communities returns every community for an administrator.
func (r *Relay) Communities(admin *Peer) ([]model.Community, error) {
	if err := r.communityAccess(admin); err != nil {
		return nil, err
	}
	return r.listCommunities()
}

// CommunityInput is what an administrator sends to create or edit a community.
type CommunityInput struct {
	Name, Icon, Color string
}

// CreateCommunity adds a community with no groups and returns its ID.
func (r *Relay) CreateCommunity(admin *Peer, in CommunityInput) (string, error) {
	if err := r.communityAccess(admin); err != nil {
		return "", err
	}
	in, err := cleanCommunityInput(in)
	if err != nil {
		return "", err
	}
	all, err := r.listCommunities()
	if err != nil {
		return "", err
	}
	if len(all) >= model.MaxCommunities {
		return "", ErrLimit
	}
	if communityNamed(all, in.Name, "") {
		return "", ErrNameTaken
	}
	c := model.Community{ID: newObjectID(), Name: in.Name, Icon: in.Icon, Color: in.Color, CreatedBy: admin.CleanID,
		CreatedAt: time.Now().UTC()}
	return c.ID, r.withStorage(func(ctx context.Context) error { return r.communities.CreateCommunity(ctx, c) })
}

// UpdateCommunity renames community id and replaces its icon and color.
func (r *Relay) UpdateCommunity(admin *Peer, id string, in CommunityInput) error {
	if err := r.communityAccess(admin); err != nil {
		return err
	}
	in, err := cleanCommunityInput(in)
	if err != nil {
		return err
	}
	all, err := r.listCommunities()
	if err != nil {
		return err
	}
	if findCommunity(all, id) == nil {
		return ErrNotFound
	}
	if communityNamed(all, in.Name, id) {
		return ErrNameTaken
	}
	return r.withStorage(func(ctx context.Context) error {
		return r.communities.UpdateCommunity(ctx, id, in.Name, in.Icon, in.Color)
	})
}

// DeleteCommunity removes community id with its groups. The devices stay registered.
func (r *Relay) DeleteCommunity(admin *Peer, id string) error {
	if err := r.communityAccess(admin); err != nil {
		return err
	}
	all, err := r.listCommunities()
	if err != nil {
		return err
	}
	if findCommunity(all, id) == nil {
		return ErrNotFound
	}
	return r.withStorage(func(ctx context.Context) error { return r.communities.DeleteCommunity(ctx, id) })
}

// CreateGroup adds an empty group to community communityID and returns its ID.
func (r *Relay) CreateGroup(admin *Peer, communityID, name string) (string, error) {
	if err := r.communityAccess(admin); err != nil {
		return "", err
	}
	name, ok := model.CleanName(name)
	if !ok {
		return "", ErrInvalidName
	}
	all, err := r.listCommunities()
	if err != nil {
		return "", err
	}
	c := findCommunity(all, communityID)
	if c == nil {
		return "", ErrNotFound
	}
	if len(c.Groups) >= model.MaxGroupsPerCommunity {
		return "", ErrLimit
	}
	if groupNamed(c, name, "") {
		return "", ErrNameTaken
	}
	g := model.Group{ID: newObjectID(), CommunityID: communityID, Name: name, CreatedAt: time.Now().UTC()}
	return g.ID, r.withStorage(func(ctx context.Context) error { return r.communities.CreateGroup(ctx, g) })
}

// RenameGroup names group id.
func (r *Relay) RenameGroup(admin *Peer, id, name string) error {
	if err := r.communityAccess(admin); err != nil {
		return err
	}
	name, ok := model.CleanName(name)
	if !ok {
		return ErrInvalidName
	}
	all, err := r.listCommunities()
	if err != nil {
		return err
	}
	c, _ := findGroup(all, id)
	if c == nil {
		return ErrNotFound
	}
	if groupNamed(c, name, id) {
		return ErrNameTaken
	}
	return r.withStorage(func(ctx context.Context) error { return r.communities.RenameGroup(ctx, id, name) })
}

// DeleteGroup removes group id. Its devices stay registered (and in All Devices).
func (r *Relay) DeleteGroup(admin *Peer, id string) error {
	if err := r.communityAccess(admin); err != nil {
		return err
	}
	all, err := r.listCommunities()
	if err != nil {
		return err
	}
	if c, _ := findGroup(all, id); c == nil {
		return ErrNotFound
	}
	return r.withStorage(func(ctx context.Context) error { return r.communities.DeleteGroup(ctx, id) })
}

// SetGroupMembers adds and removes devices of group id. Unregistered IDs in add are skipped by storage; an ID
// in both lists ends up removed.
func (r *Relay) SetGroupMembers(admin *Peer, id string, add, remove []string) error {
	if err := r.communityAccess(admin); err != nil {
		return err
	}
	if len(add)+len(remove) > model.MaxIDsPerRequest {
		return ErrLimit
	}
	addIDs, err := cleanIDList(add)
	if err != nil {
		return err
	}
	removeIDs, err := cleanIDList(remove)
	if err != nil {
		return err
	}
	all, err := r.listCommunities()
	if err != nil {
		return err
	}
	if c, _ := findGroup(all, id); c == nil {
		return ErrNotFound
	}
	return r.withStorage(func(ctx context.Context) error {
		if len(addIDs) > 0 {
			if err := r.communities.AddMembers(ctx, id, addIDs); err != nil {
				return err
			}
		}
		if len(removeIDs) > 0 {
			return r.communities.RemoveMembers(ctx, id, removeIDs)
		}
		return nil
	})
}

func (r *Relay) communityAccess(admin *Peer) error {
	if admin == nil || !admin.IsAdmin {
		return ErrNotAdmin
	}
	if r.communities == nil {
		return ErrUnsupported
	}
	return nil
}

func (r *Relay) listCommunities() ([]model.Community, error) {
	var out []model.Community
	err := r.withStorage(func(ctx context.Context) (err error) {
		out, err = r.communities.ListCommunities(ctx)
		return err
	})
	return out, err
}

// withStorage runs one repository call with the storage timeout and returns its error (unlike store, which
// only logs: an administrator's change must report failure).
func (r *Relay) withStorage(fn func(context.Context) error) error {
	ctx, cancel := context.WithTimeout(context.Background(), storageTimeout)
	defer cancel()
	return fn(ctx)
}

func cleanCommunityInput(in CommunityInput) (CommunityInput, error) {
	name, ok := model.CleanName(in.Name)
	in.Icon = strings.TrimSpace(in.Icon)
	if !ok || !model.ValidCommunityIcon(in.Icon) || !model.ValidCommunityColor(in.Color) {
		return in, ErrInvalidName
	}
	in.Name = name
	return in, nil
}

// cleanIDList validates and de-duplicates desk IDs, keeping their order.
func cleanIDList(raw []string) ([]string, error) {
	if len(raw) > model.MaxIDsPerRequest {
		return nil, ErrLimit
	}
	out := make([]string, 0, len(raw))
	for _, id := range raw {
		id = model.CleanDeskID(id)
		if !model.IsValidDeskID(id) {
			return nil, ErrInvalidDeskID
		}
		if !slices.Contains(out, id) {
			out = append(out, id)
		}
	}
	return out, nil
}

func findCommunity(all []model.Community, id string) *model.Community {
	for i := range all {
		if all[i].ID == id {
			return &all[i]
		}
	}
	return nil
}

// findGroup returns the community holding group id and the group, or nils.
func findGroup(all []model.Community, id string) (*model.Community, *model.Group) {
	for i := range all {
		for j := range all[i].Groups {
			if all[i].Groups[j].ID == id {
				return &all[i], &all[i].Groups[j]
			}
		}
	}
	return nil, nil
}

// communityNamed reports another community (not except) named name, ignoring case.
func communityNamed(all []model.Community, name, except string) bool {
	for _, c := range all {
		if c.ID != except && strings.EqualFold(c.Name, name) {
			return true
		}
	}
	return false
}

// groupNamed reports another group of c (not except) named name, ignoring case. Groups of different
// communities may share names ("Front desk" in every shop).
func groupNamed(c *model.Community, name, except string) bool {
	for _, g := range c.Groups {
		if g.ID != except && strings.EqualFold(g.Name, name) {
			return true
		}
	}
	return false
}

// newObjectID returns a random 16-hex-digit ID for a community or group.
func newObjectID() string {
	var raw [8]byte
	_, _ = rand.Read(raw[:])
	return hex.EncodeToString(raw[:])
}
