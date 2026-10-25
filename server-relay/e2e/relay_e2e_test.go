// Package e2e drives the real relay binary over WebSocket against a real PostgreSQL, including restarts, so
// what the unit tests fake (storage, migrations, process lifecycle) is covered too. Run it with scripts/e2e.sh,
// or by hand against a throwaway database (never a production one):
//
//	docker run --rm -d -p 55432:5432 -e POSTGRES_PASSWORD=postgres postgres:16-alpine
//	OPENDESK_TEST_PG_PORT=55432 go test ./e2e -v
package e2e

import (
	"bytes"
	"database/sql"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/gorilla/websocket"
	_ "github.com/lib/pq"
)

const (
	adminID  = "811111111"
	hostID   = "822222222"
	viewerID = "833333333"
	otherID  = "844444444"
)

// relay is one running relay process on a free port.
type relay struct {
	t    *testing.T
	bin  string
	cfg  string
	port int
	cmd  *exec.Cmd
	logs bytes.Buffer
}

func (r *relay) url() string { return fmt.Sprintf("ws://127.0.0.1:%d/ws", r.port) }

func (r *relay) start() {
	r.t.Helper()
	r.logs.Reset()
	r.cmd = exec.Command(r.bin, "-config", r.cfg)
	r.cmd.Env = relayEnv()
	r.cmd.Stdout, r.cmd.Stderr = &r.logs, &r.logs
	if err := r.cmd.Start(); err != nil {
		r.t.Fatal(err)
	}
	deadline := time.Now().Add(15 * time.Second)
	for time.Now().Before(deadline) {
		if resp, err := http.Get(fmt.Sprintf("http://127.0.0.1:%d/healthz", r.port)); err == nil {
			resp.Body.Close()
			if resp.StatusCode == http.StatusOK {
				return
			}
		}
		time.Sleep(100 * time.Millisecond)
	}
	r.stop()
	r.t.Fatalf("relay did not become healthy:\n%s", r.logs.String())
}

// relayEnv is this process's environment without the variables that override the relay's config file, so an
// inherited DB_HOST can never point the test relay at a real database.
func relayEnv() []string {
	var env []string
	for _, kv := range os.Environ() {
		if !strings.HasPrefix(kv, "DB_") && !strings.HasPrefix(kv, "OPENDESK_") {
			env = append(env, kv)
		}
	}
	return env
}

func (r *relay) stop() {
	if r.cmd == nil || r.cmd.Process == nil {
		return
	}
	_ = r.cmd.Process.Kill()
	_, _ = r.cmd.Process.Wait()
	r.cmd = nil
}

// startRelay builds the relay, gives it a fresh database and starts it. Everything is undone on cleanup.
func startRelay(t *testing.T) *relay {
	pgPort, err := strconv.Atoi(os.Getenv("OPENDESK_TEST_PG_PORT"))
	if err != nil {
		t.Skip("OPENDESK_TEST_PG_PORT not set (see the package comment)")
	}
	dir := t.TempDir()
	bin := filepath.Join(dir, "relay")
	if runtime.GOOS == "windows" {
		bin += ".exe"
	}
	if out, err := exec.Command("go", "build", "-o", bin, "../cmd/relay").CombinedOutput(); err != nil {
		t.Fatalf("build relay: %v\n%s", err, out)
	}

	admin, err := sql.Open("postgres", fmt.Sprintf("host=127.0.0.1 port=%d user=postgres password=postgres dbname=postgres sslmode=disable", pgPort))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { admin.Close() })
	dbName := fmt.Sprintf("opendesk_e2e_%d_%d", os.Getpid(), time.Now().UnixNano()%1_000_000)
	if _, err := admin.Exec("CREATE DATABASE " + dbName); err != nil {
		t.Fatal(err)
	}

	cfg := filepath.Join(dir, "config.json")
	port := freePort(t)
	raw, _ := json.Marshal(map[string]any{
		"host": "127.0.0.1", "port": port, "ping_interval_seconds": 15, "read_buffer_size": 32768,
		"write_buffer_size": 32768, "db_host": "127.0.0.1", "db_port": pgPort, "db_user": "postgres",
		"db_password": "postgres", "db_name": dbName,
	})
	if err := os.WriteFile(cfg, raw, 0o600); err != nil {
		t.Fatal(err)
	}
	r := &relay{t: t, bin: bin, cfg: cfg, port: port}
	r.start()
	t.Cleanup(func() {
		r.stop()
		_, _ = admin.Exec("DROP DATABASE IF EXISTS " + dbName + " WITH (FORCE)")
	})
	return r
}

