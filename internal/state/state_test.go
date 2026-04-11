package state_test

import (
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/bmiller/spinner/internal/state"
)

// stateDir returns a temporary state directory and patches config.StateDir for the test.
// It sets XDG_DATA_HOME so config.StateDir returns a path inside t.TempDir().
func stateDir(t *testing.T, projectName string) string {
	t.Helper()
	tmp := t.TempDir()
	t.Setenv("HOME", tmp)
	return filepath.Join(tmp, ".local", "share", "spinner", projectName)
}

func TestLoadEmptyState(t *testing.T) {
	stateDir(t, "proj")
	s, err := state.Load("proj")
	if err != nil {
		t.Fatalf("Load on non-existent file: %v", err)
	}
	if s.Project != "proj" {
		t.Errorf("Project = %q, want %q", s.Project, "proj")
	}
	if len(s.Worktrees) != 0 {
		t.Errorf("expected no worktrees, got %d", len(s.Worktrees))
	}
}

func TestSaveLoad(t *testing.T) {
	stateDir(t, "proj")

	original := &state.SpinnerState{
		Project:   "proj",
		Root:      "/some/path",
		DaemonPID: 12345,
		Worktrees: []state.WorktreeState{
			{
				Branch:      "main",
				Path:        "/some/path",
				Port:        4100,
				URL:         "http://main.proj.test:4100",
				Status:      state.StatusRunning,
				SetupStatus: state.SetupStatusOK,
			},
		},
	}

	if err := state.Save(original); err != nil {
		t.Fatalf("Save: %v", err)
	}

	loaded, err := state.Load("proj")
	if err != nil {
		t.Fatalf("Load: %v", err)
	}

	if loaded.Project != original.Project {
		t.Errorf("Project = %q, want %q", loaded.Project, original.Project)
	}
	if loaded.DaemonPID != original.DaemonPID {
		t.Errorf("DaemonPID = %d, want %d", loaded.DaemonPID, original.DaemonPID)
	}
	if len(loaded.Worktrees) != 1 {
		t.Fatalf("expected 1 worktree, got %d", len(loaded.Worktrees))
	}
	wt := loaded.Worktrees[0]
	if wt.Branch != "main" {
		t.Errorf("Branch = %q, want %q", wt.Branch, "main")
	}
	if wt.SetupStatus != state.SetupStatusOK {
		t.Errorf("SetupStatus = %q, want %q", wt.SetupStatus, state.SetupStatusOK)
	}
}

func TestSetWorktreeSetupStatus_ExistingBranch(t *testing.T) {
	stateDir(t, "proj")

	// Seed state with a running worktree.
	initial := &state.SpinnerState{
		Project: "proj",
		Worktrees: []state.WorktreeState{
			{Branch: "main", Status: state.StatusRunning, SetupStatus: state.SetupStatusPending},
		},
	}
	if err := state.Save(initial); err != nil {
		t.Fatalf("Save: %v", err)
	}

	if err := state.SetWorktreeSetupStatus("proj", "main", state.SetupStatusOK); err != nil {
		t.Fatalf("SetWorktreeSetupStatus: %v", err)
	}

	loaded, _ := state.Load("proj")
	if len(loaded.Worktrees) != 1 {
		t.Fatalf("expected 1 worktree, got %d", len(loaded.Worktrees))
	}
	wt := loaded.Worktrees[0]
	if wt.SetupStatus != state.SetupStatusOK {
		t.Errorf("SetupStatus = %q, want ok", wt.SetupStatus)
	}
	// Server status must be preserved.
	if wt.Status != state.StatusRunning {
		t.Errorf("Status = %q, want running (server status must be preserved)", wt.Status)
	}
	if wt.SetupAt.IsZero() {
		t.Error("SetupAt should be set")
	}
}

