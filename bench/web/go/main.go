// TechEmpower Framework Benchmarks server on net/http + pgx/v5 (pgxpool): /json, /plaintext,
// /db, /queries?queries=N, /fortunes and /updates?queries=N (bench/web/README.md has the rules).
// Usage: server [port]; env PORT, HOST (127.0.0.1), DATABASE_URL, DB_POOL (default 2 × cores).
package main

import (
	"context"
	"encoding/json"
	"log"
	"math/rand/v2"
	"net/http"
	"os"
	"runtime"
	"sort"
	"strconv"
	"strings"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

const (
	selectWorld    = `SELECT id, randomnumber FROM world WHERE id = $1`
	selectFortunes = `SELECT id, message FROM fortune`
	// One statement for any N: the sorted ids and new numbers travel as two int arrays.
	updateWorlds = `UPDATE world SET randomnumber = u.r FROM (SELECT unnest($1::int[]) AS id, ` +
		`unnest($2::int[]) AS r) AS u WHERE world.id = u.id`
)

type World struct {
	ID           int32 `json:"id"`
	RandomNumber int32 `json:"randomNumber"`
}

type Fortune struct {
	ID      int32
	Message string
}

// The same entities as the other implementations (html/template would write &#34; for ").
var escaper = strings.NewReplacer("&", "&amp;", "<", "&lt;", ">", "&gt;", `"`, "&quot;", "'", "&#39;")

const fortunesHead = `<!DOCTYPE html><html><head><title>Fortunes</title></head><body><table>` +
	`<tr><th>id</th><th>message</th></tr>`

func fortunesHTML(fortunes []Fortune) []byte {
	var b strings.Builder
	b.Grow(2048)
	b.WriteString(fortunesHead)
	for _, f := range fortunes {
		b.WriteString("<tr><td>")
		b.WriteString(strconv.Itoa(int(f.ID)))
		b.WriteString("</td><td>")
		escaper.WriteString(&b, f.Message)
		b.WriteString("</td></tr>")
	}
	b.WriteString("</table></body></html>")
	return []byte(b.String())
}

type server struct{ pool *pgxpool.Pool }

func randomID() int32 { return rand.Int32N(10000) + 1 }

// queryCount parses ?queries=N, clamped to 1..500; missing or not a number is 1.
func queryCount(r *http.Request) int {
	n, err := strconv.Atoi(r.URL.Query().Get("queries"))
	if err != nil || n < 1 {
		return 1
	}
	if n > 500 {
		return 500
	}
	return n
}

// fetchWorlds reads n random rows, one query each, pipelined as a pgx batch on one connection.
func (s *server) fetchWorlds(ctx context.Context, n int) ([]World, error) {
	batch := &pgx.Batch{}
	for i := 0; i < n; i++ {
		batch.Queue(selectWorld, randomID())
	}
	results := s.pool.SendBatch(ctx, batch)
	defer results.Close()
	worlds := make([]World, n)
	for i := range worlds {
		if err := results.QueryRow().Scan(&worlds[i].ID, &worlds[i].RandomNumber); err != nil {
			return nil, err
		}
	}
	return worlds, nil
}

func (s *server) updateWorlds(ctx context.Context, n int) ([]World, error) {
	worlds, err := s.fetchWorlds(ctx, n)
	if err != nil {
		return nil, err
	}
	for i := range worlds {
		worlds[i].RandomNumber = randomID()
	}
	sorted := append([]World(nil), worlds...)
	sort.Slice(sorted, func(i, j int) bool { return sorted[i].ID < sorted[j].ID })
	ids := make([]int32, n)
	values := make([]int32, n)
	for i, w := range sorted {
		ids[i], values[i] = w.ID, w.RandomNumber
	}
	_, err = s.pool.Exec(ctx, updateWorlds, ids, values)
	return worlds, err
}

func (s *server) fortunes(ctx context.Context) ([]Fortune, error) {
	rows, err := s.pool.Query(ctx, selectFortunes)
	if err != nil {
		return nil, err
	}
	fortunes, err := pgx.CollectRows(rows, pgx.RowToStructByPos[Fortune])
	if err != nil {
		return nil, err
	}
	fortunes = append(fortunes, Fortune{0, "Additional fortune added at request time."})
	sort.Slice(fortunes, func(i, j int) bool { return fortunes[i].Message < fortunes[j].Message })
	return fortunes, nil
}

func header(w http.ResponseWriter, contentType string) {
	h := w.Header()
	h["Content-Type"] = []string{contentType}
	h["Server"] = []string{"go"}
}

func writeJSON(w http.ResponseWriter, v any) {
	body, _ := json.Marshal(v)
	header(w, "application/json")
	h := w.Header()
	h["Content-Length"] = []string{strconv.Itoa(len(body))}
	w.Write(body)
}

func fail(w http.ResponseWriter, err error) {
	log.Printf("request failed: %v", err)
	header(w, "text/plain; charset=utf-8")
	w.WriteHeader(http.StatusInternalServerError)
	w.Write([]byte("internal error"))
}

func (s *server) routes() *http.ServeMux {
	mux := http.NewServeMux()
	mux.HandleFunc("/plaintext", func(w http.ResponseWriter, r *http.Request) {
		header(w, "text/plain; charset=utf-8")
		w.Write([]byte("Hello, World!"))
	})
	mux.HandleFunc("/json", func(w http.ResponseWriter, r *http.Request) {
		writeJSON(w, map[string]string{"message": "Hello, World!"})
	})
	mux.HandleFunc("/db", func(w http.ResponseWriter, r *http.Request) {
		worlds, err := s.fetchWorlds(r.Context(), 1)
		if err != nil {
			fail(w, err)
			return
		}
		writeJSON(w, worlds[0])
	})
	mux.HandleFunc("/queries", func(w http.ResponseWriter, r *http.Request) {
		worlds, err := s.fetchWorlds(r.Context(), queryCount(r))
		if err != nil {
			fail(w, err)
			return
		}
		writeJSON(w, worlds)
	})
	mux.HandleFunc("/updates", func(w http.ResponseWriter, r *http.Request) {
		worlds, err := s.updateWorlds(r.Context(), queryCount(r))
		if err != nil {
			fail(w, err)
			return
		}
		writeJSON(w, worlds)
	})
	mux.HandleFunc("/fortunes", func(w http.ResponseWriter, r *http.Request) {
		fortunes, err := s.fortunes(r.Context())
		if err != nil {
			fail(w, err)
			return
		}
		body := fortunesHTML(fortunes)
		header(w, "text/html; charset=utf-8")
		w.Header()["Content-Length"] = []string{strconv.Itoa(len(body))}
		w.Write(body)
	})
	return mux
}

func env(name, fallback string) string {
	if v := os.Getenv(name); v != "" {
		return v
	}
	return fallback
}

func main() {
	port := env("PORT", "8080")
	if len(os.Args) > 1 {
		port = os.Args[1]
	}
	config, err := pgxpool.ParseConfig(env("DATABASE_URL",
		"postgres://benchmarkdbuser:benchmarkdbpass@127.0.0.1:5432/hello_world"))
	if err != nil {
		log.Fatal(err)
	}
	poolSize, err := strconv.Atoi(env("DB_POOL", strconv.Itoa(runtime.NumCPU()*2)))
	if err != nil {
		log.Fatal(err)
	}
	config.MaxConns = int32(poolSize)
	pool, err := pgxpool.NewWithConfig(context.Background(), config)
	if err != nil {
		log.Fatal(err)
	}
	s := &server{pool: pool}
	addr := env("HOST", "127.0.0.1") + ":" + port
	log.Printf("listening on http://%s", addr)
	log.Fatal(http.ListenAndServe(addr, s.routes()))
}
