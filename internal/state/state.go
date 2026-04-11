package state

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"syscall"
	"time"

	"github.com/bmiller/spinner/internal/config"
)

// Status is the runtime status of a worktree's server process.
type Status string

const (
	StatusRunning Status = "running"
	StatusStopped Status = "stopped"
	StatusError   Status = "error"
)

// SetupStatus is the lifecycle status of a worktree's setup command.
type SetupStatus string

const (
	SetupStatusNone    SetupStatus = ""         // no setup command configured
	SetupStatusPending SetupStatus = "pending"  // not yet run
	SetupStatusRunning SetupStatus = "running"  // currently running
	SetupStatusOK      SetupStatus = "ok"       // last run succeeded
	SetupStatusFailed  SetupStatus = "failed"   // last run failed
)

// WorktreeState holds all runtime state for a single worktree.
// Server fields are written by the daemon; setup fields are written by the CLI.
type WorktreeState struct {
	Branch  string `json:"branch"`
	Path    string `json:"path"`

	// Server state (written by daemon on each tick).
	Port      int       `json:"port,omitempty"`
	URL       string    `json:"url,omitempty"`
	PID       int       `json:"pid,omitempty"`
	Status    Status    `json:"status"`
	LogFile   string    `json:"log_file,omitempty"`
	StartedAt time.Time `json:"started_at,omitempty"`

	// Setup state (written by `spinner setup`; persists across daemon restarts).
	SetupStatus SetupStatus `json:"setup_status,omitempty"`
	SetupAt     time.Time   `json:"setup_at,omitempty"`
}

// SpinnerState is the single persistent state file for a project.
// Stored at ~/.local/share/spinner/<project>/spinner-state.json.
type SpinnerState struct {
	Project   string          `json:"project"`
	Root      string          `json:"root"`
	DaemonPID int             `json:"daemon_pid,omitempty"`
	UpdatedAt time.Time       `json:"updated_at"`
	Worktrees []WorktreeState `json:"worktrees"`
}

func stateFile(projectName string) string {
	return filepath.Join(config.StateDir(projectName), "spinner-state.json")
}

// Load reads state for a project. Returns empty state if file doesn't exist.
func Load(projectName string) (*SpinnerState, error) {
	path := stateFile(projectName)
	data, err := os.ReadFile(path)
	if os.IsNotExist(err) {
		return &SpinnerState{Project: projectName}, nil
	}
	if err != nil {
		return nil, fmt.Errorf("reading state: %w", err)
	}
	var s SpinnerState
	if err := json.Unmarshal(data, &s); err != nil {
		return nil, fmt.Errorf("parsing state: %w", err)
	}
	return &s, nil
}

// Save writes state for a project atomically.
func Save(s *SpinnerState) error {
	dir := config.StateDir(s.Project)
	if err := os.MkdirAll(dir, 0755); err != nil {
		return err
	}
	s.UpdatedAt = time.Now()
	data, err := json.MarshalIndent(s, "", "  ")
	if err != nil {
		return err
	}
	tmp := stateFile(s.Project) + ".tmp"
	if err := os.WriteFile(tmp, data, 0644); err != nil {
		return err
	}
	return os.Rename(tmp, stateFile(s.Project))
}

// ClearDaemon marks all servers as stopped and clears the daemon PID.
// Called on daemon exit so state reflects reality without deleting setup status.
func ClearDaemon(projectName string) error {
	s, err := Load(projectName)
	if err != nil {
		return err
	}
	s.DaemonPID = 0
	for i := range s.Worktrees {
		s.Worktrees[i].Status = StatusStopped
		s.Worktrees[i].PID = 0
		s.Worktrees[i].StartedAt = time.Time{}
	}
	return Save(s)
}

// Remove deletes the state file entirely. Used for full project cleanup.
func Remove(projectName string) error {
	err := os.Remove(stateFile(projectName))
	if os.IsNotExist(err) {
		return nil
	}
	return err
}

// SetWorktreeSetupStatus updates the setup status for a branch in the state file.
// Creates an entry for the branch if one doesn't exist yet.
func SetWorktreeSetupStatus(projectName, branch string, status SetupStatus) error {
	s, err := Load(projectName)
	if err != nil {
		return err
	}
	for i := range s.Worktrees {
		if s.Worktrees[i].Branch == branch {
			s.Worktrees[i].SetupStatus = status
			s.Worktrees[i].SetupAt = time.Now()
			return Save(s)
		}
	}
	// Branch not yet in state (daemon not running); create a minimal entry.
	s.Worktrees = append(s.Worktrees, WorktreeState{
		Branch:      branch,
		SetupStatus: status,
		SetupAt:     time.Now(),
	})
	return Save(s)
}

// GetWorktreeSetupStatus returns the setup status for a branch.
// Returns SetupStatusNone if the branch isn't found.
func GetWorktreeSetupStatus(projectName, branch string) SetupStatus {
	s, err := Load(projectName)
	if err != nil {
		return SetupStatusNone
	}
	for _, wt := range s.Worktrees {
		if wt.Branch == branch {
			return wt.SetupStatus
		}
	}
	return SetupStatusNone
}

// LogFile returns the server log file path for a worktree.
func LogFile(projectName, branch string) string {
	return filepath.Join(config.StateDir(projectName), "logs", branch+".log")
}

// SetupLogFile returns the setup log file path for a worktree.
func SetupLogFile(projectName, branch string) string {
	return filepath.Join(config.StateDir(projectName), "logs", "spinner-setup-"+branch+".log")
}

// PIDFile returns the daemon PID file path for a project.
func PIDFile(projectName string) string {
	return filepath.Join(config.StateDir(projectName), "daemon.pid")
}

// WritePID writes the daemon PID to disk.
func WritePID(projectName string, pid int) error {
	dir := config.StateDir(projectName)
	if err := os.MkdirAll(dir, 0755); err != nil {
		return err
	}
	return os.WriteFile(PIDFile(projectName), []byte(fmt.Sprintf("%d\n", pid)), 0644)
}

// ReadPID reads the daemon PID from disk. Returns 0 if not found.
func ReadPID(projectName string) (int, error) {
	data, err := os.ReadFile(PIDFile(projectName))
	if os.IsNotExist(err) {
		return 0, nil
	}
	if err != nil {
		return 0, err
	}
	var pid int
	fmt.Sscanf(string(data), "%d", &pid)
	return pid, nil
}

// RemovePID deletes the daemon PID file.
func RemovePID(projectName string) error {
	return os.Remove(PIDFile(projectName))
}

// IsRunning returns true if a daemon is already running for this project.
func IsRunning(projectName string) bool {
	pid, err := ReadPID(projectName)
	if err != nil || pid == 0 {
		return false
	}
	return syscall.Kill(pid, 0) == nil
}
