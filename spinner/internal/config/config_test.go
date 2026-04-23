package config

import (
	"os"
	"path/filepath"
	"testing"
)

func TestExpandEnv(t *testing.T) {
	env := map[string]string{
		"DATABASE_URL": "postgres://localhost/myapp_{branch}_dev",
		"MIX_ENV":      "dev",
		"NO_TEMPLATE":  "static-value",
	}

	got := ExpandEnv(env, "feature-foo")

	if got["DATABASE_URL"] != "postgres://localhost/myapp_feature-foo_dev" {
		t.Errorf("DATABASE_URL = %q, want %q", got["DATABASE_URL"], "postgres://localhost/myapp_feature-foo_dev")
	}
	if got["MIX_ENV"] != "dev" {
		t.Errorf("MIX_ENV = %q, want %q", got["MIX_ENV"], "dev")
	}
	if got["NO_TEMPLATE"] != "static-value" {
		t.Errorf("NO_TEMPLATE = %q, want %q", got["NO_TEMPLATE"], "static-value")
	}
}

func TestExpandEnvBranchSlug(t *testing.T) {
	env := map[string]string{
		"PHX_HOST": "{branch-slug}.sheetwork",
		"RAW":      "{branch}",
	}

	got := ExpandEnv(env, "feature/my-thing")

	if got["PHX_HOST"] != "feature-my-thing.sheetwork" {
		t.Errorf("PHX_HOST = %q, want %q", got["PHX_HOST"], "feature-my-thing.sheetwork")
	}
	if got["RAW"] != "feature/my-thing" {
		t.Errorf("RAW = %q, want %q", got["RAW"], "feature/my-thing")
	}
}

func TestExpandEnvDoesNotMutateOriginal(t *testing.T) {
	env := map[string]string{"KEY": "{branch}"}
	ExpandEnv(env, "mybranch")
	if env["KEY"] != "{branch}" {
		t.Error("ExpandEnv mutated the original map")
	}
}

func TestLoadProject(t *testing.T) {
	dir := t.TempDir()
	toml := `
[project]
name = "myapp"
domain_suffix = "myapp.test"

[project.port_range]
min = 4100
max = 4199

[server]
command = "mix phx.server"

[server.env]
MIX_ENV = "dev"
DATABASE_URL = "postgres://localhost/myapp_{branch}_dev"
`
	if err := os.WriteFile(filepath.Join(dir, "spinner.toml"), []byte(toml), 0644); err != nil {
		t.Fatal(err)
	}

	cfg, err := LoadProject(dir)
	if err != nil {
		t.Fatalf("LoadProject: %v", err)
	}

	if cfg.Project.Name != "myapp" {
		t.Errorf("Name = %q, want %q", cfg.Project.Name, "myapp")
	}
	if cfg.Project.PortRange.Min != 4100 {
		t.Errorf("PortRange.Min = %d, want 4100", cfg.Project.PortRange.Min)
	}
	if cfg.Project.PortRange.Max != 4199 {
		t.Errorf("PortRange.Max = %d, want 4199", cfg.Project.PortRange.Max)
	}
	if cfg.Server.Command != "mix phx.server" {
		t.Errorf("Command = %q, want %q", cfg.Server.Command, "mix phx.server")
	}
	if cfg.Server.Env["MIX_ENV"] != "dev" {
		t.Errorf("MIX_ENV = %q, want %q", cfg.Server.Env["MIX_ENV"], "dev")
	}
}

func TestSaveAndLoadGlobal(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	cfg := &GlobalConfig{
		Repos: []RepoEntry{
			{Path: "/some/path", Name: "myapp"},
		},
	}

	if err := SaveGlobal(cfg); err != nil {
		t.Fatalf("SaveGlobal: %v", err)
	}

	loaded, err := LoadGlobal()
	if err != nil {
		t.Fatalf("LoadGlobal: %v", err)
	}

	if len(loaded.Repos) != 1 {
		t.Fatalf("len(Repos) = %d, want 1", len(loaded.Repos))
	}
	if loaded.Repos[0].Name != "myapp" {
		t.Errorf("Name = %q, want %q", loaded.Repos[0].Name, "myapp")
	}
}

func TestLoadGlobalMissing(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	cfg, err := LoadGlobal()
	if err != nil {
		t.Fatalf("LoadGlobal with missing file: %v", err)
	}
	if len(cfg.Repos) != 0 {
		t.Errorf("expected empty repos, got %d", len(cfg.Repos))
	}
}
