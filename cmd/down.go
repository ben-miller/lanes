package cmd

import (
	"fmt"
	"os"
	"syscall"

	"github.com/bmiller/spinner/internal/config"
	"github.com/bmiller/spinner/internal/git"
	"github.com/bmiller/spinner/internal/state"
	"github.com/spf13/cobra"
)

func newDownCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "down",
		Short: "Stop dev servers",
		RunE:  runDown,
	}
	cmd.Flags().Bool("all", false, "Stop all registered projects")
	return cmd
}

func runDown(cmd *cobra.Command, args []string) error {
	all, _ := cmd.Flags().GetBool("all")

	if all {
		return downAll()
	}

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
	return stopProject(cfg.Project.Name)
}

func downAll() error {
	global, err := config.LoadGlobal()
	if err != nil {
		return err
	}
	for _, repo := range global.Repos {
		if err := stopProject(repo.Name); err != nil {
			fmt.Fprintf(os.Stderr, "error stopping %s: %v\n", repo.Name, err)
		} else {
			fmt.Printf("Stopped %s\n", repo.Name)
		}
	}
	return nil
}

func stopProject(projectName string) error {
	pid, err := state.ReadPID(projectName)
	if err != nil {
		return err
	}
	if pid == 0 {
		fmt.Printf("%s: not running\n", projectName)
		return nil
	}

	proc, err := os.FindProcess(pid)
	if err != nil {
		return err
	}
	if err := proc.Signal(syscall.SIGTERM); err != nil {
		return fmt.Errorf("sending SIGTERM to pid %d: %w", pid, err)
	}
	fmt.Printf("Stopped %s (pid %d)\n", projectName, pid)
	return nil
}
