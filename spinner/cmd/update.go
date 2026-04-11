package cmd

import (
	"fmt"

	"github.com/bmiller/spinner/internal/build"
	"github.com/spf13/cobra"
)

func newUpdateCmd() *cobra.Command {
	return &cobra.Command{
		Use:   "update",
		Short: "Rebuild spinner from source and restart",
		RunE:  runUpdate,
	}
}

func runUpdate(cmd *cobra.Command, args []string) error {
	if !build.IsDev() {
		return fmt.Errorf("update from source is only available in development mode")
	}
	if build.SourceDir == "" {
		return fmt.Errorf("source directory not set — install with `make install` to enable updates")
	}
	if !build.IsStale() {
		fmt.Println("Already up to date.")
		return nil
	}
	return build.BuildAndReexec()
}
