package main

import (
	"bytes"
	"testing"
)

func TestRewriteServerID(t *testing.T) {
	in := append([]byte("\x00\x00\x00\x22ProjectX.LanMessage_HostResponse_X\x00"),
		`{"Result":{"Address":""},"ServerID":"192.168.0.8:7777","MetaData":"{}"}`...)
	out := rewriteBeacon(append([]byte(nil), in...), [4]byte{127, 0, 0, 1})
	if !bytes.Contains(out, []byte(`"ServerID":"127.0.0.1:7777"`)) {
		t.Fatalf("not rewritten: %q", out)
	}
	if !bytes.HasPrefix(out, in[:39]) || !bytes.HasSuffix(out, []byte(`,"MetaData":"{}"}`)) {
		t.Fatalf("rest of packet changed: %q", out)
	}
	spaced := rewriteBeacon([]byte(`{"ServerID": "10.0.0.5:7777"}`), [4]byte{127, 0, 0, 2})
	if string(spaced) != `{"ServerID": "127.0.0.2:7777"}` {
		t.Fatalf("spaced JSON not rewritten: %q", spaced)
	}
}

// ported from hosting.rs's tests, from before the rewrite moved here

func TestRewriteUnrealString(t *testing.T) {
	payload := unrealANSI("192.168.0.119:7777")
	out := rewriteBeacon(append([]byte(nil), payload...), [4]byte{127, 0, 0, 1})
	if !bytes.Equal(out, unrealANSI("127.0.0.1:7777")) {
		t.Fatalf("got %q", out)
	}
	wide := unrealUTF16("10.0.0.5:7777")
	out = rewriteBeacon(append([]byte(nil), wide...), [4]byte{127, 0, 0, 1})
	if !bytes.Equal(out, unrealUTF16("127.0.0.1:7777")) {
		t.Fatalf("utf16: got %q", out)
	}
}

func TestRewriteBinaryAndEqualLength(t *testing.T) {
	addr := [4]byte{100, 64, 0, 1}
	binary := []byte{172, 31, 64, 1, 0x1e, 0x61}
	if !replaceBinaryEndpoint(binary, addr) || !bytes.Equal(binary[:4], addr[:]) {
		t.Fatalf("binary endpoint not rewritten: %v", binary)
	}
	// only fires on an exact-length match, by design
	text := []byte("172.31.64.1:7777")
	if !replaceEqualLengthASCII(text, "100.64.77.1") || string(text) != "100.64.77.1:7777" {
		t.Fatalf("ascii endpoint not rewritten: %q", text)
	}
	if replaceEqualLengthASCII([]byte("10.0.0.1:7777"), "100.64.77.1") {
		t.Fatal("rewrote a different-length endpoint")
	}
}
