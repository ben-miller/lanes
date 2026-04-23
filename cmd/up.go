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
	"github.com/bmiller/spinner/internal/daemon"
	"github.com/bmiller/spinner/internal/dashboard"
	"github.com/bmiller/spinner/internal/dnsmasq"
	"github.com/bmiller/spinner/internal/git"
	"github.com/bmiller/spinner/internal/port"
	"github.com/bmiller/spinner/internal/state"
	"github.com/spf13/cobra"
)

func newUpCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "up",
		Short: "Start dev servers for all worktrees",
		RunE:  runUp,
	}
	cmd.Flags().BoolP("detach", "d", false, "Run in background")
	cmd.Flags().Bool("all", false, "Start all registered projects (requires --detach)")
	return cmd
}

func runUp(cmd *cobra.Command, args []string) error {
	detach, _ := cmd.Flags().GetBool("detach")
	all, _ := cmd.Flags().GetBool("all")

	if all && !detach {
		return fmt.Errorf("--all requires --detach (-d)\n\nRun: spinner up --all -d")
	}

	if all {
		return upAll()
	}

	cwd, err := os.Getwd()
	if err != nil {
		return err
	}
	mainRoot, err := git.MainWorktreeRoot(cwd)
	if err != nil {
		return fmt.Errorf("not inside a git repository: %w", err)
	}

	if _, err := os.Stat(filepath.Join(mainRoot, "spinner.toml")); os.IsNotExist(err) {
		return fmt.Errorf("no spinner.toml found\n\n  To initialize this project: spinner init")
	}

	if !isRegistered(mainRoot) {
		fmt.Fprintf(os.Stderr, "warning: project not in global registry — run `spinner register` to add it to the dashboard\n")
	}

	cfg, err := config.LoadProject(mainRoot)
	if err != nil {
		return err
	}

	if state.IsRunning(cfg.Project.Name) {
		return fmt.Errorf("spinner is already running for %q (use `spinner down` to stop it first)", cfg.Project.Name)
	}

	if detach {
		if !isRegistered(mainRoot) {
			return fmt.Errorf("project must be registered before running in the background\n\n  Run: spinner register")
		}
		return startDetached(mainRoot)
	}
	return runForeground(mainRoot, cfg)
}

func upAll() error {
	global, err := config.LoadGlobal()
	if err != nil {
		return err
	}
	if len(global.Repos) == 0 {
		return fmt.Errorf("no projects registered; run `spinner init` first")
	}
	var errs []error
	for _, repo := range global.Repos {
		if err := startDetached(repo.Path); err != nil {
			errs = append(errs, fmt.Errorf("%s: %w", repo.Name, err))
		} else {
			fmt.Printf("Started %s\n", repo.Name)
		}
	}
	if len(errs) > 0 {
		for _, e := range errs {
			fmt.Fprintf(os.Stderr, "error: %v\n", e)
		}
		return fmt.Errorf("%d project(s) failed to start", len(errs))
	}
	return nil
}

func startDetached(projectRoot string) error {
	self, err := os.Executable()
	if err != nil {
		return err
	}

	logFile := os.DevNull
	devnull, _ := os.OpenFile(os.DevNull, os.O_RDWR, 0)

	cmd := exec.Command(self, "_daemon", "--project-root", projectRoot)
	cmd.Stdin = devnull
	cmd.Stdout = openLogOrDevNull(logFile)
	cmd.Stderr = cmd.Stdout
	cmd.SysProcAttr = &syscall.SysProcAttr{Setsid: true}

	if err := cmd.Start(); err != nil {
		return fmt.Errorf("starting daemon: %w", err)
	}
	fmt.Printf("Daemon started (pid %d) for %s\n", cmd.Process.Pid, projectRoot)
	return nil
}

func runForeground(projectRoot string, cfg *config.ProjectConfig) error {
	fmt.Printf("Starting spinner for %s (foreground — Ctrl+C to stop)\n", cfg.Project.Name)

	go func() { _ = dashboard.Serve() }()

	// Print URL table before blocking — ports are deterministic so we can
	// compute them without waiting for servers to start.
	if worktrees, err := git.ListWorktrees(projectRoot); err == nil {
		printURLTable(cfg, worktrees)
	}

	mgr := daemon.New(projectRoot, cfg)

	// Forward Ctrl+C to a clean shutdown.
	sigs := make(chan os.Signal, 1)
	signal.Notify(sigs, syscall.SIGINT, syscall.SIGTERM)
	go func() {
		<-sigs
		mgr.Stop()
	}()

	return mgr.Run()
}

func printURLTable(cfg *config.ProjectConfig, worktrees []git.Worktree) {
	dnsOK := dnsmasq.IsProjectConfigured(cfg.Project.DomainSuffix)

	fmt.Println()
	fmt.Printf("  %-16s  %s\n", "BRANCH", "URL")
	fmt.Printf("  %-16s  %s\n", "------", "---")
	for _, wt := range worktrees {
		if wt.Branch == "" {
			continue
		}
		p := port.Assign(wt.Branch, cfg.Project.PortRange.Min, cfg.Project.PortRange.Max)
		var url string
		if dnsOK {
			url = fmt.Sprintf("http://%s.%s:%d", strings.ReplaceAll(wt.Branch, "/", "-"), cfg.Project.DomainSuffix, p)
		} else {
			url = fmt.Sprintf("http://localhost:%d  (.test DNS not configured)", p)
		}
		fmt.Printf("  %-16s  %s\n", wt.Branch, url)
	}
	fmt.Println()
	fmt.Printf("  Dashboard:        http://spinner.test:%d/%s\n", dashboard.Port, cfg.Project.Name)
	fmt.Printf("  All projects:     http://spinner.test:%d\n", dashboard.Port)
	if !dnsOK {
		fmt.Printf("\n  To enable .test URLs, run once: sudo spinner init\n")
	}
	fmt.Println()
}

func isRegistered(mainRoot string) bool {
	global, err := config.LoadGlobal()
	if err != nil {
		return false
	}
	for _, r := range global.Repos {
		if r.Path == mainRoot {
			return true
		}
	}
	return false
}

func openLogOrDevNull(path string) io.Writer {
	if path == os.DevNull {
		f, _ := os.OpenFile(os.DevNull, os.O_WRONLY, 0)
		return f
	}
	f, err := os.OpenFile(path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0644)
	if err != nil {
		return io.Discard
	}
	return f
}
