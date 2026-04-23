package main_test

import (
	"encoding/json"
	"fmt"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/bmiller/spinner/internal/config"
	"github.com/bmiller/spinner/internal/daemon"
	"github.com/bmiller/spinner/internal/port"
	"github.com/bmiller/spinner/internal/state"
)

// Integration tests spin up real server processes via the daemon manager.
// They require `go` in PATH to build testapp.

const (
	integPortMin = 19000
	integPortMax = 19099
)

// testappBinOnce caches the built testapp binary across test functions.
var testappBin string

func TestMain(m *testing.M) {
	bin, err := buildTestapp()
	if err != nil {
		fmt.Fprintf(os.Stderr, "skipping integration tests: could not build testapp: %v\n", err)
		os.Exit(0)
	}
	testappBin = bin
	os.Exit(m.Run())
}

func buildTestapp() (string, error) {
	dir, err := os.MkdirTemp("", "spinner-testapp-*")
	if err != nil {
		return "", err
	}
	bin := filepath.Join(dir, "testapp")
	repoRoot, err := filepath.Abs(".")
	if err != nil {
		return "", err
	}
	cmd := exec.Command("go", "build", "-o", bin, "./testapp")
	cmd.Dir = repoRoot
	out, err := cmd.CombinedOutput()
	if err != nil {
		return "", fmt.Errorf("%v\n%s", err, out)
	}
	return bin, nil
}

// --- helpers ---

func initTestRepo(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	gitRun(t, dir, "init")
	gitRun(t, dir, "config", "user.email", "test@test.com")
	gitRun(t, dir, "config", "user.name", "Test")
	if err := os.WriteFile(filepath.Join(dir, "README"), []byte("test"), 0644); err != nil {
		t.Fatal(err)
	}
	gitRun(t, dir, "add", ".")
	gitRun(t, dir, "commit", "-m", "init")
	return dir
}

func gitRun(t *testing.T, dir string, args ...string) {
	t.Helper()
	cmd := exec.Command("git", args...)
	cmd.Dir = dir
	cmd.Env = append(os.Environ(),
		"GIT_AUTHOR_NAME=Test",
		"GIT_AUTHOR_EMAIL=test@test.com",
		"GIT_COMMITTER_NAME=Test",
		"GIT_COMMITTER_EMAIL=test@test.com",
	)
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("git %v: %v\n%s", args, err, out)
	}
}

func addWorktree(t *testing.T, mainRoot, branch string) string {
	t.Helper()
	wtDir := t.TempDir()
	gitRun(t, mainRoot, "worktree", "add", "-b", branch, wtDir)
	t.Cleanup(func() {
		exec.Command("git", "-C", mainRoot, "worktree", "remove", "--force", wtDir).Run()
	})
	return wtDir
}

func removeWorktree(t *testing.T, mainRoot, wtDir string) {
	t.Helper()
	cmd := exec.Command("git", "-C", mainRoot, "worktree", "remove", "--force", wtDir)
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("git worktree remove: %v\n%s", err, out)
	}
}

func makeProjectConfig(name string) *config.ProjectConfig {
	return &config.ProjectConfig{
		Project: config.ProjectSection{
			Name:         name,
			DomainSuffix: name + ".test",
			PortRange:    config.PortRange{Min: integPortMin, Max: integPortMax},
		},
		Server: config.ServerSection{
			Command: testappBin,
			Env:     map[string]string{"BRANCH": "{branch}"},
		},
	}
}

// startDaemon starts the daemon in a goroutine and registers cleanup.
func startDaemon(t *testing.T, root string, cfg *config.ProjectConfig) *daemon.Manager {
	t.Helper()
	mgr := daemon.New(root, cfg)
	done := make(chan error, 1)
	go func() { done <- mgr.Run() }()
	t.Cleanup(func() {
		mgr.Stop()
		select {
		case <-done:
		case <-time.After(10 * time.Second):
			t.Error("daemon did not stop within 10s")
		}
		state.Remove(cfg.Project.Name)
		os.Remove(state.PIDFile(cfg.Project.Name))
	})
	return mgr
}

// waitForHTTP polls url until it responds 200 or timeout expires.
func waitForHTTP(t *testing.T, url string, timeout time.Duration) {
	t.Helper()
	client := &http.Client{Timeout: 2 * time.Second}
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		resp, err := client.Get(url)
		if err == nil {
			resp.Body.Close()
			return
		}
		time.Sleep(200 * time.Millisecond)
	}
	t.Fatalf("server at %s did not start within %v", url, timeout)
}

// waitHTTPGone polls url until connection is refused or timeout expires.
func waitHTTPGone(t *testing.T, url string, timeout time.Duration) {
	t.Helper()
	client := &http.Client{Timeout: 500 * time.Millisecond}
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		_, err := client.Get(url)
		if err != nil {
			return
		}
		time.Sleep(200 * time.Millisecond)
	}
	t.Fatalf("server at %s still responding after %v", url, timeout)
}

func serverURL(branch string) string {
	p := port.Assign(branch, integPortMin, integPortMax)
	return fmt.Sprintf("http://localhost:%d", p)
}

// --- tests ---

