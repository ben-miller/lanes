package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
)

type Session struct {
	SessionID         string `json:"session_id"`
	ClaudeSessionName string `json:"claude_session_name"`
	ZellijSession     string `json:"zellij_session"`
	ZellijPaneID      *int   `json:"zellij_pane_id"`
	WeztermTabID      *int   `json:"wezterm_tab_id"`
	CWD               string `json:"cwd"`
	StartedAt         string `json:"started_at"`
	State             string `json:"state"`
	AITitle           string `json:"ai_title,omitempty"`
}

func registryDir() string {
	return filepath.Join(os.Getenv("HOME"), ".claude", "active-sessions")
}

func cursorFile() string {
	return filepath.Join(registryDir(), ".cursor")
}

func loadSession(id string) (*Session, error) {
	data, err := os.ReadFile(filepath.Join(registryDir(), id+".json"))
	if err != nil {
		return nil, err
	}
	var s Session
	if err := json.Unmarshal(data, &s); err != nil {
		return nil, err
	}
	return &s, nil
}

func loadAllSessions() []*Session {
	files, _ := filepath.Glob(filepath.Join(registryDir(), "*.json"))
	var sessions []*Session
	for _, f := range files {
		data, err := os.ReadFile(f)
		if err != nil {
			continue
		}
		var s Session
		if err := json.Unmarshal(data, &s); err != nil {
			continue
		}
		sessions = append(sessions, &s)
	}
	return sessions
}

func aiTitleFor(cwd, sessionID string) string {
	projectDir := strings.ReplaceAll(cwd, "/", "-")
	transcript := filepath.Join(os.Getenv("HOME"), ".claude", "projects", projectDir, sessionID+".jsonl")

	f, err := os.Open(transcript)
	if err != nil {
		return ""
	}
	defer f.Close()

	var last string
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 1024*1024), 1024*1024)
	for scanner.Scan() {
		var record struct {
			AITitle string `json:"aiTitle"`
		}
		if err := json.Unmarshal(scanner.Bytes(), &record); err == nil && record.AITitle != "" {
			last = record.AITitle
		}
	}
	return last
}

func findWeztermSocket() string {
	pattern := filepath.Join(os.Getenv("HOME"), ".local", "share", "wezterm", "gui-sock-*")
	matches, _ := filepath.Glob(pattern)
	if len(matches) == 0 {
		return ""
	}
	sort.Slice(matches, func(i, j int) bool {
		return socketPID(matches[i]) < socketPID(matches[j])
	})
	return matches[len(matches)-1]
}

func socketPID(path string) int {
	parts := strings.Split(filepath.Base(path), "-")
	if len(parts) == 0 {
		return 0
	}
	pid, _ := strconv.Atoi(parts[len(parts)-1])
	return pid
}

func sortedSessions() []*Session {
	sessions := loadAllSessions()
	sort.Slice(sessions, func(i, j int) bool {
		if sessions[i].ClaudeSessionName != sessions[j].ClaudeSessionName {
			return sessions[i].ClaudeSessionName < sessions[j].ClaudeSessionName
		}
		return sessions[i].SessionID < sessions[j].SessionID
	})
	return sessions
}

func doSwitch(id string) {
	s, err := loadSession(id)
	if err != nil {
		fmt.Fprintf(os.Stderr, "error: session not found: %s\n", id)
		os.Exit(1)
	}

	os.WriteFile(cursorFile(), []byte(id), 0644)

	if socket := findWeztermSocket(); socket != "" && s.WeztermTabID != nil {
		cmd := exec.Command("wezterm", "cli", "activate-tab", "--tab-id", strconv.Itoa(*s.WeztermTabID))
		cmd.Env = append(os.Environ(), "WEZTERM_UNIX_SOCKET="+socket)
		cmd.Run()
	}

	if s.ZellijSession != "" && s.ZellijPaneID != nil {
		exec.Command("zellij", "--session", s.ZellijSession, "action", "focus-pane-id", strconv.Itoa(*s.ZellijPaneID)).Run()
	}
}

func cmdList() {
	sessions := loadAllSessions()
	for _, s := range sessions {
		s.AITitle = aiTitleFor(s.CWD, s.SessionID)
	}
	if sessions == nil {
		sessions = []*Session{}
	}
	out, _ := json.MarshalIndent(sessions, "", "  ")
	fmt.Println(string(out))
}

func cmdGet(id string) {
	s, err := loadSession(id)
	if err != nil {
		fmt.Fprintf(os.Stderr, "error: session not found: %s\n", id)
		os.Exit(1)
	}
	out, _ := json.MarshalIndent(s, "", "  ")
	fmt.Println(string(out))
}

func cmdSwitch(id string) {
	doSwitch(id)
}

func cmdCycle(direction int) {
	sessions := sortedSessions()
	if len(sessions) == 0 {
		return
	}

	cursor := ""
	if data, err := os.ReadFile(cursorFile()); err == nil {
		cursor = strings.TrimSpace(string(data))
	}

	currentIndex := -1
	for i, s := range sessions {
		if s.SessionID == cursor {
			currentIndex = i
			break
		}
	}

	n := len(sessions)
	nextIndex := ((currentIndex + direction) % n + n) % n
	doSwitch(sessions[nextIndex].SessionID)
}

func usage() {
	fmt.Fprintln(os.Stderr, "Usage: claude-session sessions {list|get <id>|switch <id>|next|prev}")
	os.Exit(1)
}

func main() {
	if len(os.Args) < 3 || os.Args[1] != "sessions" {
		usage()
	}

	op := os.Args[2]
	args := os.Args[3:]

	switch op {
	case "list":
		cmdList()
	case "get":
		if len(args) < 1 {
			usage()
		}
		cmdGet(args[0])
	case "switch":
		if len(args) < 1 {
			usage()
		}
		cmdSwitch(args[0])
	case "next":
		cmdCycle(1)
	case "prev":
		cmdCycle(-1)
	default:
		usage()
	}
}
