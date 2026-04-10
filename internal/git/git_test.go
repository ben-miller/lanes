package git_test

import (
	"os"
	"os/exec"
	"path/filepath"
	"testing"

	"github.com/bmiller/spinner/internal/git"
)

// initRepo creates a temporary git repository with one commit and returns its path.
func initRepo(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	gitRun(t, dir, "init")
	gitRun(t, dir, "config", "user.email", "test@test.com")
	gitRun(t, dir, "config", "user.name", "Test")
	if err := os.WriteFile(filepath.Join(dir, "README"), []byte("test"), 0644); err != nil {
		t.Fatal(err)
	}
	gitRun(t, dir, "add", ".")
	gitRun(t, dir, "commit", "-m", "init")
	return dir
}

func gitRun(t *testing.T, dir string, args ...string) {
	t.Helper()
	cmd := exec.Command("git", args...)
	cmd.Dir = dir
	// Ensure commits work on machines without global git config.
	cmd.Env = append(os.Environ(),
		"GIT_AUTHOR_NAME=Test",
		"GIT_AUTHOR_EMAIL=test@test.com",
		"GIT_COMMITTER_NAME=Test",
		"GIT_COMMITTER_EMAIL=test@test.com",
	)
	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("git %v: %v\n%s", args, err, out)
	}
}

func addWorktree(t *testing.T, mainRoot, branch string) string {
	t.Helper()
	wtDir := t.TempDir()
	gitRun(t, mainRoot, "worktree", "add", "-b", branch, wtDir)
	t.Cleanup(func() {
		exec.Command("git", "-C", mainRoot, "worktree", "remove", "--force", wtDir).Run()
	})
	return wtDir
}

func TestMainWorktreeRoot_FromMain(t *testing.T) {
	dir := initRepo(t)
	got, err := git.MainWorktreeRoot(dir)
	if err != nil {
		t.Fatal(err)
	}
	want := evalSymlinks(t, dir)
	if got != want {
		t.Errorf("MainWorktreeRoot = %q, want %q", got, want)
	}
}

func TestMainWorktreeRoot_FromLinked(t *testing.T) {
	main := initRepo(t)
	wt := addWorktree(t, main, "feature")

	got, err := git.MainWorktreeRoot(wt)
	if err != nil {
		t.Fatal(err)
	}
	want := evalSymlinks(t, main)
	if got != want {
		t.Errorf("MainWorktreeRoot from linked = %q, want %q", got, want)
	}
}

func TestCurrentBranch(t *testing.T) {
	dir := initRepo(t)
	branch, err := git.CurrentBranch(dir)
	if err != nil {
		t.Fatal(err)
	}
	// git init defaults to "main" or "master" depending on config/version.
	if branch != "main" && branch != "master" {
		t.Errorf("unexpected default branch %q", branch)
	}
}

func TestCurrentBranch_LinkedWorktree(t *testing.T) {
	main := initRepo(t)
	wt := addWorktree(t, main, "my-feature")

	branch, err := git.CurrentBranch(wt)
	if err != nil {
		t.Fatal(err)
	}
	if branch != "my-feature" {
		t.Errorf("CurrentBranch = %q, want %q", branch, "my-feature")
	}
}

func TestListWorktrees_MainOnly(t *testing.T) {
	dir := initRepo(t)
	wts, err := git.ListWorktrees(dir)
	if err != nil {
		t.Fatal(err)
	}
	if len(wts) != 1 {
		t.Fatalf("got %d worktrees, want 1", len(wts))
	}
	if !wts[0].IsMain {
		t.Error("sole worktree should be marked IsMain")
	}
	want := evalSymlinks(t, dir)
	if wts[0].Path != want {
		t.Errorf("worktree path = %q, want %q", wts[0].Path, want)
	}
}

func TestListWorktrees_WithLinked(t *testing.T) {
	main := initRepo(t)
	addWorktree(t, main, "feature-a")
	addWorktree(t, main, "feature-b")

	wts, err := git.ListWorktrees(main)
	if err != nil {
		t.Fatal(err)
	}
	if len(wts) != 3 {
		t.Fatalf("got %d worktrees, want 3", len(wts))
	}
	if !wts[0].IsMain {
		t.Error("first worktree should be main")
	}

	branches := map[string]bool{}
	for _, wt := range wts {
		branches[wt.Branch] = true
	}
	if !branches["feature-a"] {
		t.Error("missing feature-a")
	}
	if !branches["feature-b"] {
		t.Error("missing feature-b")
	}
}

func TestWorktreesDir(t *testing.T) {
	dir := initRepo(t)
	got := git.WorktreesDir(dir)
	want := filepath.Join(dir, ".git", "worktrees")
	if got != want {
		t.Errorf("WorktreesDir = %q, want %q", got, want)
	}
}

func evalSymlinks(t *testing.T, path string) string {
	t.Helper()
	resolved, err := filepath.EvalSymlinks(path)
	if err != nil {
		t.Fatalf("EvalSymlinks(%q): %v", path, err)
	}
	return resolved
}
