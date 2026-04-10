package dnsmasq

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

const (
	dnsmasqConf     = "/opt/homebrew/etc/dnsmasq.conf"
	resolverDir     = "/etc/resolver"
	dashboardDomain = "spinner.test"
)

// EnsureProject adds dnsmasq wildcard resolution for domainSuffix and reloads
// dnsmasq. Prompts for sudo password inline when writing /etc/resolver/.
func EnsureProject(domainSuffix string) error {
	if err := addDnsmasqEntry(domainSuffix); err != nil {
		return fmt.Errorf("dnsmasq.conf: %w", err)
	}
	if err := writeResolver(domainSuffix); err != nil {
		return fmt.Errorf("resolver: %w", err)
	}
	return reload()
}

// EnsureDashboard sets up spinner.test for the global dashboard. Safe to call
// multiple times — idempotent.
func EnsureDashboard() error {
	if err := addDnsmasqEntry(dashboardDomain); err != nil {
		return fmt.Errorf("dnsmasq.conf (dashboard): %w", err)
	}
	if err := writeResolver(dashboardDomain); err != nil {
		return fmt.Errorf("resolver (dashboard): %w", err)
	}
	return reload()
}

func addDnsmasqEntry(domain string) error {
	entry := fmt.Sprintf("address=/.%s/127.0.0.1", domain)

	data, err := os.ReadFile(dnsmasqConf)
	if err != nil {
		return fmt.Errorf("reading %s: %w", dnsmasqConf, err)
	}

	for _, line := range strings.Split(string(data), "\n") {
		if strings.TrimSpace(line) == entry {
			return nil // already present
		}
	}

	f, err := os.OpenFile(dnsmasqConf, os.O_APPEND|os.O_WRONLY, 0644)
	if err != nil {
		return fmt.Errorf("opening %s: %w", dnsmasqConf, err)
	}
	defer f.Close()
	_, err = fmt.Fprintf(f, "\n%s\n", entry)
	return err
}

func writeResolver(domain string) error {
	path := filepath.Join(resolverDir, domain)
	if _, err := os.Stat(path); err == nil {
		return nil // already exists
	}
	// /etc/resolver/ requires root — pipe content via sudo tee so the user
	// gets a normal password prompt without re-running the whole command as root.
	cmd := exec.Command("sudo", "tee", path)
	cmd.Stdin = strings.NewReader("nameserver 127.0.0.1\n")
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	return cmd.Run()
}

func reload() error {
	cmd := exec.Command("sudo", "brew", "services", "restart", "dnsmasq")
	cmd.Stdin = os.Stdin
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	return cmd.Run()
}

// IsProjectConfigured returns true if the given domain suffix is fully
// configured (both dnsmasq.conf entry and /etc/resolver file).
func IsProjectConfigured(domainSuffix string) bool {
	data, err := os.ReadFile(dnsmasqConf)
	if err != nil {
		return false
	}
	entry := fmt.Sprintf("address=/.%s/127.0.0.1", domainSuffix)
	inConf := false
	for _, line := range strings.Split(string(data), "\n") {
		if strings.TrimSpace(line) == entry {
			inConf = true
			break
		}
	}
	if !inConf {
		return false
	}
	_, err = os.Stat(filepath.Join(resolverDir, domainSuffix))
	return err == nil
}

// IsDashboardConfigured returns true if spinner.test is fully configured
// (both dnsmasq.conf entry and /etc/resolver file).
func IsDashboardConfigured() bool {
	data, err := os.ReadFile(dnsmasqConf)
	if err != nil {
		return false
	}
	entry := fmt.Sprintf("address=/.%s/127.0.0.1", dashboardDomain)
	inConf := false
	for _, line := range strings.Split(string(data), "\n") {
		if strings.TrimSpace(line) == entry {
			inConf = true
			break
		}
	}
	if !inConf {
		return false
	}
	_, err = os.Stat(filepath.Join(resolverDir, dashboardDomain))
	return err == nil
}
