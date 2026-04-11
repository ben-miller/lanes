// testapp is a minimal web server used by spinner's integration tests and demo.
// It serves an HTML page that looks like a real dev app, showing branch/port info.
// spinner injects the PORT and BRANCH environment variables at startup.
package main

import (
	"encoding/json"
	"fmt"
	"html/template"
	"net/http"
	"os"
	"time"
)

const (
	ansiReset  = "\033[0m"
	ansiBold   = "\033[1m"
	ansiGray   = "\033[90m"
	ansiGreen  = "\033[32m"
	ansiCyan   = "\033[36m"
	ansiYellow = "\033[33m"
	ansiRed    = "\033[31m"
	ansiPurple = "\033[35m"
)

type statusRecorder struct {
	http.ResponseWriter
	code int
}

func (r *statusRecorder) WriteHeader(code int) {
	r.code = code
	r.ResponseWriter.WriteHeader(code)
}

func withLogging(branch string, next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		start := time.Now()
		rec := &statusRecorder{ResponseWriter: w, code: 200}
		next.ServeHTTP(rec, r)
		dur := time.Since(start)

		ts := ansiGray + time.Now().Format("15:04:05.000") + ansiReset
		branchCol := ansiPurple + branch + ansiReset
		method := ansiBold + r.Method + ansiReset
		path := ansiCyan + r.URL.RequestURI() + ansiReset
		status := colorStatus(rec.code)
		durStr := ansiGray + fmt.Sprintf("%v", dur.Round(time.Microsecond)) + ansiReset

		fmt.Printf("%s  [%s]  %s %s  %s  %s\n", ts, branchCol, method, path, status, durStr)
	})
}

func colorStatus(code int) string {
	s := fmt.Sprintf("%d", code)
	switch {
	case code >= 500:
		return ansiRed + ansiBold + s + ansiReset
	case code >= 400:
		return ansiYellow + s + ansiReset
	case code >= 300:
		return ansiCyan + s + ansiReset
	default:
		return ansiGreen + s + ansiReset
	}
}

var startedAt = time.Now()

