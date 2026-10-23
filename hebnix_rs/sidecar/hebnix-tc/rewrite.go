package main

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"net"
	"net/netip"
	"regexp"
	"unicode/utf16"
)

// Go port of hebnix-linux hosting.rs rewrite_lan_beacon_payload: replace the
// host's real LAN address inside RL's LAN response with the guest's local
// game proxy.

// RL's current LAN reply is "ProjectX.LanMessage_HostResponse_X\0" + JSON,
// and the address the guest joins is the JSON's "ServerID":"ip:port". The
// JSON runs to the end of the packet, so a different-length ip is fine.
var serverIDRe = regexp.MustCompile(`("ServerID"\s*:\s*")\d{1,3}(?:\.\d{1,3}){3}(:\d+")`)

func rewriteBeacon(payload []byte, addr [4]byte) []byte {
	ipStr := fmt.Sprintf("%d.%d.%d.%d", addr[0], addr[1], addr[2], addr[3])
	if serverIDRe.Match(payload) {
		return serverIDRe.ReplaceAll(payload, []byte(`${1}`+ipStr+`${2}`))
	}
	if off, n, repl, ok := findUnrealEndpoint(payload, ipStr); ok {
		out := append([]byte(nil), payload[:off]...)
		out = append(out, repl...)
		return append(out, payload[off+n:]...)
	}
	if !replaceBinaryEndpoint(payload, addr) {
		replaceEqualLengthASCII(payload, ipStr)
	}
	return payload
}

func isLANEndpoint(v string) bool {
	host, port, err := net.SplitHostPort(v)
	if err != nil || port != fmt.Sprint(gamePort) {
		return false
	}
	a, err := netip.ParseAddr(host)
	return err == nil && a.Is4()
}

func findUnrealEndpoint(p []byte, ip string) (int, int, []byte, bool) {
	repl := fmt.Sprintf("%s:%d", ip, gamePort)
	for off := 0; off+4 < len(p); off++ {
		l := int32(binary.LittleEndian.Uint32(p[off:]))
		if l >= 2 && l <= 64 {
			end := off + 4 + int(l)
			if end <= len(p) && p[end-1] == 0 && isLANEndpoint(string(p[off+4:end-1])) {
				return off, 4 + int(l), unrealANSI(repl), true
			}
		}
		if l >= -64 && l <= -2 {
			chars := int(-l)
			end := off + 4 + chars*2
			if end <= len(p) && p[end-2] == 0 && p[end-1] == 0 {
				u := make([]uint16, 0, chars-1)
				for i := off + 4; i < end-2; i += 2 {
					u = append(u, binary.LittleEndian.Uint16(p[i:]))
				}
				if isLANEndpoint(string(utf16.Decode(u))) {
					return off, 4 + chars*2, unrealUTF16(repl), true
				}
			}
		}
	}
	return 0, 0, nil, false
}

func unrealANSI(v string) []byte {
	b := binary.LittleEndian.AppendUint32(nil, uint32(int32(len(v)+1)))
	return append(append(b, v...), 0)
}

func unrealUTF16(v string) []byte {
	u := utf16.Encode([]rune(v))
	b := binary.LittleEndian.AppendUint32(nil, uint32(-int32(len(u)+1)))
	for _, c := range append(u, 0) {
		b = binary.LittleEndian.AppendUint16(b, c)
	}
	return b
}

func replaceBinaryEndpoint(p []byte, repl [4]byte) bool {
	for off := 0; off+6 <= len(p); off++ {
		cand := netip.AddrFrom4([4]byte(p[off : off+4]))
		if cand.As4() == repl || !cand.IsPrivate() {
			continue
		}
		be, le := binary.BigEndian.Uint16(p[off+4:]), binary.LittleEndian.Uint16(p[off+4:])
		if be == gamePort || le == gamePort {
			copy(p[off:], repl[:])
			return true
		}
	}
	return false
}

func replaceEqualLengthASCII(p []byte, ip string) bool {
	suffix := []byte(fmt.Sprintf(":%d", gamePort))
	repl := ip + string(suffix)
	for end := len(suffix); end <= len(p); end++ {
		if !bytes.Equal(p[end-len(suffix):end], suffix) {
			continue
		}
		start := end - len(suffix)
		for start > 0 && (p[start-1] >= '0' && p[start-1] <= '9' || p[start-1] == '.') {
			start--
		}
		if isLANEndpoint(string(p[start:end])) && end-start == len(repl) {
			copy(p[start:end], repl)
			return true
		}
	}
	return false
}
