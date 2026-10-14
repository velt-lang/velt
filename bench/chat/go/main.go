// chatbench: the Go implementation of the chat server described in ../SPEC.md.
//
// One goroutine reads each connection; a second goroutine per connection owns all writes
// (the usual gorilla/websocket pattern), fed by a buffered send channel so that any number of
// broadcasters can deliver to it concurrently. Rooms live in one map guarded by a RWMutex:
// broadcasts take the read lock, joins and leaves the write lock.
package main

import (
	"bytes"
	"crypto/hmac"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"errors"
	"log"
	"net"
	"net/http"
	"os"
	"regexp"
	"strings"
	"sync"
	"sync/atomic"
	"time"
	"unicode/utf8"

	"github.com/gorilla/websocket"
	_ "github.com/mattn/go-sqlite3"
)

const (
	secret      = "chatbench-secret"
	historySize = 20
	maxText     = 1000

	// sendQueue is the number of outgoing messages a connection may have pending. A client
	// that falls this far behind is dropped (see client.enqueue).
	sendQueue    = 256
	writeTimeout = 10 * time.Second
	maxFrame     = 64 << 10
)

var nameRE = regexp.MustCompile(`^[a-z0-9]{1,32}$`)

// ---- wire messages (field order = key order in the spec) ----

type historyItem struct {
	ID   int64  `json:"id"`
	User string `json:"user"`
	Text string `json:"text"`
	TS   int64  `json:"ts"`
}

type welcomeMsg struct {
	Type    string        `json:"type"`
	User    string        `json:"user"`
	Room    string        `json:"room"`
	History []historyItem `json:"history"`
}

type chatMsg struct {
	Type string `json:"type"`
	ID   int64  `json:"id"`
	Room string `json:"room"`
	User string `json:"user"`
	Text string `json:"text"`
	TS   int64  `json:"ts"`
}

type pongMsg struct {
	Type string          `json:"type"`
	T    json.RawMessage `json:"t"`
}

type errorMsg struct {
	Type  string `json:"type"`
	Error string `json:"error"`
}

type statsMsg struct {
	Connections int64 `json:"connections"`
	Rooms       int   `json:"rooms"`
	Messages    int64 `json:"messages"`
}

// inbound is a client → server message. Text is a pointer and T raw so that a missing or
// mistyped field can be told apart from an empty one.
type inbound struct {
	Type string          `json:"type"`
	Text *string         `json:"text"`
	T    json.RawMessage `json:"t"`
}

// encode marshals v like JSON.stringify would: no HTML escaping of <, > and &, no trailing
// newline. (encoding/json escapes those by default, which the other implementations don't.)
func encode(v any) []byte {
	var buf bytes.Buffer
	enc := json.NewEncoder(&buf)
	enc.SetEscapeHTML(false)
	if err := enc.Encode(v); err != nil {
		panic("encode: " + err.Error()) // only our own fixed types are encoded
	}
	return bytes.TrimSuffix(buf.Bytes(), []byte{'\n'})
}

var badMessage = encode(errorMsg{Type: "error", Error: "bad message"})

// ---- server state ----

type client struct {
	conn  *websocket.Conn
	user  string
	room  string
	send  chan []byte
	done  chan struct{}
	close sync.Once
}

// shutdown stops the writer and closes the socket, which also ends the reader. Safe to call
// from any goroutine, any number of times.
func (c *client) shutdown() {
	c.close.Do(func() {
		close(c.done)
		c.conn.Close()
	})
}

// enqueue hands msg to the client's writer without blocking. Policy: if the client's queue is
// full it is too slow to keep up with its room, so it is disconnected rather than allowed to
// stall the broadcaster (or the whole room) or to buffer without bound.
func (c *client) enqueue(msg []byte) {
	select {
	case c.send <- msg:
	case <-c.done:
	default:
		c.shutdown()
	}
}

func (c *client) writer() {
	defer c.shutdown()
	for {
		select {
		case msg := <-c.send:
			c.conn.SetWriteDeadline(time.Now().Add(writeTimeout))
			if err := c.conn.WriteMessage(websocket.TextMessage, msg); err != nil {
				return
			}
		case <-c.done:
			return
		}
	}
}

