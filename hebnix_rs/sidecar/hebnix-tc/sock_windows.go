package main

import (
	"context"
	"net"
	"syscall"
)

// SO_REUSEADDR so RL and this proxy can hold the same discovery port (RL
// opens 14001 only while its LAN browser searches, and errors with "error
// while connecting to local host" if something holds it exclusively)
func listenReuse(network, addr string) (net.PacketConn, error) {
	lc := net.ListenConfig{Control: func(_, _ string, c syscall.RawConn) error {
		var serr error
		c.Control(func(fd uintptr) {
			serr = syscall.SetsockoptInt(syscall.Handle(fd), syscall.SOL_SOCKET, syscall.SO_REUSEADDR, 1)
		})
		return serr
	}}
	return lc.ListenPacket(context.Background(), network, addr)
}

func setBroadcast(fd uintptr) error {
	return syscall.SetsockoptInt(syscall.Handle(fd), syscall.SOL_SOCKET, syscall.SO_BROADCAST, 1)
}
