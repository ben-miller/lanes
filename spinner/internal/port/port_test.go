package port

import (
	"net"
	"strconv"
	"testing"
)

func TestAssignDeterministic(t *testing.T) {
	p1 := Assign("main", 4100, 4199)
	p2 := Assign("main", 4100, 4199)
	if p1 != p2 {
		t.Errorf("Assign is not deterministic: %d != %d", p1, p2)
	}
}

func TestAssignInRange(t *testing.T) {
	branches := []string{"main", "feature-foo", "fix/bar", "release-1.0", "my-very-long-branch-name-that-goes-on-forever"}
	for _, b := range branches {
		p := Assign(b, 4100, 4199)
		if p < 4100 || p > 4199 {
			t.Errorf("Assign(%q) = %d, out of range [4100, 4199]", b, p)
		}
	}
}

func TestAssignDifferentBranches(t *testing.T) {
	// Different branches should (usually) get different ports.
	// Not a strict requirement since collisions are possible, but with 100 ports
	// and a small set of branches this should hold.
	ports := map[int]string{}
	branches := []string{"main", "feature-foo", "fix-bar", "staging", "develop"}
	for _, b := range branches {
		p := Assign(b, 4100, 4199)
		if prev, ok := ports[p]; ok {
			t.Logf("collision: %q and %q both map to %d (acceptable but notable)", prev, b, p)
		}
		ports[p] = b
	}
}

func TestAssignSinglePort(t *testing.T) {
	p := Assign("any-branch", 5000, 5000)
	if p != 5000 {
		t.Errorf("expected 5000 for single-port range, got %d", p)
	}
}

func TestIsFree(t *testing.T) {
	l, err := net.Listen("tcp", ":0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer l.Close()
	taken := l.Addr().(*net.TCPAddr).Port

	if IsFree(taken) {
		t.Errorf("IsFree(%d) = true, want false (port is bound)", taken)
	}
}

func TestFindAvailableReturnsHashedPortWhenFree(t *testing.T) {
	branch := "totally-unique-branch-name-for-testing"
	min, max := 40000, 40099
	want := Assign(branch, min, max)

	got, err := FindAvailable(branch, min, max)
	if err != nil {
		t.Fatalf("FindAvailable: %v", err)
	}
	if got != want {
		t.Errorf("FindAvailable(%q) = %d, want hash-assigned %d (should be free)", branch, got, want)
	}
}

func TestFindAvailableSkipsOccupiedPort(t *testing.T) {
	branch := "main"
	min, max := 41000, 41099
	hashed := Assign(branch, min, max)

	l, err := net.Listen("tcp", net.JoinHostPort("", strconv.Itoa(hashed)))
	if err != nil {
		t.Skipf("could not bind hashed port %d to set up test: %v", hashed, err)
	}
	defer l.Close()

	got, err := FindAvailable(branch, min, max)
	if err != nil {
		t.Fatalf("FindAvailable: %v", err)
	}
	if got == hashed {
		t.Errorf("FindAvailable(%q) = %d, expected it to skip the occupied hashed port", branch, got)
	}
	if got < min || got > max {
		t.Errorf("FindAvailable(%q) = %d, out of range [%d, %d]", branch, got, min, max)
	}
}

func TestFindAvailableErrorsWhenRangeFull(t *testing.T) {
	branch := "main"
	min, max := 42000, 42001

	l1, err := net.Listen("tcp", net.JoinHostPort("", strconv.Itoa(min)))
	if err != nil {
		t.Skipf("could not bind %d to set up test: %v", min, err)
	}
	defer l1.Close()

	l2, err := net.Listen("tcp", net.JoinHostPort("", strconv.Itoa(max)))
	if err != nil {
		t.Skipf("could not bind %d to set up test: %v", max, err)
	}
	defer l2.Close()

	if _, err := FindAvailable(branch, min, max); err == nil {
		t.Error("FindAvailable with a fully occupied range should return an error")
	}
}