var page = template.Must(template.New("page").Parse(`<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{{.Branch}} — testapp</title>
  <style>
    *, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }
    body {
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
      background: #0f1117;
      color: #e2e8f0;
      min-height: 100vh;
      display: flex;
      flex-direction: column;
    }

    header {
      border-bottom: 1px solid #1e2535;
      padding: 0 2rem;
      height: 56px;
      display: flex;
      align-items: center;
      gap: 1rem;
    }
    .logo { font-weight: 700; font-size: 1.1rem; color: #fff; letter-spacing: -0.02em; }
    .logo span { color: #6366f1; }
    nav { margin-left: auto; display: flex; gap: 1.5rem; }
    nav a {
      color: #94a3b8;
      text-decoration: none;
      font-size: 0.875rem;
      transition: color 0.15s;
    }
    nav a:hover { color: #e2e8f0; }

    main {
      flex: 1;
      padding: 3rem 2rem;
      max-width: 860px;
      margin: 0 auto;
      width: 100%;
    }

    .branch-pill {
      display: inline-flex;
      align-items: center;
      gap: 0.4rem;
      background: #1e2535;
      border: 1px solid #2d3748;
      border-radius: 999px;
      padding: 0.25rem 0.75rem;
      font-size: 0.8rem;
      color: #a5b4fc;
      margin-bottom: 1.5rem;
    }
    .branch-pill::before {
      content: "";
      width: 7px; height: 7px;
      background: #6366f1;
      border-radius: 50%;
      display: block;
    }

    h1 {
      font-size: 2rem;
      font-weight: 700;
      letter-spacing: -0.03em;
      color: #f8fafc;
      margin-bottom: 0.5rem;
      line-height: 1.2;
    }
    .subtitle {
      color: #64748b;
      font-size: 0.95rem;
      margin-bottom: 3rem;
    }

    .cards {
      display: grid;
      grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
      gap: 1rem;
      margin-bottom: 3rem;
    }
    .card {
      background: #161b27;
      border: 1px solid #1e2535;
      border-radius: 10px;
      padding: 1.25rem 1.5rem;
    }
    .card-label {
      font-size: 0.75rem;
      text-transform: uppercase;
      letter-spacing: 0.08em;
      color: #475569;
      margin-bottom: 0.4rem;
    }
    .card-value {
      font-size: 1.25rem;
      font-weight: 600;
      color: #e2e8f0;
      font-variant-numeric: tabular-nums;
    }
    .card-value.green { color: #4ade80; }
    .card-value.purple { color: #a5b4fc; }

    .notice {
      background: #161b27;
      border: 1px solid #1e2535;
      border-left: 3px solid #6366f1;
      border-radius: 6px;
      padding: 1rem 1.25rem;
      font-size: 0.875rem;
      color: #94a3b8;
      line-height: 1.6;
    }
    .notice code {
      background: #1e2535;
      padding: 0.1em 0.4em;
      border-radius: 4px;
      font-family: "SF Mono", "Fira Code", monospace;
      font-size: 0.85em;
      color: #a5b4fc;
    }

    footer {
      border-top: 1px solid #1e2535;
      padding: 1rem 2rem;
      font-size: 0.8rem;
      color: #334155;
      display: flex;
      justify-content: space-between;
    }
  </style>
</head>
<body>
  <header>
    <div class="logo">test<span>app</span></div>
    <nav>
      <a href="/">Home</a>
      <a href="/health">Health</a>
      <a href="/api/info">API</a>
    </nav>
  </header>

  <main>
    <div class="branch-pill">{{.Branch}}</div>
    <h1>Dev server running</h1>
    <p class="subtitle">Managed by spinner · testapp placeholder</p>

    <div class="cards">
      <div class="card">
        <div class="card-label">Branch</div>
        <div class="card-value purple">{{.Branch}}</div>
      </div>
      <div class="card">
        <div class="card-label">Port</div>
        <div class="card-value">{{.Port}}</div>
      </div>
      <div class="card">
        <div class="card-label">Status</div>
        <div class="card-value green">Running</div>
      </div>
      <div class="card">
        <div class="card-label">Uptime</div>
        <div class="card-value">{{.Uptime}}</div>
      </div>
    </div>

    <div class="notice">
      This is <code>testapp</code> — a placeholder used to verify spinner's
      process management. In a real project this would be your dev server
      (<code>mix phx.server</code>, <code>npm run dev</code>, etc.).
      JSON available at <code><a href="/api/info" style="color:inherit">/api/info</a></code>.
    </div>
  </main>

  <footer>
    <span>spinner testapp</span>
    <span>{{.Branch}} · localhost:{{.Port}}</span>
  </footer>
</body>
</html>
`))

type pageData struct {
	Branch string
	Port   string
	Uptime string
}

func main() {
	branch := os.Getenv("BRANCH")
	if branch == "" {
		branch = "unknown"
	}
	port := os.Getenv("PORT")
	if port == "" {
		port = "8080"
	}

	mux := http.NewServeMux()

	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		uptime := time.Since(startedAt).Round(time.Second).String()
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		page.Execute(w, pageData{Branch: branch, Port: port, Uptime: uptime})
	})

	mux.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		fmt.Fprintln(w, `{"ok":true}`)
	})

	mux.HandleFunc("/api/info", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(map[string]string{
			"branch": branch,
			"port":   port,
			"uptime": time.Since(startedAt).Round(time.Second).String(),
		})
	})

	fmt.Print(ansiBold + "testapp" + ansiReset + " listening on " + ansiCyan + ":" + port + ansiReset + " (branch=" + ansiPurple + branch + ansiReset + ")\n")
	if err := http.ListenAndServe(":"+port, withLogging(branch, mux)); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}
