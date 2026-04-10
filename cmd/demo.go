package cmd

import (
	"fmt"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"syscall"

	"github.com/bmiller/spinner/internal/config"
	"github.com/bmiller/spinner/internal/daemon"
	"github.com/bmiller/spinner/internal/dashboard"
	"github.com/bmiller/spinner/internal/dnsmasq"
	"github.com/bmiller/spinner/internal/port"
	"github.com/spf13/cobra"
)

// Demo branches spun up by `spinner demo`.
var demoBranches = []string{"branch-a", "branch-b", "branch-c"}

const demoPortMin = 5100
const demoPortMax = 5199

func newDemoCmd() *cobra.Command {
	return &cobra.Command{
		Use:   "demo",
		Short: "Start a local demo using testapp",
		Long: `Sets up ~/spinner-demo as a persistent git repo with a few worktrees,
configures dnsmasq for demo.test (requires one sudo prompt), builds testapp,
and runs the daemon in the foreground.

Safe to run multiple times — skips setup steps that are already done.`,
		RunE: runDemo,
	}
}

func runDemo(cmd *cobra.Command, args []string) error {
	if os.Getuid() != 0 {
		return fmt.Errorf("demo requires sudo to configure DNS\n\n  Run: sudo go run . demo\n  Or:  sudo spinner demo")
	}

	home, err := os.UserHomeDir()
	if err != nil {
		return err
	}

	demoDir := filepath.Join(home, "spinner-demo")
	testappBin := filepath.Join(demoDir, "testapp")

	// --- Step 1: build testapp ---
	fmt.Println("Building testapp...")
	repoRoot, err := findRepoRoot()
	if err != nil {
		return fmt.Errorf("run `spinner demo` from the spinner repo root: %w", err)
	}
	build := exec.Command("go", "build", "-o", testappBin, "./testapp")
	build.Dir = repoRoot
	build.Stdout = os.Stdout
	build.Stderr = os.Stderr
	if err := build.Run(); err != nil {
		return fmt.Errorf("building testapp: %w", err)
	}

	// --- Step 2: init git repo ---
	if err := ensureGitRepo(demoDir); err != nil {
		return err
	}

	// --- Step 3: write spinner.toml ---
	cfg := &config.ProjectConfig{
		Project: config.ProjectSection{
			Name:         "demo",
			DomainSuffix: "demo.test",
			PortRange:    config.PortRange{Min: demoPortMin, Max: demoPortMax},
		},
		Server: config.ServerSection{
			Command: testappBin,
			Env:     map[string]string{"BRANCH": "{branch}"},
		},
	}
	if err := config.SaveProject(demoDir, cfg); err != nil {
		return fmt.Errorf("writing spinner.toml: %w", err)
	}

	// --- Step 4: configure dnsmasq ---
	if err := dnsmasq.EnsureProject("demo.test"); err != nil {
		return fmt.Errorf("configuring DNS: %w", err)
	}
	if !dnsmasq.IsDashboardConfigured() {
		if err := dnsmasq.EnsureDashboard(); err != nil {
			return fmt.Errorf("configuring dashboard DNS: %w", err)
		}
	}

	// --- Step 5: create worktrees ---
	for _, branch := range demoBranches {
		wtDir := demoDir + "-" + branch
		if err := ensureWorktree(demoDir, branch, wtDir); err != nil {
			return err
		}
	}

	// --- Step 6: print URL table ---
	fmt.Println()
	fmt.Println("Demo environment ready. Servers:")
	fmt.Printf("  %-14s  %s\n", "branch", "url")
	fmt.Printf("  %-14s  %s\n", "------", "---")

	mainBranch := gitCurrentBranch(demoDir)
	for _, branch := range append([]string{mainBranch}, demoBranches...) {
		p := port.Assign(branch, demoPortMin, demoPortMax)
		fmt.Printf("  %-14s  http://%s.demo.test:%d\n", branch, branch, p)
	}
	fmt.Println()
	fmt.Printf("  This project:  http://spinner.test:7700/demo\n")
	fmt.Printf("  All projects:  http://spinner.test:7700\n")
	fmt.Println()
	fmt.Println("Ctrl+C to stop.")
	fmt.Println()

	// --- Step 6: run daemon ---
	go func() { _ = dashboard.Serve() }()

	mgr := daemon.New(demoDir, cfg)

	sigs := make(chan os.Signal, 1)
	signal.Notify(sigs, syscall.SIGINT, syscall.SIGTERM)
	go func() {
		<-sigs
		mgr.Stop()
	}()

	return mgr.Run()
}

