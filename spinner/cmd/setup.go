package cmd

import (
	"fmt"
	"io"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"strings"
	"syscall"

	"github.com/bmiller/spinner/internal/config"
	"github.com/bmiller/spinner/internal/git"
	"github.com/bmiller/spinner/internal/state"
	"github.com/creack/pty"
	"github.com/spf13/cobra"
)

func newSetupCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "setup [branch]",
		Short: "Run the setup command for a worktree (default: current branch)",
		Args:  cobra.MaximumNArgs(1),
		RunE:  runSetup,
	}
	cmd.Flags().Bool("all", false, "Run setup for all worktrees with pending or failed status")
	return cmd
}

func runSetup(cmd *cobra.Command, args []string) error {
	all, _ := cmd.Flags().GetBool("all")

	cwd, err := os.Getwd()
	if err != nil {
		return err
	}
	mainRoot, err := git.MainWorktreeRoot(cwd)
	if err != nil {
		return fmt.Errorf("not inside a git repository: %w", err)
	}
	cfg, err := config.LoadProject(mainRoot)
	if err != nil {
		return err
	}
	if cfg.Server.Setup == "" {
		return fmt.Errorf("no setup command configured in spinner.toml (add [server] setup = \"...\")")
	}

	if all {
		return setupAll(mainRoot, cfg)
	}

	var branch string
	if len(args) > 0 {
		branch = args[0]
	} else {
		branch, err = git.CurrentBranch(cwd)
		if err != nil {
			return err
		}
	}

	worktreeDir, err := findWorktreeDir(mainRoot, branch)
	if err != nil {
		return err
	}

	return runSetupForBranch(cfg, branch, worktreeDir)
}

func setupAll(mainRoot string, cfg *config.ProjectConfig) error {
	worktrees, err := git.ListWorktrees(mainRoot)
	if err != nil {
		return fmt.Errorf("listing worktrees: %w", err)
	}

	var targets []git.Worktree
	for _, wt := range worktrees {
		if wt.Branch == "" {
			continue
		}
		st := state.GetWorktreeSetupStatus(cfg.Project.Name, wt.Branch)
		if st != state.SetupStatusOK {
			targets = append(targets, wt)
		}
	}

	if len(targets) == 0 {
		fmt.Println("All worktrees are already set up.")
		return nil
	}

	var firstErr error
	for _, wt := range targets {
		fmt.Printf("\n=== Setting up %s ===\n", wt.Branch)
		if err := runSetupForBranch(cfg, wt.Branch, wt.Path); err != nil {
			fmt.Fprintf(os.Stderr, "error: setup failed for %s: %v\n", wt.Branch, err)
			if firstErr == nil {
				firstErr = err
			}
		}
	}
	return firstErr
}

func findWorktreeDir(mainRoot, branch string) (string, error) {
	worktrees, err := git.ListWorktrees(mainRoot)
	if err != nil {
		return "", fmt.Errorf("listing worktrees: %w", err)
	}
	for _, wt := range worktrees {
		if wt.Branch == branch {
			return wt.Path, nil
		}
	}
	return "", fmt.Errorf("no worktree found for branch %q", branch)
}

func runSetupForBranch(cfg *config.ProjectConfig, branch, worktreeDir string) error {
	projectName := cfg.Project.Name
	logFile := state.SetupLogFile(projectName, branch)

	if err := os.MkdirAll(filepath.Dir(logFile), 0755); err != nil {
		return fmt.Errorf("creating log dir: %w", err)
	}
	lf, err := os.OpenFile(logFile, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0644)
	if err != nil {
		return fmt.Errorf("opening setup log: %w", err)
	}
	defer lf.Close()

	if err := state.SetWorktreeSetupStatus(projectName, branch, state.SetupStatusRunning); err != nil {
		return fmt.Errorf("updating setup status: %w", err)
	}

	fmt.Fprintf(os.Stderr, "Running setup for %s/%s (Ctrl+C to abort)\n", projectName, branch)
	fmt.Fprintf(os.Stderr, "Log: %s\n\n", logFile)

	parts := strings.Fields(cfg.Server.Setup)
	if len(parts) == 0 {
		return fmt.Errorf("empty setup command")
	}

	c := exec.Command(parts[0], parts[1:]...)
	c.Dir = worktreeDir

	runErr := runWithPTY(c, lf)

	newStatus := state.SetupStatusOK
	if runErr != nil {
		newStatus = state.SetupStatusFailed
	}
	if err := state.SetWorktreeSetupStatus(projectName, branch, newStatus); err != nil {
		fmt.Fprintf(os.Stderr, "warning: could not save setup status: %v\n", err)
	}

	if runErr != nil {
		return fmt.Errorf("setup failed for %s: %w", branch, runErr)
	}

	fmt.Fprintf(os.Stderr, "\nSetup complete for %s.\n", branch)
	return nil
}

// runWithPTY runs cmd under a PTY, copying output to both stdout and lf.
// Falls back to plain pipes if a PTY cannot be allocated.
func runWithPTY(c *exec.Cmd, lf io.Writer) error {
	ptmx, err := pty.Start(c)
	if err != nil {
		// PTY unavailable (e.g. non-interactive environment); fall back to pipes.
		c.Stdout = io.MultiWriter(os.Stdout, lf)
		c.Stderr = io.MultiWriter(os.Stderr, lf)
		c.Stdin = os.Stdin
		return c.Run()
	}

	// Set initial terminal size.
	_ = pty.InheritSize(os.Stdin, ptmx)

	// Forward terminal resize signals to the PTY.
	resizeCh := make(chan os.Signal, 1)
	signal.Notify(resizeCh, syscall.SIGWINCH)
	go func() {
		for range resizeCh {
			_ = pty.InheritSize(os.Stdin, ptmx)
		}
	}()

	// Stream PTY output to terminal and log file simultaneously.
	copyDone := make(chan struct{})
	go func() {
		defer close(copyDone)
		io.Copy(io.MultiWriter(os.Stdout, lf), ptmx) //nolint:errcheck
	}()

	runErr := c.Wait()
	signal.Stop(resizeCh)
	close(resizeCh)
	ptmx.Close()
	<-copyDone

	return runErr
}
