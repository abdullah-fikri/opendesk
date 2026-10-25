package handler

import (
	"net"
	"time"

	"github.com/gorilla/websocket"
)

// writeTimeout bounds how long a stalled receiver can block the sender's read loop.
const writeTimeout = 10 * time.Second

// wsConn adapts a gorilla WebSocket to usecase.Conn.
type wsConn struct {
	ws *websocket.Conn
}

func newWSConn(ws *websocket.Conn) *wsConn {
	// Input and small control messages must not wait for Nagle coalescing.
	if tcp, ok := ws.UnderlyingConn().(*net.TCPConn); ok {
		_ = tcp.SetNoDelay(true)
	}
	return &wsConn{ws: ws}
}

// WriteText implements usecase.Conn.
func (c *wsConn) WriteText(data []byte) error {
	_ = c.ws.SetWriteDeadline(time.Now().Add(writeTimeout))
	return c.ws.WriteMessage(websocket.TextMessage, data)
}

// WriteBinary implements usecase.Conn; several parts are streamed into one message without joining them.
func (c *wsConn) WriteBinary(parts ...[]byte) error {
	_ = c.ws.SetWriteDeadline(time.Now().Add(writeTimeout))
	if len(parts) == 1 {
		return c.ws.WriteMessage(websocket.BinaryMessage, parts[0])
	}
	w, err := c.ws.NextWriter(websocket.BinaryMessage)
	if err != nil {
		return err
	}
	for _, p := range parts {
		if _, err := w.Write(p); err != nil {
			_ = w.Close()
			return err
		}
	}
	return w.Close()
}

// Ping uses a control frame; gorilla allows WriteControl concurrently with other writes.
func (c *wsConn) Ping() error {
	return c.ws.WriteControl(websocket.PingMessage, nil, time.Now().Add(writeTimeout))
}

// Close implements usecase.Conn.
func (c *wsConn) Close() error { return c.ws.Close() }
