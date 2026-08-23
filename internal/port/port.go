package port

import (
	"fmt"
	"hash/fnv"
	"net"
)

// Assign returns a deterministic port for the given branch name within [min, max].
// The same branch name always maps to the same port for a given range.
func Assign(branch string, min, max int) int {
	h := fnv.New32a()
	h.Write([]byte(branch))
	size := max - min + 1
	return min + int(h.Sum32())%size
}

// IsFree reports whether a TCP port is currently free to bind on localhost.
// Dev servers bind to specific addresses (typically 127.0.0.1, sometimes the
// IPv6 or IPv4 wildcard) rather than always using the wildcard, and on macOS
// a wildcard bind does not conflict with an already-bound specific address
// (or vice versa) — so every address a server might realistically use has to
// be checked individually.
func IsFree(p int) bool {
	addrs := []string{
		fmt.Sprintf("127.0.0.1:%d", p),
		fmt.Sprintf("0.0.0.0:%d", p),
		fmt.Sprintf(":%d", p), // IPv6 wildcard
	}
	for _, addr := range addrs {
		l, err := net.Listen("tcp", addr)
		if err != nil {
			return false
		}
		l.Close()
	}
	return true
}

// FindAvailable returns a free port for the given branch. It starts from the
// branch's deterministic hash-assigned port and scans forward (wrapping
// within the range) until it finds one that isn't already bound by another
// process. Same branch, same port on almost every run — it only shifts when
// the hash-assigned port collides with something else.
func FindAvailable(branch string, min, max int) (int, error) {
	size := max - min + 1
	start := Assign(branch, min, max)
	for i := 0; i < size; i++ {
		p := min + (start-min+i)%size
		if IsFree(p) {
			return p, nil
		}
	}
	return 0, fmt.Errorf("no free port in range [%d, %d]", min, max)
}
