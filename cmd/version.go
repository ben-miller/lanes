package cmd

import (
	"fmt"

	"github.com/bmiller/spinner/internal/build"
	"github.com/spf13/cobra"
)

func newVersionCmd() *cobra.Command {
	return &cobra.Command{
		Use:   "version",
		Short: "Show version information",
		RunE:  runVersion,
	}
}

func runVersion(cmd *cobra.Command, args []string) error {
	fmt.Println(build.Info())

	if !build.IsDev() || !build.IsKnown() {
		return nil
	}

	head := build.HeadSHA()
	if head == "" {
		fmt.Println("  source: could not read HEAD")
		return nil
	}

	short := head
	if len(short) > 8 {
		short = short[:8]
	}

	if head == build.Version {
		fmt.Printf("  up to date  (source: %s)\n", short)
	} else {
		fmt.Printf("  stale — source is at %s\n", short)
		fmt.Println("  run: spinner update")
	}
	return nil
}