func freePort(t *testing.T) int {
	l, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer l.Close()
	return l.Addr().(*net.TCPAddr).Port
}

type peer struct {
	t  *testing.T
	ws *websocket.Conn
}

func dial(t *testing.T, r *relay, id string, extra map[string]any) *peer {
	t.Helper()
	ws, _, err := websocket.DefaultDialer.Dial(r.url(), nil)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { ws.Close() })
	msg := map[string]any{"type": "register", "deskId": id, "platform": "windows-pc", "deviceName": "e2e-" + id}
	for k, v := range extra {
		msg[k] = v
	}
	p := &peer{t: t, ws: ws}
	p.send(msg)
	return p
}

func (p *peer) send(msg map[string]any) {
	p.t.Helper()
	if err := p.ws.WriteJSON(msg); err != nil {
		p.t.Fatal(err)
	}
}

// expect skips other messages until one of type want arrives.
func (p *peer) expect(want string) map[string]any {
	p.t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for {
		_ = p.ws.SetReadDeadline(deadline)
		kind, data, err := p.ws.ReadMessage()
		if err != nil {
			p.t.Fatalf("waiting for %q: %v", want, err)
		}
		if kind != websocket.TextMessage {
			continue
		}
		var m map[string]any
		if err := json.Unmarshal(data, &m); err != nil {
			p.t.Fatal(err)
		}
		if m["type"] == want {
			return m
		}
	}
}

func (p *peer) expectBinary() []byte {
	p.t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for {
		_ = p.ws.SetReadDeadline(deadline)
		kind, data, err := p.ws.ReadMessage()
		if err != nil {
			p.t.Fatalf("waiting for a binary frame: %v", err)
		}
		if kind == websocket.BinaryMessage {
			return data
		}
	}
}

// hosts asks for the host list until pred holds for the named device (the registry is written asynchronously).
func (p *peer) host(clean string, pred func(map[string]any) bool) map[string]any {
	p.t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	var last map[string]any
	for time.Now().Before(deadline) {
		p.send(map[string]any{"type": "get-hosts"})
		for _, h := range p.expect("host-list")["hosts"].([]any) {
			if h := h.(map[string]any); h["clean_id"] == clean {
				last = h
				if pred(h) {
					return h
				}
			}
		}
		time.Sleep(100 * time.Millisecond)
	}
	p.t.Fatalf("host %s never matched; last %v", clean, last)
	return nil
}

func TestStatusEndpoint(t *testing.T) {
	r := startRelay(t)
	resp, err := http.Get(fmt.Sprintf("http://127.0.0.1:%d/", r.port))
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()
	var st map[string]any
	if err := json.NewDecoder(resp.Body).Decode(&st); err != nil {
		t.Fatal(err)
	}
	if st["status"] != "online" || st["storage"] != "postgresql" || st["version"] == "" {
		t.Fatalf("status: %v", st)
	}
}

