// Package handler is the relay's transport layer: HTTP endpoints and the WebSocket protocol. It decodes
// requests into DTOs, calls the usecase layer and encodes its results; it holds no business rules.
package handler

import (
	"context"
	"crypto/subtle"
	"encoding/json"
	"log/slog"
	"net/http"
	"sync"
	"time"

	"github.com/gorilla/websocket"

	"opendesk/server-relay/internal/dto"
	"opendesk/server-relay/internal/metrics"
	"opendesk/server-relay/internal/model"
	"opendesk/server-relay/internal/usecase"
	"opendesk/server-relay/internal/version"
)

// Options configures the transport.
type Options struct {
	ReadBufferSize  int
	WriteBufferSize int
	PingInterval    time.Duration
	// MetricsToken protects /metrics; fewer than 32 characters disables the endpoint.
	MetricsToken string
	// TLSProxy accepts plaintext WebSocket from non-loopback addresses (TLS ends at a proxy in front).
	TLSProxy bool
}

// Handler serves every relay endpoint.
type Handler struct {
	relay    *usecase.Relay
	metrics  *metrics.Collector
	opts     Options
	upgrader websocket.Upgrader
	started  time.Time
	log      *slog.Logger
}

// New returns a Handler. Call RunPinger to start WebSocket keep-alives.
func New(relay *usecase.Relay, m *metrics.Collector, opts Options, log *slog.Logger) *Handler {
	return &Handler{
		relay:   relay,
		metrics: m,
		opts:    opts,
		started: time.Now(),
		log:     log.With("component", "handler"),
		upgrader: websocket.Upgrader{
			ReadBufferSize:  opts.ReadBufferSize,
			WriteBufferSize: opts.WriteBufferSize,
			// Shared write buffers: an idle or slow peer holds none, so memory follows active traffic only.
			WriteBufferPool:   &sync.Pool{},
			EnableCompression: false,
			// Clients are native apps without an Origin; access control is the device credential, not the origin.
			CheckOrigin: func(*http.Request) bool { return true },
		},
	}
}

// ServeHTTP routes WebSocket upgrades to the relay protocol and plain requests to the HTTP endpoints.
func (h *Handler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if websocket.IsWebSocketUpgrade(r) {
		h.serveWebSocket(w, r)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	switch r.URL.Path {
	case "/healthz":
		_ = json.NewEncoder(w).Encode(dto.Health{Status: "ok"})
	case "/metrics":
		h.serveMetrics(w, r)
	default:
		h.serveStatus(w)
	}
}

func (h *Handler) serveStatus(w http.ResponseWriter) {
	w.Header().Set("Access-Control-Allow-Origin", "*")
	_ = json.NewEncoder(w).Encode(dto.Status{
		Service:     "opendesk.relay",
		Version:     version.Version,
		Status:      "online",
		Storage:     "postgresql",
		ActivePeers: h.relay.PeerCount(),
		Uptime:      int(time.Since(h.started).Seconds()),
	})
}

// serveMetrics requires a bearer token: loopback is not proof of trust behind a local TLS terminator.
func (h *Handler) serveMetrics(w http.ResponseWriter, r *http.Request) {
	token := h.opts.MetricsToken
	given := r.Header.Get("Authorization")
	if len(token) < model.MinDeviceTokenLength || subtle.ConstantTimeCompare([]byte(given), []byte("Bearer "+token)) != 1 {
		http.Error(w, `{"error":"unauthorized"}`, http.StatusUnauthorized)
		return
	}
	_ = json.NewEncoder(w).Encode(h.metrics.Snapshot(h.relay.PeerCount()))
}

// RunPinger sends a WebSocket ping to every peer each interval until ctx ends. Peers that stop answering
// hit their read deadline and are disconnected.
func (h *Handler) RunPinger(ctx context.Context) {
	ticker := time.NewTicker(h.opts.PingInterval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			for _, p := range h.relay.Peers() {
				_ = p.Ping()
			}
		}
	}
}
