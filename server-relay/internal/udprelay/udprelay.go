// Package udprelay forwards a session's direct-path UDP datagrams between its two clients when they cannot reach
// each other (NATs that drop unsolicited packets). Each client only ever talks to the relay, which every NAT
// allows. Datagrams are end-to-end encrypted by the clients (see client-desktop/docs/features/p2p.md); the relay
// moves them unread, one to one, and only between the two addresses that bound with the session's tokens.
//
// Wire format (client <-> relay):
//
//	[0x52][0x01][token 16B]   bind: the sender's address becomes this token's leg
//	[0x52][0x02]              bound: the relay's answer to a bind
//	[0x4F]...                 a P2P datagram, forwarded to the other leg once both are bound
package udprelay

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"log/slog"
	"net"
	"net/netip"
	"sync"
	"time"
)

const (
	tagControl  = 0x52
	ctlBind     = 0x01
	ctlBound    = 0x02
	tagP2P      = 0x4F
	tokenLen    = 16
	maxDatagram = 1500
	// unboundTTL: an allocation nobody bound in this time is dropped (the session never used it).
	unboundTTL = 60 * time.Second
	// idleTTL: a pair without traffic for this long is dropped; clients rebind every 15 s.
	idleTTL    = 60 * time.Second
	sweepEvery = 10 * time.Second
	// burstSeconds of the per-pair rate may go out at once (a key frame).
	burstSeconds = 0.25
)

// Token identifies one side (leg) of an allocation.
type Token [tokenLen]byte

// String is the hex form sent to clients in udp-relay-offer.
func (t Token) String() string { return hex.EncodeToString(t[:]) }

type leg struct {
	token Token
	addr  netip.AddrPort // zero until bound
	pair  *pair
	other *leg
}

type pair struct {
	key     string
	legs    [2]*leg
	created time.Time
	last    time.Time // last forwarded datagram or bind
	// Token bucket in bytes.
	tokens float64
	at     time.Time
	// Counters for the release log.
	forwarded, dropped uint64
}

// Server is the UDP forwarder. All methods are safe for concurrent use.
type Server struct {
	conn        *net.UDPConn
	bytesPerSec float64
	log         *slog.Logger
	mu          sync.Mutex
	pairs       map[string]*pair
	byToken     map[Token]*leg
	byAddr      map[netip.AddrPort]*leg
	now         func() time.Time
	writeTo     func([]byte, netip.AddrPort)
}

// Listen opens the UDP socket. mbps caps each pair's forwarded rate (both directions together).
func Listen(addr string, mbps int, log *slog.Logger) (*Server, error) {
	ua, err := net.ResolveUDPAddr("udp", addr)
	if err != nil {
		return nil, err
	}
	conn, err := net.ListenUDP("udp", ua)
	if err != nil {
		return nil, err
	}
	_ = conn.SetReadBuffer(4 << 20)
	_ = conn.SetWriteBuffer(4 << 20)
	s := newServer(mbps, log)
	s.conn = conn
	s.writeTo = func(b []byte, to netip.AddrPort) { _, _ = conn.WriteToUDPAddrPort(b, to) }
	return s, nil
}

func newServer(mbps int, log *slog.Logger) *Server {
	return &Server{
		bytesPerSec: float64(mbps) * 1e6 / 8,
		log:         log.With("component", "udprelay"),
		pairs:       make(map[string]*pair),
		byToken:     make(map[Token]*leg),
		byAddr:      make(map[netip.AddrPort]*leg),
		now:         time.Now,
	}
}

// Run reads datagrams until ctx ends.
func (s *Server) Run(ctx context.Context) {
	go func() {
		<-ctx.Done()
		_ = s.conn.Close()
	}()
	go s.sweepLoop(ctx)
	buf := make([]byte, maxDatagram)
	for {
		n, from, err := s.conn.ReadFromUDPAddrPort(buf)
		if err != nil {
			if errors.Is(err, net.ErrClosed) || ctx.Err() != nil {
				return
			}
			continue
		}
		s.handle(buf[:n], from)
	}
}

// PairKey is the allocation key of a session: both clean IDs, order-independent.
func PairKey(a, b string) string {
	if a > b {
		a, b = b, a
	}
	return a + "|" + b
}

