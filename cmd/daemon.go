package cmd

import (
	"github.com/bmiller/spinner/internal/config"
	"github.com/bmiller/spinner/internal/daemon"
	"github.com/bmiller/spinner/internal/dashboard"
	"github.com/spf13/cobra"
)

// newDaemonCmd returns the internal _daemon command. It is hidden from help
// and is invoked by `spinner up -d` to run as a background process.
func newDaemonCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:    "_daemon",
		Short:  "Internal: run as daemon process",
		Hidden: true,
		RunE:   runDaemon,
	}
	cmd.Flags().String("project-root", "", "project root directory")
	cmd.MarkFlagRequired("project-root")
	return cmd
}

func runDaemon(cmd *cobra.Command, args []string) error {
	root, _ := cmd.Flags().GetString("project-root")

	cfg, err := config.LoadProject(root)
	if err != nil {
		return err
	}

	// Start dashboard in background (only this daemon serves it; multiple daemons
	// will race but it's harmless — only one will bind successfully).
	go func() { _ = dashboard.Serve() }()

	mgr := daemon.New(root, cfg)
	return mgr.Run()
}
