package udprelay

import (
	"bytes"
	"context"
	"io"
	"log/slog"
	"net"
	"net/netip"
	"testing"
	"time"
)

type sent struct {
	b  []byte
	to netip.AddrPort
}

func testServer(mbps int) (*Server, *[]sent, *time.Time) {
	s := newServer(mbps, slog.New(slog.NewTextHandler(io.Discard, nil)))
	out := &[]sent{}
	s.writeTo = func(b []byte, to netip.AddrPort) { *out = append(*out, sent{append([]byte(nil), b...), to}) }
	now := time.Unix(1000, 0)
	s.now = func() time.Time { return now }
	return s, out, &now
}

func bindPkt(t Token) []byte { return append([]byte{tagControl, ctlBind}, t[:]...) }

var (
	addrA = netip.MustParseAddrPort("198.51.100.1:40000")
	addrB = netip.MustParseAddrPort("203.0.113.9:50000")
	addrX = netip.MustParseAddrPort("192.0.2.66:1234")
)

func TestForwardsOnlyBetweenBoundLegs(t *testing.T) {
	s, out, _ := testServer(100)
	ta, tb, _ := s.Allocate(PairKey("222", "111"))

	s.handle([]byte{tagP2P, 1}, addrA)
	if len(*out) != 0 {
		t.Fatal("an unbound address got a datagram forwarded")
	}
	s.handle(bindPkt(ta), addrA)
	if len(*out) != 1 || !bytes.Equal((*out)[0].b, []byte{tagControl, ctlBound}) || (*out)[0].to != addrA {
		t.Fatalf("bind not acknowledged: %+v", *out)
	}
	s.handle([]byte{tagP2P, 2}, addrA)
	if len(*out) != 1 {
		t.Fatal("forwarded before the other leg bound")
	}
	s.handle(bindPkt(tb), addrB)
	*out = nil

	s.handle([]byte{tagP2P, 3}, addrA)
	s.handle([]byte{tagP2P, 4}, addrB)
	s.handle([]byte{tagP2P, 5}, addrX)       // stranger
	s.handle([]byte{0x00, 6}, addrA)         // not a P2P datagram
	s.handle(bindPkt(Token{9, 9, 9}), addrX) // unknown token
	want := []sent{{[]byte{tagP2P, 3}, addrB}, {[]byte{tagP2P, 4}, addrA}}
	if len(*out) != len(want) {
		t.Fatalf("got %d datagrams, want %d: %+v", len(*out), len(want), *out)
	}
	for i := range want {
		if !bytes.Equal((*out)[i].b, want[i].b) || (*out)[i].to != want[i].to {
			t.Fatalf("datagram %d = %+v, want %+v", i, (*out)[i], want[i])
		}
	}
}

func TestRebindMovesLegAndReleaseStopsForwarding(t *testing.T) {
	s, out, _ := testServer(100)
	key := PairKey("111", "222")
	ta, tb, _ := s.Allocate(key)
	s.handle(bindPkt(ta), addrA)
	s.handle(bindPkt(tb), addrB)
	moved := netip.MustParseAddrPort("198.51.100.1:40001")
	s.handle(bindPkt(ta), moved) // NAT rebinding
	*out = nil

	s.handle([]byte{tagP2P, 1}, addrA)
	s.handle([]byte{tagP2P, 2}, addrB)
	if len(*out) != 1 || (*out)[0].to != moved {
		t.Fatalf("after rebinding: %+v", *out)
	}
	s.Release(key)
	*out = nil
	s.handle([]byte{tagP2P, 3}, moved)
	s.handle(bindPkt(ta), moved)
	if len(*out) != 0 {
		t.Fatalf("released pair still active: %+v", *out)
	}
}

func TestAllocateReplacesOldTokens(t *testing.T) {
	s, out, _ := testServer(100)
	key := PairKey("111", "222")
	old, _, _ := s.Allocate(key)
	s.Allocate(key)
	s.handle(bindPkt(old), addrA)
	if len(*out) != 0 {
		t.Fatal("a replaced token still binds")
	}
}

func TestRateLimitDropsOverBudget(t *testing.T) {
	s, out, now := testServer(1) // 125 kB/s, burst 31.25 kB
	ta, tb, _ := s.Allocate(PairKey("111", "222"))
	s.handle(bindPkt(ta), addrA)
	s.handle(bindPkt(tb), addrB)
	*out = nil
	pkt := append([]byte{tagP2P}, make([]byte, 999)...)
	for range 100 {
		s.handle(pkt, addrA)
	}
	if n := len(*out); n != 31 {
		t.Fatalf("burst forwarded %d datagrams, want 31", n)
	}
	*now = now.Add(100 * time.Millisecond) // +12.5 kB
	*out = nil
	for range 100 {
		s.handle(pkt, addrA)
	}
	if n := len(*out); n != 12 && n != 13 {
		t.Fatalf("refill forwarded %d datagrams, want about 12", n)
	}
}

func TestSweepDropsUnboundAndIdlePairs(t *testing.T) {
	s, _, now := testServer(100)
	s.Allocate("unbound")
	ta, _, _ := s.Allocate("idle")
	s.handle(bindPkt(ta), addrA)
	*now = now.Add(30 * time.Second)
	ta2, _, _ := s.Allocate("fresh")
	s.handle(bindPkt(ta2), addrB)
	*now = now.Add(31 * time.Second)
	s.sweep()
	if _, ok := s.pairs["unbound"]; ok {
		t.Error("unbound allocation survived")
	}
	if _, ok := s.pairs["idle"]; ok {
		t.Error("idle pair survived")
	}
	if _, ok := s.pairs["fresh"]; !ok {
		t.Error("recently bound pair was dropped")
	}
	if len(s.byAddr) != 1 || len(s.byToken) != 2 {
		t.Errorf("index leak: byAddr=%d byToken=%d", len(s.byAddr), len(s.byToken))
	}
}

// TestRealSockets runs the forwarder on loopback UDP.
func TestRealSockets(t *testing.T) {
	s, err := Listen("127.0.0.1:0", 100, slog.New(slog.NewTextHandler(io.Discard, nil)))
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go s.Run(ctx)
	relay := s.conn.LocalAddr().(*net.UDPAddr)
	ta, tb, _ := s.Allocate(PairKey("1", "2"))

	dial := func(tok Token) *net.UDPConn {
		c, err := net.DialUDP("udp", nil, relay)
		if err != nil {
			t.Fatal(err)
		}
		_ = c.SetReadDeadline(time.Now().Add(2 * time.Second))
		if _, err := c.Write(bindPkt(tok)); err != nil {
			t.Fatal(err)
		}
		buf := make([]byte, 16)
		if n, err := c.Read(buf); err != nil || !bytes.Equal(buf[:n], []byte{tagControl, ctlBound}) {
			t.Fatalf("bind answer %x, %v", buf[:n], err)
		}
		return c
	}
	a, b := dial(ta), dial(tb)
	defer a.Close()
	defer b.Close()
	if _, err := a.Write([]byte{tagP2P, 'h', 'i'}); err != nil {
		t.Fatal(err)
	}
	buf := make([]byte, 16)
	n, err := b.Read(buf)
	if err != nil || string(buf[1:n]) != "hi" {
		t.Fatalf("b got %q, %v", buf[:n], err)
	}
}
