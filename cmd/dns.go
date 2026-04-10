package cmd

import (
	"fmt"
	"os"

	"github.com/bmiller/spinner/internal/config"
	"github.com/bmiller/spinner/internal/dnsmasq"
	"github.com/bmiller/spinner/internal/git"
	"github.com/spf13/cobra"
)

func newDNSCmd() *cobra.Command {
	return &cobra.Command{
		Use:   "dns",
		Short: "Configure dnsmasq for the current project's .test domain",
		Long: `Adds a wildcard dnsmasq entry and /etc/resolver file for the project's
domain suffix, then restarts dnsmasq. Run once per project after spinner init.
Will prompt for your sudo password only for the parts that need it.`,
		RunE: runDNS,
	}
}

func runDNS(cmd *cobra.Command, args []string) error {
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

	domain := cfg.Project.DomainSuffix

	if dnsmasq.IsProjectConfigured(domain) {
		fmt.Printf("DNS already configured for .%s — nothing to do.\n", domain)
		return nil
	}

	if !dnsmasq.IsDashboardConfigured() {
		fmt.Println("First project — also configuring spinner.test dashboard domain.")
		if err := dnsmasq.EnsureDashboard(); err != nil {
			return fmt.Errorf("configuring dashboard DNS: %w", err)
		}
	}

	fmt.Printf("Configuring dnsmasq for .%s (will prompt for sudo)...\n", domain)
	if err := dnsmasq.EnsureProject(domain); err != nil {
		return fmt.Errorf("configuring DNS: %w", err)
	}

	fmt.Printf("\nDone! DNS configured for *.%s\n", domain)
	fmt.Printf("Run `spinner up` to start.\n")
	return nil
}