func TestSetWorktreeSetupStatus_NewBranch(t *testing.T) {
	stateDir(t, "proj")

	if err := state.SetWorktreeSetupStatus("proj", "feat", state.SetupStatusPending); err != nil {
		t.Fatalf("SetWorktreeSetupStatus: %v", err)
	}

	loaded, _ := state.Load("proj")
	if len(loaded.Worktrees) != 1 {
		t.Fatalf("expected 1 worktree, got %d", len(loaded.Worktrees))
	}
	if loaded.Worktrees[0].SetupStatus != state.SetupStatusPending {
		t.Errorf("SetupStatus = %q, want pending", loaded.Worktrees[0].SetupStatus)
	}
}

func TestGetWorktreeSetupStatus(t *testing.T) {
	stateDir(t, "proj")

	// Not found returns None.
	if st := state.GetWorktreeSetupStatus("proj", "missing"); st != state.SetupStatusNone {
		t.Errorf("missing branch: got %q, want empty", st)
	}

	_ = state.SetWorktreeSetupStatus("proj", "main", state.SetupStatusFailed)
	if st := state.GetWorktreeSetupStatus("proj", "main"); st != state.SetupStatusFailed {
		t.Errorf("got %q, want failed", st)
	}
}

func TestClearDaemon(t *testing.T) {
	stateDir(t, "proj")

	s := &state.SpinnerState{
		Project:   "proj",
		DaemonPID: 9999,
		Worktrees: []state.WorktreeState{
			{Branch: "main", Status: state.StatusRunning, PID: 1234, SetupStatus: state.SetupStatusOK},
			{Branch: "feat", Status: state.StatusRunning, PID: 5678, SetupStatus: state.SetupStatusPending},
		},
	}
	if err := state.Save(s); err != nil {
		t.Fatalf("Save: %v", err)
	}

	if err := state.ClearDaemon("proj"); err != nil {
		t.Fatalf("ClearDaemon: %v", err)
	}

	loaded, _ := state.Load("proj")
	if loaded.DaemonPID != 0 {
		t.Errorf("DaemonPID = %d, want 0", loaded.DaemonPID)
	}
	for _, wt := range loaded.Worktrees {
		if wt.Status != state.StatusStopped {
			t.Errorf("branch %s: Status = %q, want stopped", wt.Branch, wt.Status)
		}
		if wt.PID != 0 {
			t.Errorf("branch %s: PID = %d, want 0", wt.Branch, wt.PID)
		}
	}
	// Setup status must survive ClearDaemon.
	if loaded.Worktrees[0].SetupStatus != state.SetupStatusOK {
		t.Errorf("main SetupStatus = %q, want ok (must survive ClearDaemon)", loaded.Worktrees[0].SetupStatus)
	}
	if loaded.Worktrees[1].SetupStatus != state.SetupStatusPending {
		t.Errorf("feat SetupStatus = %q, want pending (must survive ClearDaemon)", loaded.Worktrees[1].SetupStatus)
	}
}

func TestSaveIsAtomic(t *testing.T) {
	stateDir(t, "proj")

	s := &state.SpinnerState{Project: "proj", Root: "/tmp/proj"}
	if err := state.Save(s); err != nil {
		t.Fatalf("Save: %v", err)
	}

	// No .tmp file should linger after a successful save.
	dir := filepath.Join(os.Getenv("HOME"), ".local", "share", "spinner", "proj")
	entries, _ := os.ReadDir(dir)
	for _, e := range entries {
		if filepath.Ext(e.Name()) == ".tmp" {
			t.Errorf("leftover tmp file: %s", e.Name())
		}
	}
}

func TestUpdateAtIsSet(t *testing.T) {
	stateDir(t, "proj")

	before := time.Now()
	s := &state.SpinnerState{Project: "proj"}
	if err := state.Save(s); err != nil {
		t.Fatalf("Save: %v", err)
	}
	after := time.Now()

	loaded, _ := state.Load("proj")
	if loaded.UpdatedAt.Before(before) || loaded.UpdatedAt.After(after) {
		t.Errorf("UpdatedAt %v not in expected range [%v, %v]", loaded.UpdatedAt, before, after)
	}
}
