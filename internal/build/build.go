// Package build exposes version information injected at build time via -ldflags,
// and provides update logic for development builds.
package build

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
)

// These vars are set at build time via:
//
//	go build -ldflags "
//	  -X github.com/bmiller/spinner/internal/build.Version=$(git rev-parse HEAD)
//	  -X github.com/bmiller/spinner/internal/build.SourceDir=$(pwd)
//	  -X github.com/bmiller/spinner/internal/build.Mode=development
//	"
var (
	Version   = "unknown"     // git commit SHA of the source at build time
	SourceDir = ""            // absolute path to source repository at build time
	Mode      = "development" // "development" or "release"
)

// IsDev returns true for local development builds.
func IsDev() bool {
	return Mode == "development"
}

// IsKnown returns true if version info was injected at build time.
func IsKnown() bool {
	return Version != "unknown" && SourceDir != ""
}

// Short returns the first 8 characters of the version SHA.
func Short() string {
	if len(Version) >= 8 {
		return Version[:8]
	}
	return Version
}

// Info returns a human-readable version string.
func Info() string {
	if !IsKnown() {
		return "spinner (unknown version — install with `make install` to embed version info)"
	}
	return fmt.Sprintf("spinner %s (%s)", Short(), Mode)
}

// HeadSHA reads the current HEAD commit SHA from the source repository.
// Returns empty string if the source directory is not set or unreadable.
func HeadSHA() string {
	if SourceDir == "" {
		return ""
	}
	headFile := filepath.Join(SourceDir, ".git", "HEAD")
	data, err := os.ReadFile(headFile)
	if err != nil {
		return ""
	}
	line := strings.TrimSpace(string(data))
	// Detached HEAD: line is the SHA directly.
	if !strings.HasPrefix(line, "ref: ") {
		return line
	}
	// Symbolic ref: e.g. "ref: refs/heads/main"
	ref := strings.TrimPrefix(line, "ref: ")
	refData, err := os.ReadFile(filepath.Join(SourceDir, ".git", ref))
	if err != nil {
		return ""
	}
	return strings.TrimSpace(string(refData))
}

// IsStale returns true if the source repo has newer commits than the running binary.
func IsStale() bool {
	if !IsDev() || !IsKnown() {
		return false
	}
	head := HeadSHA()
	return head != "" && head != Version
}

// BuildAndReexec rebuilds spinner from source with version info baked in,
// then replaces the current process with the new binary via exec.
func BuildAndReexec() error {
	binary, err := os.Executable()
	if err != nil {
		return fmt.Errorf("finding executable: %w", err)
	}
	binary, err = filepath.EvalSymlinks(binary)
	if err != nil {
		return fmt.Errorf("resolving executable path: %w", err)
	}

	head := HeadSHA()
	if head == "" {
		head = Version
	}

	short := head
	if len(short) > 8 {
		short = short[:8]
	}

	ldflags := fmt.Sprintf(
		"-X github.com/bmiller/spinner/internal/build.Version=%s -X github.com/bmiller/spinner/internal/build.SourceDir=%s -X github.com/bmiller/spinner/internal/build.Mode=%s",
		head, SourceDir, Mode,
	)

	fmt.Fprintf(os.Stderr, "spinner: building %s...\n", short)

	c := exec.Command("go", "build", "-ldflags", ldflags, "-o", binary, SourceDir)
	c.Stdout = os.Stdout
	c.Stderr = os.Stderr
	if err := c.Run(); err != nil {
		return fmt.Errorf("build failed: %w", err)
	}

	fmt.Fprintf(os.Stderr, "spinner: updated to %s — restarting\n", short)
	return syscall.Exec(binary, os.Args, os.Environ())
}
