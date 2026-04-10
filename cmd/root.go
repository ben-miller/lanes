package cmd

import (
	"github.com/spf13/cobra"
)

var rootCmd = &cobra.Command{
	Use:   "spinner",
	Short: "Manage per-worktree dev server instances",
}

func Execute() error {
	rootCmd.SetUsageTemplate("\n" + rootCmd.UsageTemplate())
	return rootCmd.Execute()
}

func init() {
	rootCmd.AddCommand(
		newInitCmd(),
		newRegisterCmd(),
		newUpCmd(),
		newDownCmd(),
		newStatusCmd(),
		newLogsCmd(),
		newOpenCmd(),
		newDaemonCmd(),
		newTestenvCmd(),
		newDemoCmd(),
	)
}
