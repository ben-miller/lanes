package daemon

import (
	"fmt"
	"log"
	"os"
	"os/signal"
	"path/filepath"
	"sync"
	"syscall"
	"time"

	"github.com/bmiller/spinner/internal/config"
	"github.com/bmiller/spinner/internal/git"
	"github.com/bmiller/spinner/internal/port"
	"github.com/bmiller/spinner/internal/process"
	"github.com/bmiller/spinner/internal/state"
	"github.com/bmiller/spinner/internal/watcher"
)

// Manager runs as the long-lived daemon process for a single project.
type Manager struct {
	cfg         *config.ProjectConfig
	projectRoot string
	mu          sync.Mutex
	servers     map[string]*process.Server // branch -> server
	stop        chan struct{}
}

func New(projectRoot string, cfg *config.ProjectConfig) *Manager {
	return &Manager{
		cfg:         cfg,
		projectRoot: projectRoot,
		servers:     make(map[string]*process.Server),
		stop:        make(chan struct{}),
	}
}

// Run starts the manager. It blocks until a signal is received or Stop is called.
func (m *Manager) Run() error {
	projectName := m.cfg.Project.Name

	// Write our PID.
	if err := state.WritePID(projectName, os.Getpid()); err != nil {
		return fmt.Errorf("writing pid: %w", err)
	}
	defer state.RemovePID(projectName)
	defer state.Remove(projectName)

	// Start servers for all existing worktrees.
	worktrees, err := git.ListWorktrees(m.projectRoot)
	if err != nil {
		return fmt.Errorf("listing worktrees: %w", err)
	}
	for _, wt := range worktrees {
		if err := m.startWorktree(wt); err != nil {
			log.Printf("spinner: failed to start %s: %v", wt.Branch, err)
		}
	}
	m.saveState()

	// Watch for worktree changes.
	watchDir := git.WorktreesDir(m.projectRoot)
	if err := git.EnsureWorktreesDirExists(m.projectRoot); err != nil {
		log.Printf("spinner: could not ensure worktrees dir: %v", err)
	}
	events := make(chan watcher.Event, 16)
	go watcher.Watch(watchDir, events, m.stop)

	// Handle OS signals.
	sigs := make(chan os.Signal, 1)
	signal.Notify(sigs, syscall.SIGTERM, syscall.SIGINT)

	ticker := time.NewTicker(5 * time.Second)
	defer ticker.Stop()

	for {
		select {
		case <-sigs:
			m.shutdown()
			return nil
		case <-m.stop:
			m.shutdown()
			return nil
		case ev := <-events:
			m.handleWatchEvent(ev)
		case <-ticker.C:
			m.saveState()
		}
	}
}

// Stop signals the manager to shut down.
func (m *Manager) Stop() {
	close(m.stop)
}

func (m *Manager) startWorktree(wt git.Worktree) error {
	if wt.Branch == "" {
		return nil // detached HEAD, skip
	}

	p := port.Assign(wt.Branch, m.cfg.Project.PortRange.Min, m.cfg.Project.PortRange.Max)
	env := config.ExpandEnv(m.cfg.Server.Env, wt.Branch)
	env["PORT"] = fmt.Sprintf("%d", p)

	logFile := state.LogFile(m.cfg.Project.Name, wt.Branch)

	srv := &process.Server{
		Branch:  wt.Branch,
		Command: m.cfg.Server.Command,
		Env:     env,
		Dir:     wt.Path,
		LogFile: logFile,
	}

	if err := srv.Start(); err != nil {
		return err
	}

	m.mu.Lock()
	m.servers[wt.Branch] = srv
	m.mu.Unlock()

	return nil
}

func (m *Manager) stopWorktree(branch string) {
	m.mu.Lock()
	srv, ok := m.servers[branch]
	if ok {
		delete(m.servers, branch)
	}
	m.mu.Unlock()

	if ok {
		if err := srv.Stop(); err != nil {
			log.Printf("spinner: error stopping %s: %v", branch, err)
		}
		log.Printf("spinner: stopped %s", branch)
	}
}

func (m *Manager) handleWatchEvent(ev watcher.Event) {
	// ev.Name is the full path to the entry under .git/worktrees/
	// We need to re-list worktrees to get the branch name.
	worktrees, err := git.ListWorktrees(m.projectRoot)
	if err != nil {
		log.Printf("spinner: listing worktrees after event: %v", err)
		return
	}

	if ev.Removed {
		// Find which branch was removed by checking what we're running vs what exists now.
		existing := map[string]bool{}
		for _, wt := range worktrees {
			existing[wt.Branch] = true
		}
		m.mu.Lock()
		var gone []string
		for branch := range m.servers {
			if !existing[branch] {
				gone = append(gone, branch)
			}
		}
		m.mu.Unlock()
		for _, branch := range gone {
			m.stopWorktree(branch)
		}
	} else {
		// A new entry appeared under .git/worktrees/. git may still be writing
		// the worktree metadata, so retry until ListWorktrees shows the new branch.
		for attempt := 0; attempt < 10; attempt++ {
			if attempt > 0 {
				time.Sleep(250 * time.Millisecond)
				worktrees, _ = git.ListWorktrees(m.projectRoot)
			}
			m.mu.Lock()
			running := map[string]bool{}
			for branch := range m.servers {
				running[branch] = true
			}
			m.mu.Unlock()

			var newWTs []git.Worktree
			for _, wt := range worktrees {
				if !running[wt.Branch] && wt.Branch != "" {
					newWTs = append(newWTs, wt)
				}
			}
			if len(newWTs) == 0 {
				continue
			}
			for _, wt := range newWTs {
				if err := m.startWorktree(wt); err != nil {
					log.Printf("spinner: failed to start new worktree %s: %v", wt.Branch, err)
				}
			}
			break
		}
	}
	m.saveState()
}

func (m *Manager) shutdown() {
	m.mu.Lock()
	branches := make([]string, 0, len(m.servers))
	for branch := range m.servers {
		branches = append(branches, branch)
	}
	m.mu.Unlock()

	var wg sync.WaitGroup
	for _, branch := range branches {
		wg.Add(1)
		go func(b string) {
			defer wg.Done()
			m.stopWorktree(b)
		}(branch)
	}
	wg.Wait()
}

func (m *Manager) saveState() {
	worktrees, _ := git.ListWorktrees(m.projectRoot)
	wtMap := map[string]git.Worktree{}
	for _, wt := range worktrees {
		wtMap[wt.Branch] = wt
	}

	m.mu.Lock()
	defer m.mu.Unlock()

	var wtStates []state.WorktreeState
	for branch, srv := range m.servers {
		p := port.Assign(branch, m.cfg.Project.PortRange.Min, m.cfg.Project.PortRange.Max)
		url := fmt.Sprintf("http://%s.%s:%d", branch, m.cfg.Project.DomainSuffix, p)
		wt := wtMap[branch]
		wtStates = append(wtStates, state.WorktreeState{
			Branch:  branch,
			Path:    wt.Path,
			Port:    p,
			URL:     url,
			PID:     srv.PID(),
			Status:  state.StatusRunning,
			LogFile: filepath.Join(state.LogFile(m.cfg.Project.Name, branch)),
		})
	}

	s := &state.ProjectState{
		Project:   m.cfg.Project.Name,
		Root:      m.projectRoot,
		DaemonPID: os.Getpid(),
		Worktrees: wtStates,
	}
	if err := state.Save(s); err != nil {
		log.Printf("spinner: saving state: %v", err)
	}
}
