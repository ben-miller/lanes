package port

import "testing"

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
