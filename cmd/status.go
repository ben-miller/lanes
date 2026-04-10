package cmd

import (
	"fmt"
	"os"
	"strings"
	"syscall"

	"github.com/bmiller/spinner/internal/config"
	"github.com/bmiller/spinner/internal/git"
	"github.com/bmiller/spinner/internal/state"
	"github.com/charmbracelet/lipgloss"
	"github.com/spf13/cobra"
)

var (
	styleHeader = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("86"))
	styleURL    = lipgloss.NewStyle().Foreground(lipgloss.Color("75"))
	styleBranch = lipgloss.NewStyle().Foreground(lipgloss.Color("252"))
	stylePort   = lipgloss.NewStyle().Foreground(lipgloss.Color("240"))

	styleRunning = lipgloss.NewStyle().Foreground(lipgloss.Color("82")).Bold(true)
	styleStopped = lipgloss.NewStyle().Foreground(lipgloss.Color("196"))
	styleError   = lipgloss.NewStyle().Foreground(lipgloss.Color("214"))

	styleProject = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("141"))
	styleDim     = lipgloss.NewStyle().Foreground(lipgloss.Color("240"))
	styleSep     = lipgloss.NewStyle().Foreground(lipgloss.Color("237"))
)

func newStatusCmd() *cobra.Command {
	return &cobra.Command{
		Use:   "status",
		Short: "Show status of all registered projects",
		RunE:  runStatus,
	}
}

func runStatus(cmd *cobra.Command, args []string) error {
	global, err := config.LoadGlobal()
	if err != nil {
		return err
	}
	if len(global.Repos) == 0 {
		hint := "spinner init"
		if cwd, err := os.Getwd(); err == nil {
			if mainRoot, err := git.MainWorktreeRoot(cwd); err == nil {
				if _, err := os.Stat(mainRoot + "/spinner.toml"); err == nil {
					hint = "spinner register"
				}
			}
		}
		fmt.Println(styleDim.Render("No projects registered. Run `" + hint + "` to add one."))
		return nil
	}

	fmt.Println()
	for _, repo := range global.Repos {
		s, err := state.Load(repo.Name)
		if err != nil {
			fmt.Fprintf(os.Stderr, "error reading state for %s: %v\n", repo.Name, err)
			continue
		}

		daemonStatus := styleStopped.Render("stopped")
		if s.DaemonPID > 0 {
			if pidAlive(s.DaemonPID) {
				daemonStatus = styleRunning.Render("running")
			}
		}

		fmt.Printf("  %s  %s\n", styleProject.Render(repo.Name), daemonStatus)
		fmt.Printf("  %s\n", styleDim.Render(repo.Path))

		if len(s.Worktrees) == 0 {
			fmt.Printf("  %s\n", styleDim.Render("no worktrees"))
		} else {
			// Calculate column widths.
			maxBranch, maxURL, maxPort := 6, 3, 4
			for _, wt := range s.Worktrees {
				if len(wt.Branch) > maxBranch {
					maxBranch = len(wt.Branch)
				}
				if len(wt.URL) > maxURL {
					maxURL = len(wt.URL)
				}
				portStr := fmt.Sprintf("%d", wt.Port)
				if len(portStr) > maxPort {
					maxPort = len(portStr)
				}
			}

			// Header
			fmt.Printf("  %s  %s  %s  %s\n",
				styleHeader.Render(pad("branch", maxBranch)),
				styleHeader.Render(pad("url", maxURL)),
				styleHeader.Render(pad("port", maxPort)),
				styleHeader.Render("status"),
			)
			fmt.Printf("  %s\n", styleSep.Render(strings.Repeat("─", maxBranch+maxURL+maxPort+20)))

			for _, wt := range s.Worktrees {
				statusStr := renderStatus(wt.Status)
				fmt.Printf("  %s  %s  %s  %s\n",
					styleBranch.Render(pad(wt.Branch, maxBranch)),
					styleURL.Render(pad(wt.URL, maxURL)),
					stylePort.Render(pad(fmt.Sprintf("%d", wt.Port), maxPort)),
					statusStr,
				)
			}
		}
		fmt.Println()
	}
	return nil
}

func renderStatus(s state.Status) string {
	switch s {
	case state.StatusRunning:
		return styleRunning.Render(string(s))
	case state.StatusStopped:
		return styleStopped.Render(string(s))
	case state.StatusError:
		return styleError.Render(string(s))
	default:
		return styleDim.Render(string(s))
	}
}

func pad(s string, width int) string {
	if len(s) >= width {
		return s
	}
	return s + strings.Repeat(" ", width-len(s))
}

func pidAlive(pid int) bool {
	return syscall.Kill(pid, 0) == nil
}
