// Reading the UDP socket buffer sizes the Linux kernel actually granted.

package udprelay

import (
	"net"
	"syscall"
)

// socketBuffers returns the receive and send buffer sizes conn got, in the units SetReadBuffer and
// SetWriteBuffer take. Linux reports twice the usable size (it reserves the extra half for bookkeeping), so the
// values are halved; a request above net.core.rmem_max / wmem_max is silently capped to that maximum.
func socketBuffers(conn *net.UDPConn) (rcv, snd int, ok bool) {
	raw, err := conn.SyscallConn()
	if err != nil {
		return 0, 0, false
	}
	var rerr, serr error
	err = raw.Control(func(fd uintptr) {
		rcv, rerr = syscall.GetsockoptInt(int(fd), syscall.SOL_SOCKET, syscall.SO_RCVBUF)
		snd, serr = syscall.GetsockoptInt(int(fd), syscall.SOL_SOCKET, syscall.SO_SNDBUF)
	})
	if err != nil || rerr != nil || serr != nil {
		return 0, 0, false
	}
	return rcv / 2, snd / 2, true
}
