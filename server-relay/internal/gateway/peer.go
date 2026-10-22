package gateway

import (
	"fmt"
	"net"
	"regexp"
	"sync"

	"github.com/gorilla/websocket"
)

var nonDigits = regexp.MustCompile(`\D`)

func CleanDeskID(id string) string {
	return nonDigits.ReplaceAllString(id, "")
}

func FormatDeskID(raw string) string {
	digits := CleanDeskID(raw)
	if len(digits) >= 9 {
		return fmt.Sprintf("%s %s %s", digits[0:3], digits[3:6], digits[6:9])
	}
	return digits
}

type framePacket struct {
	isBinary bool
	data     []byte
	jsonMsg  map[string]interface{}
}

type Peer struct {
	CleanID     string
	DeskID      string
	Platform    string
	DeviceName  string
	IsAdmin     bool
	SessionWith string
	WS          *websocket.Conn
	writeMu     sync.Mutex
	frameCh     chan framePacket
	closeCh     chan struct{}
	closeOnce   sync.Once
}

func NewPeer(cleanID, platform, deviceName string, isAdmin bool, ws *websocket.Conn) *Peer {
	if tcpConn, ok := ws.UnderlyingConn().(*net.TCPConn); ok {
		_ = tcpConn.SetNoDelay(true)
	}
	p := &Peer{
		CleanID:    cleanID,
		DeskID:     FormatDeskID(cleanID),
		Platform:   platform,
		DeviceName: deviceName,
		IsAdmin:    isAdmin,
		WS:         ws,
		frameCh:    make(chan framePacket, 1),
		closeCh:    make(chan struct{}),
	}
	go p.frameWriterLoop()
	return p
}

func (p *Peer) Close() {
	p.closeOnce.Do(func() {
		close(p.closeCh)
	})
}

func (p *Peer) frameWriterLoop() {
	for {
		select {
		case <-p.closeCh:
			return
		case pkt := <-p.frameCh:
			if pkt.isBinary {
				_ = p.SendRawBinary(pkt.data)
			} else if pkt.jsonMsg != nil {
				_ = p.SendJSON(pkt.jsonMsg)
			}
		}
	}
}

// EnqueueBinaryFrame uses latest-frame-wins to prevent Cloudflare tunnel bufferbloat
func (p *Peer) EnqueueBinaryFrame(data []byte) {
	pkt := framePacket{isBinary: true, data: data}
	select {
	case p.frameCh <- pkt:
	default:
		select {
		case <-p.frameCh:
		default:
		}
		select {
		case p.frameCh <- pkt:
		default:
		}
	}
}

// EnqueueJSONFrame uses latest-frame-wins for legacy JSON base64 frames
func (p *Peer) EnqueueJSONFrame(msg map[string]interface{}) {
	pkt := framePacket{isBinary: false, jsonMsg: msg}
	select {
	case p.frameCh <- pkt:
	default:
		select {
		case <-p.frameCh:
		default:
		}
		select {
		case p.frameCh <- pkt:
		default:
		}
	}
}

func (p *Peer) SendJSON(v interface{}) error {
	p.writeMu.Lock()
	defer p.writeMu.Unlock()
	return p.WS.WriteJSON(v)
}

func (p *Peer) SendRawText(data []byte) error {
	p.writeMu.Lock()
	defer p.writeMu.Unlock()
	return p.WS.WriteMessage(websocket.TextMessage, data)
}

func (p *Peer) SendRawBinary(data []byte) error {
	p.writeMu.Lock()
	defer p.writeMu.Unlock()
	return p.WS.WriteMessage(websocket.BinaryMessage, data)
}
