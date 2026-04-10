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

type Status string

const (
	StatusRunning Status = "running"
	StatusStopped Status = "stopped"
	StatusError   Status = "error"
)

// WorktreeState holds runtime state for a single worktree's server.
type WorktreeState struct {
	Branch    string    `json:"branch"`
	Path      string    `json:"path"`
	Port      int       `json:"port"`
	URL       string    `json:"url"`
	PID       int       `json:"pid"`
	Status    Status    `json:"status"`
	LogFile   string    `json:"log_file"`
	StartedAt time.Time `json:"started_at,omitempty"`
}

// ProjectState holds runtime state for a single registered project.
type ProjectState struct {
	Project   string          `json:"project"`
	Root      string          `json:"root"`
	DaemonPID int             `json:"daemon_pid"`
	UpdatedAt time.Time       `json:"updated_at"`
	Worktrees []WorktreeState `json:"worktrees"`
}

func stateFile(projectName string) string {
	return filepath.Join(config.StateDir(projectName), "state.json")
}

// Load reads state for a project. Returns empty state if file doesn't exist.
func Load(projectName string) (*ProjectState, error) {
	path := stateFile(projectName)
	data, err := os.ReadFile(path)
	if os.IsNotExist(err) {
		return &ProjectState{Project: projectName}, nil
	}
	if err != nil {
		return nil, fmt.Errorf("reading state: %w", err)
	}
	var s ProjectState
	if err := json.Unmarshal(data, &s); err != nil {
		return nil, fmt.Errorf("parsing state: %w", err)
	}
	return &s, nil
}

// Save writes state for a project atomically.
func Save(s *ProjectState) error {
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

// Remove deletes the state file for a project.
func Remove(projectName string) error {
	return os.Remove(stateFile(projectName))
}

// LogFile returns the log file path for a worktree.
func LogFile(projectName, branch string) string {
	return filepath.Join(config.StateDir(projectName), "logs", branch+".log")
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
