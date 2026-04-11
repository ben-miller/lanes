package main

import (
	"fmt"
	"os"

	"github.com/bmiller/spinner/cmd"
	"github.com/bmiller/spinner/internal/build"
	"github.com/bmiller/spinner/internal/config"
)

func main() {
	// In dev mode, auto-update if configured and source has newer commits.
	if build.IsDev() && build.IsStale() {
		cfg, err := config.LoadUserConfig()
		if err == nil && cfg.Update.Auto {
			fmt.Fprintln(os.Stderr, "spinner: source has newer commits — auto-updating...")
			if err := build.BuildAndReexec(); err != nil {
				fmt.Fprintf(os.Stderr, "spinner: auto-update failed: %v\n", err)
				// Fall through and continue with current binary.
			}
		}
	}

	if err := cmd.Execute(); err != nil {
		os.Exit(1)
	}
}
