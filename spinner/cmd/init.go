package cmd

import (
	"bufio"
	"fmt"
	"os"
	"strings"

	"github.com/bmiller/spinner/internal/config"
	"github.com/bmiller/spinner/internal/dnsmasq"
	"github.com/bmiller/spinner/internal/git"
	"github.com/spf13/cobra"
)

func newInitCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "init",
		Short: "Initialize spinner for the current project",
		Long: `Creates spinner.toml and configures dnsmasq for local .test domain resolution.
Run this once per project. Pass --register to also add it to the global dashboard.`,
		RunE: runInit,
	}
	cmd.Flags().Bool("register", false, "Also register in the global registry")
	return cmd
}

func runInit(cmd *cobra.Command, args []string) error {
	cwd, err := os.Getwd()
	if err != nil {
		return err
	}

	mainRoot, err := git.MainWorktreeRoot(cwd)
	if err != nil {
		return fmt.Errorf("not inside a git repository: %w", err)
	}

	// Check if already initialized.
	if _, err := os.Stat(mainRoot + "/spinner.toml"); err == nil {
		return fmt.Errorf("spinner.toml already exists in %s", mainRoot)
	}

	fmt.Println("Initializing spinner for", mainRoot)

	r := bufio.NewReader(os.Stdin)

	// Project name
	defaultName := lastPathComponent(mainRoot)
	name := prompt(r, fmt.Sprintf("Project name [%s]: ", defaultName))
	if name == "" {
		name = defaultName
	}

	// Domain suffix
	defaultDomain := name + ".test"
	domain := prompt(r, fmt.Sprintf("Domain suffix [%s]: ", defaultDomain))
	if domain == "" {
		domain = defaultDomain
	}

	// Port range
	portMin := promptInt(r, "Port range min [4100]: ", 4100)
	portMax := promptInt(r, "Port range max [4199]: ", 4199)

	// Dev server command
	serverCmd := prompt(r, "Dev server command: ")
	if serverCmd == "" {
		return fmt.Errorf("dev server command is required")
	}

	cfg := &config.ProjectConfig{
		Project: config.ProjectSection{
			Name:         name,
			DomainSuffix: domain,
			PortRange:    config.PortRange{Min: portMin, Max: portMax},
		},
		Server: config.ServerSection{
			Command: serverCmd,
			Env:     map[string]string{},
		},
	}

	if err := config.SaveProject(mainRoot, cfg); err != nil {
		return fmt.Errorf("writing spinner.toml: %w", err)
	}
	fmt.Printf("Wrote %s/spinner.toml\n", mainRoot)

	fmt.Printf("\nConfiguring dnsmasq for .%s (requires sudo)...\n", domain)
	firstProject := !dnsmasq.IsDashboardConfigured()
	if firstProject {
		fmt.Println("First project — also configuring spinner.test dashboard domain.")
		if err := dnsmasq.EnsureDashboard(); err != nil {
			return fmt.Errorf("configuring dashboard DNS: %w", err)
		}
	}
	if err := dnsmasq.EnsureProject(domain); err != nil {
		return fmt.Errorf("configuring DNS: %w", err)
	}

	register, _ := cmd.Flags().GetBool("register")
	if register {
		if err := runRegister(cmd, args); err != nil {
			return err
		}
	} else {
		fmt.Printf("\nDone! Run `spinner up` to start.\n")
		fmt.Printf("To add this project to the global dashboard: spinner init --register\n")
	}
	return nil
}

func prompt(r *bufio.Reader, msg string) string {
	fmt.Print(msg)
	line, _ := r.ReadString('\n')
	return strings.TrimSpace(line)
}

func promptInt(r *bufio.Reader, msg string, def int) int {
	val := prompt(r, msg)
	if val == "" {
		return def
	}
	var n int
	if _, err := fmt.Sscanf(val, "%d", &n); err != nil {
		return def
	}
	return n
}

func lastPathComponent(path string) string {
	path = strings.TrimRight(path, "/")
	parts := strings.Split(path, "/")
	if len(parts) == 0 {
		return ""
	}
	return parts[len(parts)-1]
}
