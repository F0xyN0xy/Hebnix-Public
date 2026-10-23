package main

import (
	"context"
	"net"
	"syscall"
)

// SO_REUSEADDR so RL and this proxy can hold the same discovery port; Linux
// delivers broadcasts to every socket sharing it
func listenReuse(network, addr string) (net.PacketConn, error) {
	lc := net.ListenConfig{Control: func(_, _ string, c syscall.RawConn) error {
		var serr error
		c.Control(func(fd uintptr) {
			serr = syscall.SetsockoptInt(int(fd), syscall.SOL_SOCKET, syscall.SO_REUSEADDR, 1)
		})
		return serr
	}}
	return lc.ListenPacket(context.Background(), network, addr)
}

func setBroadcast(fd uintptr) error {
	return syscall.SetsockoptInt(int(fd), syscall.SOL_SOCKET, syscall.SO_BROADCAST, 1)
}
