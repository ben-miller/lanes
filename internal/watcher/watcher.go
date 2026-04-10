package watcher

import (
	"os"

	"github.com/fsnotify/fsnotify"
)

// Event signals a worktree was added or removed.
type Event struct {
	Name    string // subdirectory name under .git/worktrees/
	Removed bool
}

// Watch monitors dir for subdirectory creation/removal and sends events on ch.
// It blocks until ctx is done or an error occurs.
func Watch(dir string, events chan<- Event, stop <-chan struct{}) error {
	if err := os.MkdirAll(dir, 0755); err != nil {
		return err
	}

	w, err := fsnotify.NewWatcher()
	if err != nil {
		return err
	}
	defer w.Close()

	if err := w.Add(dir); err != nil {
		return err
	}

	for {
		select {
		case <-stop:
			return nil
		case ev, ok := <-w.Events:
			if !ok {
				return nil
			}
			switch {
			case ev.Has(fsnotify.Create):
				events <- Event{Name: ev.Name}
			case ev.Has(fsnotify.Remove), ev.Has(fsnotify.Rename):
				events <- Event{Name: ev.Name, Removed: true}
			}
		case err, ok := <-w.Errors:
			if !ok {
				return nil
			}
			// Log but don't fatal on watcher errors.
			_ = err
		}
	}
}
