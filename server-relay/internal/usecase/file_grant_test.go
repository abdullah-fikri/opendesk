package usecase

import (
	"strings"
	"testing"
)

// fileChunk builds a binary file chunk: marker, id length, id, offset, data.
func fileChunk(id string, offset byte, data string) []byte {
	msg := append([]byte{fileChunkMarker, byte(len(id))}, id...)
	msg = append(msg, 0, 0, 0, 0, 0, 0, 0, offset)
	return append(msg, data...)
}

// A binary chunk needs exactly what a JSON one needs: the grant of this sender, transfer and receiver. It also
// needs a receiver that accepts binary chunks, and it is queued as file traffic tagged with its sender.
func TestBinaryFileChunkNeedsGrant(t *testing.T) {
	r := newTestRelay(Options{})
	a, b, other := addPeer(r, "111111111", false), addPeer(r, "222222222", false), addPeer(r, "333333333", false)
	b.FileBinary, other.FileBinary = true, true
	if r.ForwardBinary(a, fileChunk("ft_1", 0, "x")) {
		t.Fatal("chunk forwarded without a grant")
	}
	if !r.openSession(a, b, true) || r.RouteFile(a, b.CleanID, FileStart, "ft_1") != b {
		t.Fatal("transfer rejected inside session")
	}
	r.Disconnect(a, b.CleanID)
	if !r.ForwardBinary(a, fileChunk("ft_1", 0, "abc")) {
		t.Fatal("granted chunk rejected after the session ended")
	}
	for name, msg := range map[string][]byte{
		"other transfer": fileChunk("ft_2", 0, "x"),
		"empty id":       fileChunk("", 0, "x"),
		"short header":   fileChunk("ft_1", 0, "")[:8],
		"id too long":    fileChunk(strings.Repeat("x", maxFileIDLen+1), 0, "x"),
	} {
		if r.ForwardBinary(a, msg) {
			t.Fatalf("%s forwarded", name)
		}
	}
	if r.ForwardBinary(other, fileChunk("ft_1", 0, "x")) || r.ForwardBinary(b, fileChunk("ft_1", 0, "x")) {
		t.Fatal("grant used by another sender")
	}
	if b.file.n != 1 || b.control.n != 0 || other.file.n != 0 {
		t.Fatalf("chunk not queued as file traffic: file %d control %d other %d", b.file.n, b.control.n, other.file.n)
	}
	if m, _ := b.file.pop(); m.tag != a.CleanID || m.marker != sessionTag || string(m.bin) != string(fileChunk("ft_1", 0, "abc")) {
		t.Fatalf("queued chunk %+v", m)
	}

	// A receiver that never advertised file-binary gets JSON only.
	c := addPeer(r, "444444444", false)
	if !r.openSession(a, c, true) || r.RouteFile(a, c.CleanID, FileStart, "ft_3") != c {
		t.Fatal("transfer rejected inside session")
	}
	if r.ForwardBinary(a, fileChunk("ft_3", 0, "x")) || c.file.n != 0 {
		t.Fatal("binary chunk sent to a receiver without file-binary")
	}
}

// A multi-session viewer may name the host with the session tag; the grant still decides.
func TestBinaryFileChunkFromMultiSessionViewer(t *testing.T) {
	r, v, h := multiFixture(2)
	h[0].FileBinary, h[1].FileBinary = true, true
	r.openSession(v, h[0], true)
	r.openSession(v, h[1], true)
	if r.RouteFile(v, h[0].CleanID, FileStart, "ft_1") != h[0] {
		t.Fatal("transfer rejected inside session")
	}
	chunk := fileChunk("ft_1", 0, "x")
	if r.ForwardBinary(v, append([]byte("\x10"+h[1].CleanID), chunk...)) {
		t.Fatal("chunk routed to another host than its grant's")
	}
	if !r.ForwardBinary(v, append([]byte("\x10"+h[0].CleanID), chunk...)) || !r.ForwardBinary(v, chunk) {
		t.Fatal("chunk for the granted host rejected")
	}
	if h[0].file.n != 2 || h[1].file.n != 0 {
		t.Fatalf("queued: %d %d", h[0].file.n, h[1].file.n)
	}
}
