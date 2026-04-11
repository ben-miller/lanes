package git

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

// Worktree represents a git worktree.
type Worktree struct {
	Path   string
	Branch string
	IsMain bool
}

// MainWorktreeRoot returns the root of the main worktree for the git repo
// containing dir (which may itself be a linked worktree).
func MainWorktreeRoot(dir string) (string, error) {
	cmd := exec.Command("git", "-C", dir, "rev-parse", "--git-common-dir")
	out, err := cmd.Output()
	if err != nil {
		return "", fmt.Errorf("not a git repo: %w", err)
	}
	commonDir := strings.TrimSpace(string(out))
	if !filepath.IsAbs(commonDir) {
		commonDir = filepath.Join(dir, commonDir)
	}
	// commonDir is .git (or .git/worktrees/NAME for linked worktrees).
	// The main worktree root is always the parent of the top-level .git directory.
	// Walk up until we find a directory named ".git" whose parent is the main root.
	abs, err := filepath.Abs(commonDir)
	if err != nil {
		return "", err
	}
	// Strip trailing /worktrees/... if present
	for {
		base := filepath.Base(abs)
		if base == ".git" {
			root := filepath.Dir(abs)
			// Resolve symlinks so callers get a canonical path (important on macOS
			// where /var/folders is a symlink to /private/var/folders).
			if resolved, err := filepath.EvalSymlinks(root); err == nil {
				root = resolved
			}
			return root, nil
		}
		parent := filepath.Dir(abs)
		if parent == abs {
			break
		}
		abs = parent
	}
	return "", fmt.Errorf("could not find .git directory in %s", commonDir)
}

// CurrentBranch returns the current branch name for dir.
func CurrentBranch(dir string) (string, error) {
	cmd := exec.Command("git", "-C", dir, "rev-parse", "--abbrev-ref", "HEAD")
	out, err := cmd.Output()
	if err != nil {
		return "", fmt.Errorf("git rev-parse failed: %w", err)
	}
	return strings.TrimSpace(string(out)), nil
}

// ListWorktrees returns all worktrees for the repo rooted at mainRoot.
func ListWorktrees(mainRoot string) ([]Worktree, error) {
	cmd := exec.Command("git", "-C", mainRoot, "worktree", "list", "--porcelain")
	out, err := cmd.Output()
	if err != nil {
		return nil, fmt.Errorf("git worktree list: %w", err)
	}

	var worktrees []Worktree
	var current Worktree
	first := true

	for _, line := range strings.Split(string(out), "\n") {
		if line == "" {
			if current.Path != "" {
				current.IsMain = first
				worktrees = append(worktrees, current)
				current = Worktree{}
				if first {
					first = false
				}
			}
			continue
		}
		if strings.HasPrefix(line, "worktree ") {
			current.Path = strings.TrimPrefix(line, "worktree ")
		} else if strings.HasPrefix(line, "branch ") {
			branch := strings.TrimPrefix(line, "branch ")
			current.Branch = strings.TrimPrefix(branch, "refs/heads/")
		}
	}
	if current.Path != "" {
		current.IsMain = first
		worktrees = append(worktrees, current)
	}
	return worktrees, nil
}

// WorktreesDir returns the .git/worktrees directory for the main worktree.
func WorktreesDir(mainRoot string) string {
	return filepath.Join(mainRoot, ".git", "worktrees")
}

// EnsureWorktreesDirExists creates .git/worktrees if it doesn't exist yet.
// Git only creates this directory once the first linked worktree is added.
func EnsureWorktreesDirExists(mainRoot string) error {
	dir := WorktreesDir(mainRoot)
	return os.MkdirAll(dir, 0755)
}
