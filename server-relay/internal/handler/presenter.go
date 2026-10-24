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
