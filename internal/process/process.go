package process

import (
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/creack/pty"
)

// Server manages a single dev server process running under a PTY.
type Server struct {
	Branch  string
	Command string
	Env     map[string]string
	Dir     string
	LogFile string

	cmd  *exec.Cmd
	ptmx *os.File
	mu   sync.Mutex
	done chan struct{}
}

// Start launches the server process.
func (s *Server) Start() error {
	s.mu.Lock()
	defer s.mu.Unlock()

	if err := os.MkdirAll(filepath.Dir(s.LogFile), 0755); err != nil {
		return fmt.Errorf("creating log dir: %w", err)
	}
	logf, err := os.OpenFile(s.LogFile, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0644)
	if err != nil {
		return fmt.Errorf("opening log file: %w", err)
	}

	parts := strings.Fields(s.Command)
	if len(parts) == 0 {
		logf.Close()
		return fmt.Errorf("empty command")
	}

	s.cmd = exec.Command(parts[0], parts[1:]...)
	s.cmd.Dir = s.Dir
	s.cmd.Env = buildEnv(s.Env)

	ptmx, err := pty.Start(s.cmd)
	if err != nil {
		logf.Close()
		return fmt.Errorf("starting pty: %w", err)
	}
	s.ptmx = ptmx
	s.done = make(chan struct{})

	go func() {
		defer logf.Close()
		defer close(s.done)
		io.Copy(logf, ptmx)
	}()

	return nil
}

// PID returns the process ID, or 0 if not running.
func (s *Server) PID() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.cmd == nil || s.cmd.Process == nil {
		return 0
	}
	return s.cmd.Process.Pid
}

// Done returns a channel that is closed when the process exits.
func (s *Server) Done() <-chan struct{} {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.done
}

// IsAlive reports whether the managed process is currently running.
func (s *Server) IsAlive() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.done == nil {
		return false
	}
	select {
	case <-s.done:
		return false
	default:
		return true
	}
}

// Stop sends SIGTERM to the process and waits for it to exit.
func (s *Server) Stop() error {
	s.mu.Lock()
	ptmx := s.ptmx
	cmd := s.cmd
	done := s.done
	s.mu.Unlock()

	if cmd == nil || cmd.Process == nil {
		return nil
	}

	cmd.Process.Signal(syscall.SIGTERM)
	if done != nil {
		select {
		case <-done:
		case <-time.After(3 * time.Second):
			cmd.Process.Kill()
			<-done
		}
	}
	ptmx.Close()
	cmd.Wait()
	return nil
}

// Tail returns a reader that streams from the log file starting at the end.
// The caller is responsible for closing the returned file.
func TailLog(logFile string) (*os.File, error) {
	f, err := os.Open(logFile)
	if err != nil {
		return nil, err
	}
	f.Seek(0, io.SeekEnd)
	return f, nil
}

// buildEnv merges provided env vars over the current process environment.
func buildEnv(extra map[string]string) []string {
	base := os.Environ()
	overrides := make(map[string]string, len(extra))
	for k, v := range extra {
		overrides[k] = v
	}

	var result []string
	for _, e := range base {
		key := e[:strings.IndexByte(e, '=')]
		if _, ok := overrides[key]; !ok {
			result = append(result, e)
		}
	}
	for k, v := range overrides {
		result = append(result, k+"="+v)
	}
	return result
}