// A full relayed session: connect, video one way, input/clipboard/file the other, then a clean disconnect.
func TestRelayedSession(t *testing.T) {
	r := startRelay(t)
	host := dial(t, r, hostID, map[string]any{"caps": []string{"video-delta", "file-ack"}})
	host.expect("registered-ack")
	viewer := dial(t, r, viewerID, map[string]any{"caps": []string{"video-delta"}})
	viewer.expect("registered-ack")

	viewer.send(map[string]any{"type": "connect-request", "targetId": "822 222 222"})
	if m := host.expect("session-connected"); m["role"] != "host" || m["targetClean"] != viewerID {
		t.Fatalf("host side: %v", m)
	}
	viewer.expect("session-connected")

	// Video is anything starting with the JPEG marker; the relay forwards it untouched.
	frame := append([]byte{0xFF, 0xD8}, bytes.Repeat([]byte{0xAB}, 64*1024)...)
	if err := host.ws.WriteMessage(websocket.BinaryMessage, frame); err != nil {
		t.Fatal(err)
	}
	if got := viewer.expectBinary(); !bytes.Equal(got, frame) {
		t.Fatalf("frame changed in transit: %d bytes", len(got))
	}

	viewer.send(map[string]any{"type": "relay-input", "targetClean": hostID, "event": map[string]any{"action": "mousemove", "normX": 0.25}})
	if m := host.expect("input-command"); m["event"].(map[string]any)["normX"] != 0.25 {
		t.Fatalf("input: %v", m)
	}
	viewer.send(map[string]any{"type": "relay-clipboard", "targetClean": hostID, "text": "halo"})
	if m := host.expect("clipboard-sync"); m["text"] != "halo" {
		t.Fatalf("clipboard: %v", m)
	}
	viewer.send(map[string]any{"type": "relay-file", "targetClean": hostID, "payload": map[string]any{"action": "start", "id": "ft_1", "name": "a.txt"}})
	if m := host.expect("file-transfer"); m["fromClean"] != viewerID {
		t.Fatalf("file: %v", m)
	}

	// A stranger cannot inject input into a session it is not part of.
	other := dial(t, r, otherID, nil)
	other.expect("registered-ack")
	other.send(map[string]any{"type": "relay-input", "targetClean": hostID, "event": map[string]any{"action": "mousemove", "normX": 0.9}})

	viewer.send(map[string]any{"type": "session-disconnect", "targetClean": hostID})
	host.expect("session-closed")
	_ = host.ws.SetReadDeadline(time.Now().Add(300 * time.Millisecond))
	for {
		kind, data, err := host.ws.ReadMessage()
		if err != nil {
			break
		}
		if kind == websocket.TextMessage && bytes.Contains(data, []byte(`"input-command"`)) {
			t.Fatalf("stranger input reached the host: %s", data)
		}
	}
}

// Device names and the client-token allowlist live in PostgreSQL: both survive a relay restart, and the
// allowlist keeps refusing devices without their token afterwards.
func TestSecurityAndNamesSurviveRestart(t *testing.T) {
	r := startRelay(t)
	admin := dial(t, r, adminID, map[string]any{"isAdmin": true})
	admin.expect("registered-ack")
	host := dial(t, r, hostID, nil)
	host.expect("registered-ack")

	admin.send(map[string]any{"type": "host-rename", "targetClean": hostID, "alias": "Kasir 1"})
	admin.host(hostID, func(h map[string]any) bool { return h["alias"] == "Kasir 1" })

	admin.send(map[string]any{"type": "security-allow", "targetId": hostID})
	admin.expect("security-state")
	admin.send(map[string]any{"type": "security-set", "enabled": true})
	adminToken := admin.expect("device-credential")["token"].(string)
	if m := admin.expect("security-state"); m["enabled"] != true {
		t.Fatalf("allowlist not on: %v", m)
	}

	// The listed host is connected, so it enrolls at once; an unlisted device is refused.
	hostToken := host.expect("device-credential")["token"].(string)
	if m := dial(t, r, otherID, nil).expect("error"); m["message"] == "" {
		t.Fatalf("unlisted device admitted: %v", m)
	}

	r.stop()
	r.start()

	if m := dial(t, r, otherID, nil).expect("error"); m["message"] == "" {
		t.Fatalf("unlisted device admitted after restart: %v", m)
	}
	if m := dial(t, r, hostID, nil).expect("error"); m["message"] == "" {
		t.Fatalf("enrolled device admitted without its token after restart: %v", m)
	}
	dial(t, r, hostID, map[string]any{"deviceToken": hostToken}).expect("registered-ack")
	admin = dial(t, r, adminID, map[string]any{"isAdmin": true, "deviceToken": adminToken})
	admin.expect("registered-ack")
	admin.host(hostID, func(h map[string]any) bool { return h["alias"] == "Kasir 1" && h["online"] == true })

	// Turning the allowlist off opens the relay again, also across a restart.
	admin.send(map[string]any{"type": "security-set", "enabled": false})
	admin.expect("security-state")
	r.stop()
	r.start()
	dial(t, r, otherID, nil).expect("registered-ack")
}