type server struct {
	db       *sql.DB
	insert   *sql.Stmt
	history  *sql.Stmt
	upgrader websocket.Upgrader
	mu       sync.RWMutex
	rooms    map[string]map[*client]struct{}
	conns    atomic.Int64
	messages atomic.Int64
}

func (s *server) join(c *client) {
	s.mu.Lock()
	members := s.rooms[c.room]
	if members == nil {
		members = make(map[*client]struct{})
		s.rooms[c.room] = members
	}
	members[c] = struct{}{}
	s.mu.Unlock()
}

func (s *server) leave(c *client) {
	s.mu.Lock()
	if members := s.rooms[c.room]; members != nil {
		delete(members, c)
		if len(members) == 0 {
			delete(s.rooms, c.room)
		}
	}
	s.mu.Unlock()
}

func (s *server) broadcast(room string, msg []byte) {
	s.mu.RLock()
	for c := range s.rooms[room] {
		c.enqueue(msg) // never blocks, so holding the read lock is fine
	}
	s.mu.RUnlock()
}

func (s *server) loadHistory(room string) ([]historyItem, error) {
	rows, err := s.history.Query(room, historySize)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	items := make([]historyItem, 0, historySize) // [] rather than null when empty
	for rows.Next() {
		var it historyItem
		if err := rows.Scan(&it.ID, &it.User, &it.Text, &it.TS); err != nil {
			return nil, err
		}
		items = append(items, it)
	}
	return items, rows.Err()
}

// authenticate returns the user named by a valid token, or "" if the token is missing or its
// signature is wrong.
func authenticate(token string) string {
	user, sig, ok := strings.Cut(token, ".")
	if !ok {
		return ""
	}
	got, err := hex.DecodeString(sig)
	if err != nil || strings.ToLower(sig) != sig {
		return ""
	}
	mac := hmac.New(sha256.New, []byte(secret))
	mac.Write([]byte(user))
	if !hmac.Equal(got, mac.Sum(nil)) {
		return ""
	}
	return user
}

func (s *server) handleStats(w http.ResponseWriter, r *http.Request) {
	s.mu.RLock()
	rooms := len(s.rooms)
	s.mu.RUnlock()
	body := encode(statsMsg{Connections: s.conns.Load(), Rooms: rooms, Messages: s.messages.Load()})
	w.Header().Set("Content-Type", "application/json")
	w.Write(body)
}

func (s *server) handleWS(w http.ResponseWriter, r *http.Request) {
	q := r.URL.Query()
	user := authenticate(q.Get("token"))
	if user == "" {
		http.Error(w, "unauthorized", http.StatusUnauthorized)
		return
	}
	room := q.Get("room")
	if !nameRE.MatchString(user) || !nameRE.MatchString(room) {
		http.Error(w, "bad request", http.StatusBadRequest)
		return
	}

	conn, err := s.upgrader.Upgrade(w, r, nil)
	if err != nil {
		return // the upgrader has already replied
	}
	conn.SetReadLimit(maxFrame)
	c := &client{
		conn: conn,
		user: user,
		room: room,
		send: make(chan []byte, sendQueue),
		done: make(chan struct{}),
	}

	history, err := s.loadHistory(room)
	if err != nil {
		log.Printf("history %s: %v", room, err)
		conn.Close()
		return
	}
	// The welcome is queued before the client joins, so it is always the first frame sent.
	c.send <- encode(welcomeMsg{Type: "welcome", User: user, Room: room, History: history})
	s.conns.Add(1)
	s.join(c)
	go c.writer()

	defer func() {
		s.leave(c)
		s.conns.Add(-1)
		c.shutdown()
	}()
	s.read(c)
}