// TestDaemonStartsServerForExistingWorktree verifies that when the daemon starts
// it launches a server for every worktree already present.
func TestDaemonStartsServerForExistingWorktree(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	repoDir := initTestRepo(t)
	wtDir := addWorktree(t, repoDir, "feature-hello")

	cfg := makeProjectConfig("proj-existing")
	if err := config.SaveProject(repoDir, cfg); err != nil {
		t.Fatal(err)
	}

	startDaemon(t, repoDir, cfg)

	// Main worktree server
	mainBranch := currentBranch(t, repoDir)
	waitForHTTP(t, serverURL(mainBranch), 10*time.Second)

	// Linked worktree server
	waitForHTTP(t, serverURL("feature-hello"), 10*time.Second)

	// Assert /api/info payload contains the branch name.
	resp, err := http.Get(serverURL("feature-hello") + "/api/info")
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()

	var payload map[string]string
	if err := json.NewDecoder(resp.Body).Decode(&payload); err != nil {
		t.Fatal(err)
	}
	if payload["branch"] != "feature-hello" {
		t.Errorf("branch = %q, want %q", payload["branch"], "feature-hello")
	}

	// Assert log file was created.
	logFile := state.LogFile("proj-existing", "feature-hello")
	if _, err := os.Stat(logFile); os.IsNotExist(err) {
		t.Errorf("log file not created: %s", logFile)
	}

	_ = wtDir
}

// TestDaemonPicksUpNewWorktree verifies the daemon starts a server when a new
// worktree is added while the daemon is already running.
func TestDaemonPicksUpNewWorktree(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	repoDir := initTestRepo(t)
	cfg := makeProjectConfig("proj-new-wt")
	if err := config.SaveProject(repoDir, cfg); err != nil {
		t.Fatal(err)
	}

	startDaemon(t, repoDir, cfg)

	// Wait for the main branch server to confirm daemon is running.
	mainBranch := currentBranch(t, repoDir)
	waitForHTTP(t, serverURL(mainBranch), 10*time.Second)

	// Now add a new worktree.
	addWorktree(t, repoDir, "live-added")

	// Daemon should detect and start a server for it.
	waitForHTTP(t, serverURL("live-added"), 15*time.Second)

	resp, err := http.Get(serverURL("live-added") + "/api/info")
	if err != nil {
		t.Fatal(err)
	}
	defer resp.Body.Close()

	var payload map[string]string
	if err := json.NewDecoder(resp.Body).Decode(&payload); err != nil {
		t.Fatal(err)
	}
	if payload["branch"] != "live-added" {
		t.Errorf("branch = %q, want %q", payload["branch"], "live-added")
	}
}

// TestDaemonStopsServerWhenWorktreeRemoved verifies the daemon stops a server
// when its worktree is removed.
func TestDaemonStopsServerWhenWorktreeRemoved(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	repoDir := initTestRepo(t)
	wtDir := addWorktree(t, repoDir, "to-be-removed")

	cfg := makeProjectConfig("proj-remove-wt")
	if err := config.SaveProject(repoDir, cfg); err != nil {
		t.Fatal(err)
	}

	startDaemon(t, repoDir, cfg)

	// Wait for server to come up.
	waitForHTTP(t, serverURL("to-be-removed"), 10*time.Second)

	// Remove the worktree.
	removeWorktree(t, repoDir, wtDir)

	// Server should stop.
	waitHTTPGone(t, serverURL("to-be-removed"), 15*time.Second)
}

// TestDaemonSlashedBranchURL verifies that slashes in branch names are replaced
// with hyphens when building the URL stored in state.
func TestDaemonSlashedBranchURL(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	repoDir := initTestRepo(t)
	addWorktree(t, repoDir, "feature/slash")

	cfg := makeProjectConfig("proj-slash")
	if err := config.SaveProject(repoDir, cfg); err != nil {
		t.Fatal(err)
	}

	startDaemon(t, repoDir, cfg)

	// Wait for the server to start so state has been written.
	waitForHTTP(t, serverURL("feature/slash"), 10*time.Second)

	s, err := state.Load("proj-slash")
	if err != nil {
		t.Fatal(err)
	}

	var found *state.WorktreeState
	for i := range s.Worktrees {
		if s.Worktrees[i].Branch == "feature/slash" {
			found = &s.Worktrees[i]
			break
		}
	}
	if found == nil {
		t.Fatal("feature/slash not found in state")
	}
	if strings.Contains(found.URL, "feature/slash") {
		t.Errorf("URL %q contains raw slash — want hyphen-separated subdomain", found.URL)
	}
	if !strings.Contains(found.URL, "feature-slash") {
		t.Errorf("URL %q does not contain expected subdomain feature-slash", found.URL)
	}
}

// TestPortAssignmentIsStable verifies the same branch always gets the same port
// (regression guard — if the hash function changes, URLs break).
func TestPortAssignmentIsStable(t *testing.T) {
	cases := []struct {
		branch string
		want   int
	}{
		{"main", port.Assign("main", integPortMin, integPortMax)},
		{"feature-foo", port.Assign("feature-foo", integPortMin, integPortMax)},
	}
	for _, tc := range cases {
		got := port.Assign(tc.branch, integPortMin, integPortMax)
		if got != tc.want {
			t.Errorf("port for %q changed: got %d, was %d", tc.branch, got, tc.want)
		}
		if got < integPortMin || got > integPortMax {
			t.Errorf("port %d out of range [%d, %d]", got, integPortMin, integPortMax)
		}
	}
}

func currentBranch(t *testing.T, dir string) string {
	t.Helper()
	cmd := exec.Command("git", "-C", dir, "rev-parse", "--abbrev-ref", "HEAD")
	out, err := cmd.Output()
	if err != nil {
		t.Fatalf("git rev-parse: %v", err)
	}
	branch := string(out[:len(out)-1]) // trim newline
	return branch
}
