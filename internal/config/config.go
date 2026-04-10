package config

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/BurntSushi/toml"
)

// ProjectConfig is the per-project spinner.toml.
type ProjectConfig struct {
	Project ProjectSection `toml:"project"`
	Server  ServerSection  `toml:"server"`
}

type ProjectSection struct {
	Name         string    `toml:"name"`
	DomainSuffix string    `toml:"domain_suffix"`
	PortRange    PortRange `toml:"port_range"`
}

type PortRange struct {
	Min int `toml:"min"`
	Max int `toml:"max"`
}

type ServerSection struct {
	Command string            `toml:"command"`
	Env     map[string]string `toml:"env"`
}

// GlobalConfig is ~/.config/spinner/registry.toml.
type GlobalConfig struct {
	Repos []RepoEntry `toml:"repos"`
}

type RepoEntry struct {
	Path string `toml:"path"`
	Name string `toml:"name"`
}

func GlobalConfigPath() string {
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".config", "spinner", "registry.toml")
}

func LoadGlobal() (*GlobalConfig, error) {
	path := GlobalConfigPath()
	var cfg GlobalConfig
	if _, err := os.Stat(path); os.IsNotExist(err) {
		return &cfg, nil
	}
	if _, err := toml.DecodeFile(path, &cfg); err != nil {
		return nil, fmt.Errorf("reading %s: %w", path, err)
	}
	return &cfg, nil
}

func SaveGlobal(cfg *GlobalConfig) error {
	path := GlobalConfigPath()
	if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
		return err
	}
	f, err := os.Create(path)
	if err != nil {
		return err
	}
	defer f.Close()
	return toml.NewEncoder(f).Encode(cfg)
}

func LoadProject(dir string) (*ProjectConfig, error) {
	path := filepath.Join(dir, "spinner.toml")
	var cfg ProjectConfig
	if _, err := toml.DecodeFile(path, &cfg); err != nil {
		return nil, fmt.Errorf("reading %s: %w", path, err)
	}
	return &cfg, nil
}

func SaveProject(dir string, cfg *ProjectConfig) error {
	path := filepath.Join(dir, "spinner.toml")
	f, err := os.Create(path)
	if err != nil {
		return err
	}
	defer f.Close()
	return toml.NewEncoder(f).Encode(cfg)
}

// ExpandEnv substitutes {branch} in env values.
func ExpandEnv(env map[string]string, branch string) map[string]string {
	result := make(map[string]string, len(env))
	for k, v := range env {
		result[k] = strings.ReplaceAll(v, "{branch}", branch)
	}
	return result
}

// StateDir returns the runtime state directory for a project.
func StateDir(projectName string) string {
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".local", "share", "spinner", projectName)
}
