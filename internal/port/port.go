package port

import "hash/fnv"

// Assign returns a deterministic port for the given branch name within [min, max].
// The same branch name always maps to the same port for a given range.
func Assign(branch string, min, max int) int {
	h := fnv.New32a()
	h.Write([]byte(branch))
	size := max - min + 1
	return min + int(h.Sum32())%size
}
