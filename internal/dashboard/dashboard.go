package dashboard

import (
	"fmt"
	"html/template"
	"net/http"
	"strings"
	"time"

	"github.com/bmiller/spinner/internal/config"
	"github.com/bmiller/spinner/internal/state"
)

const Port = 7700

const tmplGlobal = `<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>spinner</title>
<style>
body { font-family: monospace; padding: 2rem; background: #111; color: #eee; }
h1 { color: #7ee; margin-bottom: 0.25rem; }
.sub { color: #888; margin-bottom: 2rem; font-size: 0.9rem; }
h2 { color: #aef; margin-top: 2rem; }
table { border-collapse: collapse; width: 100%; margin-top: 0.5rem; }
th { text-align: left; color: #888; padding: 0.3rem 1rem 0.3rem 0; border-bottom: 1px solid #333; }
td { padding: 0.4rem 1rem 0.4rem 0; border-bottom: 1px solid #222; }
.running { color: #7f7; }
.stopped { color: #f77; }
a { color: #7ae; }
</style>
</head>
<body>
<h1>spinner</h1>
<p class="sub">{{.UpdatedAt}}</p>
{{range .Projects}}
<h2><a href="/{{.Project}}">{{.Project}}</a></h2>
<table>
<tr><th>branch</th><th>url</th><th>port</th><th>status</th></tr>
{{range .Worktrees}}
<tr>
  <td>{{.Branch}}</td>
  <td><a href="{{.URL}}" target="_blank" rel="noopener">{{.URL}}</a></td>
  <td>{{.Port}}</td>
  <td class="{{.Status}}">{{.Status}}</td>
</tr>
{{else}}
<tr><td colspan="4" style="color:#555">no worktrees running</td></tr>
{{end}}
</table>
{{else}}
<p style="color:#555">No projects registered. Run <code>spinner init</code> in a project.</p>
{{end}}
</body>
</html>`

const tmplProject = `<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>spinner / {{.Project}}</title>
<style>
body { font-family: monospace; padding: 2rem; background: #111; color: #eee; }
h1 { color: #7ee; }
a { color: #7ae; }
table { border-collapse: collapse; width: 100%; margin-top: 1rem; }
th { text-align: left; color: #888; padding: 0.3rem 1rem 0.3rem 0; border-bottom: 1px solid #333; }
td { padding: 0.4rem 1rem 0.4rem 0; border-bottom: 1px solid #222; }
.running { color: #7f7; }
.stopped { color: #f77; }
</style>
</head>
<body>
<p><a href="/">← all projects</a></p>
<h1>{{.Project}}</h1>
<table>
<tr><th>branch</th><th>url</th><th>port</th><th>status</th></tr>
{{range .Worktrees}}
<tr>
  <td>{{.Branch}}</td>
  <td><a href="{{.URL}}" target="_blank" rel="noopener">{{.URL}}</a></td>
  <td>{{.Port}}</td>
  <td class="{{.Status}}">{{.Status}}</td>
</tr>
{{else}}
<tr><td colspan="4" style="color:#555">no worktrees running</td></tr>
{{end}}
</table>
</body>
</html>`

// Serve starts the dashboard HTTP server. It blocks until an error occurs.
func Serve() error {
	mux := http.NewServeMux()
	mux.HandleFunc("/", handleDashboard)
	addr := fmt.Sprintf("127.0.0.1:%d", Port)
	return http.ListenAndServe(addr, mux)
}

func handleDashboard(w http.ResponseWriter, r *http.Request) {
	// Path /PROJECT routes to a project-specific view.
	path := strings.TrimPrefix(r.URL.Path, "/")
	if path != "" {
		handleProject(w, r, path)
		return
	}

	global, err := config.LoadGlobal()
	if err != nil {
		http.Error(w, err.Error(), 500)
		return
	}

	type pageData struct {
		UpdatedAt string
		Projects  []*state.SpinnerState
	}

	var projects []*state.SpinnerState
	for _, repo := range global.Repos {
		s, err := state.Load(repo.Name)
		if err != nil {
			continue
		}
		projects = append(projects, s)
	}

	data := pageData{
		UpdatedAt: time.Now().Format("2006-01-02 15:04:05"),
		Projects:  projects,
	}

	t := template.Must(template.New("global").Parse(tmplGlobal))
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	t.Execute(w, data)
}

func handleProject(w http.ResponseWriter, r *http.Request, projectName string) {
	s, err := state.Load(projectName)
	if err != nil {
		http.Error(w, err.Error(), 500)
		return
	}

	t := template.Must(template.New("project").Parse(tmplProject))
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	t.Execute(w, s)
}
