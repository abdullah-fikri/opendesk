package udprelay

import (
	"bytes"
	"log/slog"
	"net"
	"strings"
	"testing"
)

func TestReportBuffersWarnsWhenTheKernelCaps(t *testing.T) {
	cases := []struct {
		rcv, snd int
		ok       bool
		want     string
	}{
		{socketBuffer, socketBuffer, true, "level=INFO"},
		{212992, socketBuffer, true, "level=WARN"}, // default net.core.rmem_max
		{socketBuffer, 212992, true, "level=WARN"},
		{0, 0, false, "level=INFO"},
	}
	for _, c := range cases {
		var out bytes.Buffer
		reportBuffers(slog.New(slog.NewTextHandler(&out, nil)), socketBuffer, c.rcv, c.snd, c.ok)
		if !strings.Contains(out.String(), c.want) {
			t.Errorf("rcv %d snd %d ok %v: want %s, got %q", c.rcv, c.snd, c.ok, c.want, out.String())
		}
	}
}

func TestSocketBuffersReadsAGrantedSize(t *testing.T) {
	conn, err := net.ListenUDP("udp", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)})
	if err != nil {
		t.Skip("no loopback UDP:", err)
	}
	defer conn.Close()
	_ = conn.SetReadBuffer(64 << 10)
	rcv, snd, ok := socketBuffers(conn)
	if ok && (rcv <= 0 || snd <= 0) {
		t.Fatalf("granted sizes must be positive, got %d %d", rcv, snd)
	}
}