// Allocate creates (or replaces) the allocation for key and returns one token per side.
func (s *Server) Allocate(key string) (Token, Token, error) {
	var a, b Token
	if _, err := rand.Read(a[:]); err != nil {
		return a, b, err
	}
	if _, err := rand.Read(b[:]); err != nil {
		return a, b, err
	}
	now := s.now()
	p := &pair{key: key, created: now, last: now, tokens: s.bytesPerSec * burstSeconds, at: now}
	la, lb := &leg{token: a, pair: p}, &leg{token: b, pair: p}
	la.other, lb.other = lb, la
	p.legs = [2]*leg{la, lb}
	s.mu.Lock()
	s.releaseLocked(key)
	s.pairs[key] = p
	s.byToken[a], s.byToken[b] = la, lb
	s.mu.Unlock()
	return a, b, nil
}

// Release drops the allocation for key (the session ended); unknown keys are ignored.
func (s *Server) Release(key string) {
	s.mu.Lock()
	s.releaseLocked(key)
	s.mu.Unlock()
}

func (s *Server) releaseLocked(key string) {
	p := s.pairs[key]
	if p == nil {
		return
	}
	for _, l := range p.legs {
		delete(s.byToken, l.token)
		if l.addr.IsValid() && s.byAddr[l.addr] == l {
			delete(s.byAddr, l.addr)
		}
	}
	delete(s.pairs, key)
	if p.forwarded > 0 || p.dropped > 0 {
		s.log.Info("pair released", "forwarded", p.forwarded, "dropped", p.dropped,
			"age", s.now().Sub(p.created).Round(time.Second))
	}
}

func (s *Server) handle(pkt []byte, from netip.AddrPort) {
	if len(pkt) == 0 {
		return
	}
	from = netip.AddrPortFrom(from.Addr().Unmap(), from.Port())
	switch pkt[0] {
	case tagControl:
		if len(pkt) == 2+tokenLen && pkt[1] == ctlBind {
			s.bind(Token(pkt[2:]), from)
		}
	case tagP2P:
		s.forward(pkt, from)
	}
}

func (s *Server) bind(tok Token, from netip.AddrPort) {
	s.mu.Lock()
	l := s.byToken[tok]
	if l == nil {
		s.mu.Unlock()
		return
	}
	if old := s.byAddr[from]; old != nil && old != l {
		// The address belonged to another leg (a stale allocation): the token proves the new owner.
		old.addr = netip.AddrPort{}
	}
	if l.addr.IsValid() && l.addr != from && s.byAddr[l.addr] == l {
		delete(s.byAddr, l.addr) // NAT rebinding: the leg moves
	}
	l.addr = from
	s.byAddr[from] = l
	l.pair.last = s.now()
	s.mu.Unlock()
	s.writeTo([]byte{tagControl, ctlBound}, from)
}

func (s *Server) forward(pkt []byte, from netip.AddrPort) {
	s.mu.Lock()
	l := s.byAddr[from]
	if l == nil || !l.other.addr.IsValid() {
		s.mu.Unlock()
		return
	}
	p := l.pair
	now := s.now()
	p.tokens = min(s.bytesPerSec*burstSeconds, p.tokens+now.Sub(p.at).Seconds()*s.bytesPerSec)
	p.at = now
	if p.tokens < float64(len(pkt)) {
		p.dropped++
		s.mu.Unlock()
		return
	}
	p.tokens -= float64(len(pkt))
	p.forwarded++
	p.last = now
	to := l.other.addr
	s.mu.Unlock()
	s.writeTo(pkt, to)
}

func (s *Server) sweepLoop(ctx context.Context) {
	t := time.NewTicker(sweepEvery)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
			s.sweep()
		}
	}
}

// sweep drops allocations nobody bound and pairs that went quiet.
func (s *Server) sweep() {
	now := s.now()
	s.mu.Lock()
	defer s.mu.Unlock()
	for key, p := range s.pairs {
		bound := p.legs[0].addr.IsValid() || p.legs[1].addr.IsValid()
		if (!bound && now.Sub(p.created) > unboundTTL) || (bound && now.Sub(p.last) > idleTTL) {
			s.releaseLocked(key)
		}
	}
}