func ensureGitRepo(dir string) error {
	if err := os.MkdirAll(dir, 0755); err != nil {
		return err
	}
	// Check for .git specifically in this directory, not a parent repo.
	if _, err := os.Stat(filepath.Join(dir, ".git")); err == nil {
		fmt.Printf("Git repo already exists at %s\n", dir)
		return nil
	}
	fmt.Printf("Initialising git repo at %s\n", dir)
	gitDemoRun(dir, "init")
	gitDemoRun(dir, "config", "user.email", "demo@spinner.test")
	gitDemoRun(dir, "config", "user.name", "Spinner Demo")
	// Need at least one commit before worktrees can be added.
	placeholder := filepath.Join(dir, ".gitkeep")
	if err := os.WriteFile(placeholder, nil, 0644); err != nil {
		return err
	}
	gitDemoRun(dir, "add", ".")
	gitDemoRun(dir, "commit", "-m", "init")
	return nil
}

func ensureWorktree(mainRoot, branch, wtDir string) error {
	// Check for .git file (linked worktrees have a .git file, not directory).
	if _, err := os.Stat(filepath.Join(wtDir, ".git")); err == nil {
		fmt.Printf("Worktree %s already exists\n", branch)
		return nil
	}
	fmt.Printf("Creating worktree for %s at %s\n", branch, wtDir)
	cmd := exec.Command("git", "-C", mainRoot, "worktree", "add", "-b", branch, wtDir)
	out, err := cmd.CombinedOutput()
	if err != nil {
		// Branch may already exist from a previous run — try without -b.
		cmd2 := exec.Command("git", "-C", mainRoot, "worktree", "add", wtDir, branch)
		out2, err2 := cmd2.CombinedOutput()
		if err2 != nil {
			return fmt.Errorf("git worktree add %s: %v\n%s\n%s", branch, err, out, out2)
		}
	}
	return nil
}

func gitDemoRun(dir string, args ...string) {
	cmd := exec.Command("git", args...)
	cmd.Dir = dir
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	cmd.Run()
}

func gitCurrentBranch(dir string) string {
	out, err := exec.Command("git", "-C", dir, "rev-parse", "--abbrev-ref", "HEAD").Output()
	if err != nil {
		return "main"
	}
	branch := string(out)
	if len(branch) > 0 && branch[len(branch)-1] == '\n' {
		branch = branch[:len(branch)-1]
	}
	return branch
}

// findRepoRoot walks up from the current directory looking for go.mod with
// the spinner module. Falls back to "." if it can't find it.
func findRepoRoot() (string, error) {
	dir, err := os.Getwd()
	if err != nil {
		return "", err
	}
	for {
		modFile := filepath.Join(dir, "go.mod")
		if data, err := os.ReadFile(modFile); err == nil {
			for _, line := range splitLines(string(data)) {
				if line == "module github.com/bmiller/spinner" {
					return dir, nil
				}
			}
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			break
		}
		dir = parent
	}
	return "", fmt.Errorf("could not find spinner repo root (go.mod not found)")
}

func splitLines(s string) []string {
	var lines []string
	start := 0
	for i, c := range s {
		if c == '\n' {
			lines = append(lines, s[start:i])
			start = i + 1
		}
	}
	if start < len(s) {
		lines = append(lines, s[start:])
	}
	return lines
}
