// Package memory implements the host registry and community storage ports in memory, with the same semantics
// as the PostgreSQL repositories (deleting a host drops its group memberships, adding an unknown device to a
// group is skipped). Tests use it; it never backs a production relay.
package memory

import (
	"context"
	"slices"
	"sort"
	"sync"
	"time"

	"opendesk/server-relay/internal/model"
)

// Store is a host registry and community store. The zero value is not usable; call New.
type Store struct {
	mu          sync.Mutex
	hosts       map[string]model.Host
	communities []model.Community
}

// New returns an empty store.
func New() *Store {
	return &Store{hosts: map[string]model.Host{}}
}

// Upsert inserts or updates a host; empty name, platform or address keep the stored value, the alias is kept.
func (s *Store) Upsert(_ context.Context, h model.Host) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if old, ok := s.hosts[h.CleanID]; ok {
		h.Alias = old.Alias
		if h.DeviceName == "" {
			h.DeviceName = old.DeviceName
		}
		if h.Platform == "" {
			h.Platform = old.Platform
		}
		if h.RemoteAddr == "" {
			h.RemoteAddr = old.RemoteAddr
		}
	}
	s.hosts[h.CleanID] = h
	return nil
}

// MarkOffline flags one host offline.
func (s *Store) MarkOffline(_ context.Context, cleanID string, at time.Time) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if h, ok := s.hosts[cleanID]; ok {
		h.Online, h.LastSeen = false, at
		s.hosts[cleanID] = h
	}
	return nil
}

// MarkAllOffline flags every host offline.
func (s *Store) MarkAllOffline(_ context.Context, at time.Time) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	for id, h := range s.hosts {
		if h.Online {
			h.Online, h.LastSeen = false, at
			s.hosts[id] = h
		}
	}
	return nil
}

// List returns every host, online first, then by device name.
func (s *Store) List(context.Context) ([]model.Host, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]model.Host, 0, len(s.hosts))
	for _, h := range s.hosts {
		out = append(out, h)
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].Online != out[j].Online {
			return out[i].Online
		}
		return out[i].DeviceName < out[j].DeviceName
	})
	return out, nil
}

// SetAlias names a host.
func (s *Store) SetAlias(_ context.Context, cleanID, alias string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if h, ok := s.hosts[cleanID]; ok {
		h.Alias = alias
		s.hosts[cleanID] = h
	}
	return nil
}

// DeleteOffline removes the offline hosts among cleanIDs with their group memberships.
func (s *Store) DeleteOffline(_ context.Context, cleanIDs []string) (int, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	n := 0
	for _, id := range cleanIDs {
		if h, ok := s.hosts[id]; ok && !h.Online {
			delete(s.hosts, id)
			n++
			s.eachGroup(func(g *model.Group) {
				g.Members = slices.DeleteFunc(g.Members, func(m string) bool { return m == id })
			})
		}
	}
	return n, nil
}

// ListCommunities returns a deep copy of every community.
func (s *Store) ListCommunities(context.Context) ([]model.Community, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]model.Community, len(s.communities))
	for i, c := range s.communities {
		c.Groups = slices.Clone(c.Groups)
		for j := range c.Groups {
			c.Groups[j].Members = slices.Clone(c.Groups[j].Members)
			if c.Groups[j].Members == nil {
				c.Groups[j].Members = []string{}
			}
		}
		out[i] = c
	}
	return out, nil
}

// CreateCommunity appends c.
func (s *Store) CreateCommunity(_ context.Context, c model.Community) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	c.Groups = nil
	s.communities = append(s.communities, c)
	return nil
}

// UpdateCommunity replaces the name, icon and color of community id.
func (s *Store) UpdateCommunity(_ context.Context, id, name, icon, color string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	for i := range s.communities {
		if s.communities[i].ID == id {
			s.communities[i].Name, s.communities[i].Icon, s.communities[i].Color = name, icon, color
		}
	}
	return nil
}

// DeleteCommunity removes community id with its groups.
func (s *Store) DeleteCommunity(_ context.Context, id string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.communities = slices.DeleteFunc(s.communities, func(c model.Community) bool { return c.ID == id })
	return nil
}

// CreateGroup appends g to its community.
func (s *Store) CreateGroup(_ context.Context, g model.Group) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	for i := range s.communities {
		if s.communities[i].ID == g.CommunityID {
			g.Members = []string{}
			s.communities[i].Groups = append(s.communities[i].Groups, g)
		}
	}
	return nil
}

// RenameGroup names group id.
func (s *Store) RenameGroup(_ context.Context, id, name string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.eachGroup(func(g *model.Group) {
		if g.ID == id {
			g.Name = name
		}
	})
	return nil
}

// DeleteGroup removes group id.
func (s *Store) DeleteGroup(_ context.Context, id string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	for i := range s.communities {
		s.communities[i].Groups = slices.DeleteFunc(s.communities[i].Groups, func(g model.Group) bool { return g.ID == id })
	}
	return nil
}

// AddMembers adds the registered hosts among cleanIDs to group id.
func (s *Store) AddMembers(_ context.Context, id string, cleanIDs []string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.eachGroup(func(g *model.Group) {
		if g.ID != id {
			return
		}
		for _, c := range cleanIDs {
			if _, known := s.hosts[c]; known && !slices.Contains(g.Members, c) {
				g.Members = append(g.Members, c)
			}
		}
		slices.Sort(g.Members)
	})
	return nil
}

// RemoveMembers takes cleanIDs out of group id.
func (s *Store) RemoveMembers(_ context.Context, id string, cleanIDs []string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.eachGroup(func(g *model.Group) {
		if g.ID == id {
			g.Members = slices.DeleteFunc(g.Members, func(m string) bool { return slices.Contains(cleanIDs, m) })
		}
	})
	return nil
}

// eachGroup calls fn with every group. Caller holds mu.
func (s *Store) eachGroup(fn func(*model.Group)) {
	for i := range s.communities {
		for j := range s.communities[i].Groups {
			fn(&s.communities[i].Groups[j])
		}
	}
}
