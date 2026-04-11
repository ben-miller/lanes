package cmd

import (
	"fmt"
	"os"

	"github.com/bmiller/spinner/internal/config"
	"github.com/bmiller/spinner/internal/git"
	"github.com/bmiller/spinner/internal/state"
	"github.com/spf13/cobra"
)

func newCleanCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "clean [branch]",
		Short: "Remove spinner log files for a worktree (default: current branch)",
		Args:  cobra.MaximumNArgs(1),
		RunE:  runClean,
	}
	cmd.Flags().Bool("all", false, "Remove log files for all branches")
	return cmd
}

func runClean(cmd *cobra.Command, args []string) error {
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
	projectName := cfg.Project.Name

	if all {
		return cleanAll(mainRoot, projectName)
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

	return cleanBranch(projectName, branch)
}

func cleanAll(mainRoot, projectName string) error {
	// Collect all branches that have any artifacts: current worktrees + state entries.
	seen := map[string]bool{}

	if gits, err := git.ListWorktrees(mainRoot); err == nil {
		for _, wt := range gits {
			if wt.Branch != "" {
				seen[wt.Branch] = true
			}
		}
	}
	if s, err := state.Load(projectName); err == nil {
		for _, wt := range s.Worktrees {
			seen[wt.Branch] = true
		}
	}

	var firstErr error
	for branch := range seen {
		if err := cleanBranch(projectName, branch); err != nil && firstErr == nil {
			firstErr = err
		}
	}
	return firstErr
}

func cleanBranch(projectName, branch string) error {
	removed := 0

	if ok, err := removeIfExists(state.LogFile(projectName, branch)); err != nil {
		return err
	} else if ok {
		removed++
	}

	if ok, err := removeIfExists(state.SetupLogFile(projectName, branch)); err != nil {
		return err
	} else if ok {
		removed++
	}

	if removed > 0 {
		fmt.Printf("Cleaned logs for %s/%s\n", projectName, branch)
	} else {
		fmt.Printf("Nothing to clean for %s/%s\n", projectName, branch)
	}
	return nil
}

// removeIfExists removes path if it exists.
// Returns (true, nil) if removed, (false, nil) if not found, or (false, err) on failure.
func removeIfExists(path string) (bool, error) {
	err := os.Remove(path)
	if err == nil {
		return true, nil
	}
	if os.IsNotExist(err) {
		return false, nil
	}
	return false, fmt.Errorf("removing %s: %w", path, err)
}
