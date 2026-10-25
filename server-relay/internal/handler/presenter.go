package handler

import (
	"fmt"
	"time"

	"opendesk/server-relay/internal/dto"
	"opendesk/server-relay/internal/model"
	"opendesk/server-relay/internal/usecase"
)

// This file maps domain values to outbound DTOs.

// sessionConnected tells the receiver that its session with other is live; role is the receiver's role.
func sessionConnected(other *usecase.Peer, role model.Role) dto.SessionConnected {
	return dto.SessionConnected{
		Type:        dto.TypeSessionConnected,
		TargetID:    other.DeskID,
		TargetClean: other.CleanID,
		Role:        string(role),
		PeerCaps:    other.Caps,
	}
}

// sessionClosed names the ended session (other's ID) for multi-session viewers only, so single-session
// clients keep receiving the message shape they understand.
func sessionClosed(rcpt *usecase.Peer, other, message string) dto.SessionClosed {
	m := dto.SessionClosed{Type: dto.TypeSessionClosed, Message: message}
	if rcpt.Multi {
		m.TargetClean = other
	}
	return m
}

// sourceFor returns the sender ID to stamp on a relayed message: only multi-session viewers need it.
func sourceFor(rcpt, sender *usecase.Peer) string {
	if rcpt.Multi {
		return sender.CleanID
	}
	return ""
}

func targetUnavailableMessage(clean string) string {
	return fmt.Sprintf(msgTargetUnavailable, model.FormatDeskID(clean))
}

func hostListDTO(hosts []model.Host) dto.HostList {
	out := make([]dto.Host, len(hosts))
	for i, h := range hosts {
		out[i] = dto.Host{
			CleanID:    h.CleanID,
			DeskID:     h.DeskID,
			DeviceName: h.DeviceName,
			Alias:      h.Alias,
			Platform:   h.Platform,
			Online:     h.Online,
			LastSeen:   h.LastSeen.UTC().Format(time.RFC3339),
		}
	}
	return dto.HostList{Type: dto.TypeHostList, Hosts: out}
}

// sendMonitorUpdate delivers the monitor-wall messages a relay change produced.
func sendMonitorUpdate(up usecase.MonitorUpdate) {
	for _, host := range up.Start {
		_ = host.Send(dto.MonitorStart{Type: dto.TypeMonitorStart, MaxWidth: model.MonitorMaxWidth, FPS: model.MonitorFPS})
	}
	for _, host := range up.Stop {
		_ = host.Send(dto.MonitorStop{Type: dto.TypeMonitorStop})
	}
	for _, n := range up.Notices {
		_ = n.Admin.Send(dto.MonitorState{Type: dto.TypeMonitorState, TargetClean: n.Host, State: string(n.State)})
	}
}

// securityStateDTO joins the allowlist with the registry for names and presence.
func securityStateDTO(view usecase.SecurityView, hosts []model.Host, online func(string) bool) dto.SecurityState {
	names := make(map[string]string, len(hosts))
	for _, h := range hosts {
		names[h.CleanID] = h.DeviceName
		if h.Alias != "" {
			names[h.CleanID] = h.Alias
		}
	}
	out := dto.SecurityState{Type: dto.TypeSecurityState, Enabled: view.Enforced, ManagedByEnv: view.ManagedByEnv,
		Devices: make([]dto.SecurityDevice, len(view.Devices))}
	for i, d := range view.Devices {
		enrolledAt := ""
		if !d.EnrolledAt.IsZero() {
			enrolledAt = d.EnrolledAt.UTC().Format(time.RFC3339)
		}
		out.Devices[i] = dto.SecurityDevice{
			CleanID: d.CleanID, DeskID: model.FormatDeskID(d.CleanID), Name: names[d.CleanID], Admin: d.Admin,
			Enrolled: d.Enrolled(), EnrolledAt: enrolledAt, Online: online(d.CleanID),
		}
	}
	return out
}
