package cmd

import (
	"fmt"
	"io"
	"os"
	"time"

	"github.com/bmiller/spinner/internal/config"
	"github.com/bmiller/spinner/internal/git"
	"github.com/bmiller/spinner/internal/state"
	"github.com/spf13/cobra"
)

func newLogsCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "logs [branch]",
		Short: "Tail logs for a worktree (default: current branch)",
		Args:  cobra.MaximumNArgs(1),
		RunE:  runLogs,
	}
	cmd.Flags().IntP("lines", "n", 100, "Number of lines to show before following (0 = follow from end)")
	return cmd
}

func runLogs(cmd *cobra.Command, args []string) error {
	lines, _ := cmd.Flags().GetInt("lines")

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

	var branch string
	if len(args) > 0 {
		branch = args[0]
	} else {
		branch, err = git.CurrentBranch(cwd)
		if err != nil {
			return err
		}
	}

	logFile := state.LogFile(cfg.Project.Name, branch)
	if _, err := os.Stat(logFile); os.IsNotExist(err) {
		return fmt.Errorf("no log file for branch %q (has spinner started for this worktree?)", branch)
	}

	f, err := os.Open(logFile)
	if err != nil {
		return err
	}
	defer f.Close()

	if err := seekLastLines(f, lines); err != nil {
		return err
	}

	fmt.Fprintf(os.Stderr, "Tailing logs for %s/%s (Ctrl+C to stop)\n", cfg.Project.Name, branch)

	buf := make([]byte, 4096)
	for {
		n, err := f.Read(buf)
		if n > 0 {
			os.Stdout.Write(buf[:n])
		}
		if err == io.EOF {
			time.Sleep(200 * time.Millisecond)
			continue
		}
		if err != nil {
			return err
		}
	}
}

// seekLastLines positions f so that the next read returns the last n lines.
// If n == 0 it seeks to the end (follow-only). If the file has fewer than n
// lines it seeks to the beginning.
func seekLastLines(f *os.File, n int) error {
	if n == 0 {
		_, err := f.Seek(0, io.SeekEnd)
		return err
	}

	size, err := f.Seek(0, io.SeekEnd)
	if err != nil {
		return err
	}
	if size == 0 {
		return nil
	}

	const chunkSize = 4096
	remaining := size
	newlines := 0

	for remaining > 0 {
		chunkLen := int64(chunkSize)
		if chunkLen > remaining {
			chunkLen = remaining
		}
		pos := remaining - chunkLen

		chunk := make([]byte, chunkLen)
		if _, err := f.ReadAt(chunk, pos); err != nil {
			return err
		}

		for i := len(chunk) - 1; i >= 0; i-- {
			if chunk[i] == '\n' {
				newlines++
				if newlines > n {
					_, err = f.Seek(pos+int64(i)+1, io.SeekStart)
					return err
				}
			}
		}
		remaining = pos
	}

	// Fewer lines than requested — show everything.
	_, err = f.Seek(0, io.SeekStart)
	return err
}
