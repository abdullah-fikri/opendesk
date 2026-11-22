package model

import (
	"slices"
	"strings"
	"time"
	"unicode"
	"unicode/utf8"
)

// Relay capabilities of the administrator registry features. Declared here rather than in capability.go so the
// feature stays in one file.
const (
	// CapHostDelete: administrators can delete offline devices from the registry (host-delete).
	CapHostDelete = "host-delete"
	// CapCommunities: the relay stores communities and groups (community-* / group-* messages, community-list).
	CapCommunities = "communities"
)

// Community is an administrator-made collection of devices, split into groups (see docs/features/communities.md
// at the repository root). Communities are shared by every administrator of the relay.
type Community struct {
	ID   string
	Name string
	// Icon is a short emoji shown on the community's tile; empty shows the default mark.
	Icon string
	// Color is one of CommunityColors; empty is the default tint.
	Color     string
	CreatedBy string
	CreatedAt time.Time
	// Groups are ordered by creation.
	Groups []Group
}

// Group is one group of a community. A device may be a member of any number of groups.
type Group struct {
	ID          string
	CommunityID string
	Name        string
	CreatedAt   time.Time
	// Members are clean desk IDs of registered devices, sorted.
	Members []string
}

// Limits on communities. They keep one community-list message small and stop a runaway client from creating
// thousands of rows.
const (
	MaxCommunityNameLength = 48
	MaxCommunityIconBytes  = 32
	MaxCommunityIconRunes  = 8
	MaxCommunities         = 64
	MaxGroupsPerCommunity  = 64
	// MaxIDsPerRequest bounds the device IDs one host-delete or group-members message may name.
	MaxIDsPerRequest = 500
)

// CommunityColors are the tints clients know; anything else is refused so every client can draw it.
var CommunityColors = []string{"red", "orange", "yellow", "green", "teal", "blue", "indigo", "purple", "pink", "graphite"}

// CleanName trims a community or group name and drops control characters. ok is false when the result is
// empty or longer than MaxCommunityNameLength runes.
func CleanName(raw string) (name string, ok bool) {
	name = strings.TrimSpace(strings.Map(func(r rune) rune {
		if unicode.IsControl(r) {
			return -1
		}
		return r
	}, raw))
	n := utf8.RuneCountInString(name)
	return name, n > 0 && n <= MaxCommunityNameLength
}

// ValidCommunityIcon accepts an empty icon or a short run of printable characters without ASCII letters or
// digits (an emoji with its modifiers), so the field cannot carry a second name.
func ValidCommunityIcon(icon string) bool {
	if icon == "" {
		return true
	}
	if len(icon) > MaxCommunityIconBytes || utf8.RuneCountInString(icon) > MaxCommunityIconRunes || !utf8.ValidString(icon) {
		return false
	}
	for _, r := range icon {
		if unicode.IsControl(r) || unicode.IsSpace(r) || r < 0x80 && (unicode.IsLetter(r) || unicode.IsDigit(r)) {
			return false
		}
	}
	return true
}

// ValidCommunityColor accepts an empty color or one of CommunityColors.
func ValidCommunityColor(color string) bool {
	return color == "" || slices.Contains(CommunityColors, color)
}
