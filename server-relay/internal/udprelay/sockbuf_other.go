// UDP socket buffer sizes are only read back on Linux, where the relay runs in production.

//go:build !linux

package udprelay

import "net"

// socketBuffers reports nothing outside Linux; the startup log then only shows the requested sizes.
func socketBuffers(*net.UDPConn) (rcv, snd int, ok bool) { return 0, 0, false }