func (s *server) read(c *client) {
	for {
		kind, data, err := c.conn.ReadMessage()
		if err != nil {
			return
		}
		var m inbound
		if kind != websocket.TextMessage || json.Unmarshal(data, &m) != nil {
			c.enqueue(badMessage)
			continue
		}
		switch m.Type {
		case "say":
			if m.Text == nil {
				c.enqueue(badMessage)
				continue
			}
			text := *m.Text
			if n := utf8.RuneCountInString(text); n < 1 || n > maxText {
				c.enqueue(badMessage)
				continue
			}
			ts := time.Now().UnixMilli()
			res, err := s.insert.Exec(c.room, c.user, text, ts)
			if err != nil {
				log.Printf("insert: %v", err)
				return
			}
			id, err := res.LastInsertId()
			if err != nil {
				log.Printf("insert id: %v", err)
				return
			}
			s.messages.Add(1)
			s.broadcast(c.room, encode(chatMsg{
				Type: "message", ID: id, Room: c.room, User: c.user, Text: text, TS: ts,
			}))
		case "ping":
			// Unmarshal has already checked that t is valid JSON; a JSON number is the only
			// value that starts with '-' or a digit. RawMessage echoes it digit for digit.
			if len(m.T) == 0 || !(m.T[0] == '-' || (m.T[0] >= '0' && m.T[0] <= '9')) {
				c.enqueue(badMessage)
				continue
			}
			c.enqueue(encode(pongMsg{Type: "pong", T: m.T}))
		default:
			c.enqueue(badMessage)
		}
	}
}

func openDB(path string) (*sql.DB, error) {
	for _, p := range []string{path, path + "-wal", path + "-shm"} {
		if err := os.Remove(p); err != nil && !errors.Is(err, os.ErrNotExist) {
			return nil, err
		}
	}
	db, err := sql.Open("sqlite3", path)
	if err != nil {
		return nil, err
	}
	// SQLite allows one writer at a time, and the spec asks for one connection: every
	// statement (inserts and history reads) goes through this single connection, so the pool
	// serializes access instead of SQLite returning SQLITE_BUSY.
	db.SetMaxOpenConns(1)
	db.SetMaxIdleConns(1)
	db.SetConnMaxLifetime(0)
	for _, stmt := range []string{
		`PRAGMA journal_mode=WAL`,
		`PRAGMA synchronous=NORMAL`,
		`CREATE TABLE messages(id INTEGER PRIMARY KEY, room TEXT NOT NULL, user TEXT NOT NULL, text TEXT NOT NULL, ts INTEGER NOT NULL)`,
		`CREATE INDEX messages_room_id ON messages(room, id)`,
	} {
		if _, err := db.Exec(stmt); err != nil {
			db.Close()
			return nil, err
		}
	}
	return db, nil
}

func getenv(key, def string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return def
}

func main() {
	port := getenv("PORT", "8080")
	if len(os.Args) > 1 {
		port = os.Args[1]
	}
	host := getenv("HOST", "127.0.0.1")

	db, err := openDB(getenv("DB", "chat.db"))
	if err != nil {
		log.Fatalf("db: %v", err)
	}
	insert, err := db.Prepare(`INSERT INTO messages(room, user, text, ts) VALUES (?, ?, ?, ?)`)
	if err != nil {
		log.Fatalf("prepare insert: %v", err)
	}
	history, err := db.Prepare(`SELECT id, user, text, ts FROM (SELECT id, user, text, ts FROM messages WHERE room = ? ORDER BY id DESC LIMIT ?) ORDER BY id`)
	if err != nil {
		log.Fatalf("prepare history: %v", err)
	}

	s := &server{
		db:      db,
		insert:  insert,
		history: history,
		rooms:   make(map[string]map[*client]struct{}),
		upgrader: websocket.Upgrader{
			ReadBufferSize:    1024,
			WriteBufferSize:   1024,
			WriteBufferPool:   &sync.Pool{},
			EnableCompression: false,
			// The benchmark's clients are not browsers; accept any origin.
			CheckOrigin: func(*http.Request) bool { return true },
		},
	}

	mux := http.NewServeMux()
	mux.HandleFunc("GET /stats", s.handleStats)
	mux.HandleFunc("GET /ws", s.handleWS)

	addr := net.JoinHostPort(host, port)
	// HTTPS/WSS when TLS_CERT and TLS_KEY name PEM files, else plain HTTP/WS.
	if cert, key := os.Getenv("TLS_CERT"), os.Getenv("TLS_KEY"); cert != "" && key != "" {
		log.Printf("chat-go listening on https://%s", addr)
		log.Fatal(http.ListenAndServeTLS(addr, cert, key, mux))
	}
	log.Printf("chat-go listening on http://%s", addr)
	log.Fatal(http.ListenAndServe(addr, mux))
}
