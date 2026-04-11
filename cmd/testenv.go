package cmd

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"

	"github.com/bmiller/spinner/internal/config"
	"github.com/bmiller/spinner/internal/port"
	"github.com/spf13/cobra"
)

func newTestenvCmd() *cobra.Command {
	return &cobra.Command{
		Use:   "create-demo-app",
		Short: "Set up a local test environment at ~/spinner-demo",
		Long: `Creates ~/spinner-demo as a git repo with a few worktrees and a spinner.toml
pointing at testapp. Safe to run multiple times.

After running this, cd into ~/spinner-demo and use normal spinner commands:
  spinner up
  spinner status
  spinner logs branch-a
  spinner open branch-a`,
		RunE: runTestenv,
	}
}

func runTestenv(cmd *cobra.Command, args []string) error {
	repoRoot, err := findRepoRoot()
	if err != nil {
		return fmt.Errorf("run this from the spinner repo root: %w", err)
	}

	demosDir := filepath.Join(repoRoot, "demos")
	demoDir := filepath.Join(demosDir, "main")
	testappBin := filepath.Join(demoDir, "testapp")

	// Build testapp
	fmt.Println("Building testapp...")
	build := exec.Command("go", "build", "-o", testappBin, "./testapp")
	build.Dir = repoRoot
	build.Stdout = os.Stdout
	build.Stderr = os.Stderr
	if err := build.Run(); err != nil {
		return fmt.Errorf("building testapp: %w", err)
	}

	// Set up git repo
	if err := ensureGitRepo(demoDir); err != nil {
		return err
	}

	// Write spinner.toml
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

	// Create worktrees as siblings of main/ inside demos/
	for _, branch := range demoBranches {
		wtDir := filepath.Join(demosDir, branch)
		if err := ensureWorktree(demoDir, branch, wtDir); err != nil {
			return err
		}
	}

	// Print summary
	fmt.Println()
	fmt.Printf("Test environment ready at %s\n", demosDir)
	fmt.Println()
	fmt.Printf("  %-14s  %s\n", "branch", "port")
	fmt.Printf("  %-14s  %s\n", "------", "----")
	mainBranch := gitCurrentBranch(demoDir)
	for _, branch := range append([]string{mainBranch}, demoBranches...) {
		p := port.Assign(branch, demoPortMin, demoPortMax)
		fmt.Printf("  %-14s  %d\n", branch, p)
	}
	fmt.Println()
	fmt.Printf("  cd %s\n", demosDir+"/main")
	fmt.Printf("  spinner up\n")
	fmt.Println()
	fmt.Println("For .test DNS resolution, run once (requires sudo):")
	fmt.Println("  sudo spinner init  (from ~/spinner-demo)")

	return nil
}
