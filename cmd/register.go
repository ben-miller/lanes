package cmd

import (
	"fmt"
	"os"

	"github.com/bmiller/spinner/internal/config"
	"github.com/bmiller/spinner/internal/git"
	"github.com/spf13/cobra"
)

func newRegisterCmd() *cobra.Command {
	return &cobra.Command{
		Use:   "register",
		Short: "Register the current project in the global registry",
		Long: `Adds the current project to ~/.config/spinner/registry.toml so it
appears in "spinner status", "spinner up --all", and the dashboard at
http://spinner.test:7700.

Requires spinner.toml to exist (run "spinner init" first).`,
		RunE: runRegister,
	}
}

func runRegister(cmd *cobra.Command, args []string) error {
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
		return fmt.Errorf("no spinner.toml found — run `spinner init` first: %w", err)
	}

	global, err := config.LoadGlobal()
	if err != nil {
		return err
	}

	for _, r := range global.Repos {
		if r.Path == mainRoot {
			fmt.Printf("%s is already registered.\n", cfg.Project.Name)
			return nil
		}
	}

	global.Repos = append(global.Repos, config.RepoEntry{Path: mainRoot, Name: cfg.Project.Name})
	if err := config.SaveGlobal(global); err != nil {
		return fmt.Errorf("saving registry: %w", err)
	}

	fmt.Printf("Registered %s in %s\n", cfg.Project.Name, config.GlobalConfigPath())
	fmt.Printf("Dashboard: http://spinner.test:7700/%s\n", cfg.Project.Name)
	return nil
}
