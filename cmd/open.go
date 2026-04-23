package cmd

import (
	"fmt"
	"os"
	"os/exec"
	"strings"

	"github.com/bmiller/spinner/internal/config"
	"github.com/bmiller/spinner/internal/git"
	"github.com/bmiller/spinner/internal/port"
	"github.com/spf13/cobra"
)

func newOpenCmd() *cobra.Command {
	return &cobra.Command{
		Use:   "open [branch]",
		Short: "Open worktree URL in browser (default: current branch)",
		Args:  cobra.MaximumNArgs(1),
		RunE:  runOpen,
	}
}

func runOpen(cmd *cobra.Command, args []string) error {
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

	var branch string
	if len(args) > 0 {
		branch = args[0]
	} else {
		branch, err = git.CurrentBranch(cwd)
		if err != nil {
			return err
		}
	}

	p := port.Assign(branch, cfg.Project.PortRange.Min, cfg.Project.PortRange.Max)
	url := fmt.Sprintf("http://%s.%s:%d", strings.ReplaceAll(branch, "/", "-"), cfg.Project.DomainSuffix, p)

	fmt.Printf("Opening %s\n", url)
	return exec.Command("open", url).Run()
}
