#!/usr/bin/env python3
"""Helpers for bench/web/run.sh (standard library only, Python 3.8+).

  measure.py verify <base-url> [db-url]   checks every route's response; exit 1 on a mismatch
  measure.py rss <pid>                    samples the RSS of <pid> and its descendants every
                                          100 ms until SIGTERM/SIGINT, then prints the peak in MB
  measure.py wrk                          wrk --latency output on stdin -> one line
                                          "<req/s> <avg ms> <p99 ms> <errors> <non-2xx>"
"""

import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
import urllib.request

FORTUNES = [
    (1, "fortune: No such file or directory"),
    (2, "A computer scientist is someone who fixes things that aren't broken."),
    (3, "After enough decimal places, nobody gives a damn."),
    (4, "A bad random number generator: 1, 1, 1, 1, 1, 4.33e+67, 1, 1, 1"),
    (5, "A computer program does what you tell it to do, not what you want it to do."),
    (6, "Emacs is a nice operating system, but I prefer UNIX. — Tom Christaensen"),
    (7, "Any program that runs right is obsolete."),
    (8, "A list is only as strong as its weakest link. — Donald Knuth"),
    (9, "Feature: A bug with seniority."),
    (10, "Computers make very fast, very accurate mistakes."),
    (11, '<script>alert("This should not be displayed in a browser alert box.");</script>'),
    (12, "フレームワークのベンチマーク"),
    (0, "Additional fortune added at request time."),
]


def escape_html(s):
    for a, b in (("&", "&amp;"), ("<", "&lt;"), (">", "&gt;"), ('"', "&quot;"), ("'", "&#39;")):
        s = s.replace(a, b)
    return s


def expected_fortunes():
    # Sorted by message as code points (UTF-8 byte order is the same), like every server.
    rows = "".join(
        "<tr><td>%d</td><td>%s</td></tr>" % (i, escape_html(m))
        for i, m in sorted(FORTUNES, key=lambda f: f[1])
    )
    return (
        "<!DOCTYPE html><html><head><title>Fortunes</title></head><body><table>"
        "<tr><th>id</th><th>message</th></tr>" + rows + "</table></body></html>"
    )


class Checker:
    def __init__(self, base):
        self.base = base.rstrip("/")
        self.failures = []

    def get(self, path):
        with urllib.request.urlopen(self.base + path, timeout=10) as res:
            return res.status, res.headers, res.read().decode("utf-8")

    def check(self, cond, what):
        if not cond:
            self.failures.append(what)
        return cond

    def headers(self, path, headers, content_type):
        got = headers.get("content-type", "")
        self.check(got.replace(" ", "").lower() == content_type.replace(" ", "").lower(),
                   "%s: content-type %r, want %r" % (path, got, content_type))
        self.check(headers.get("server"), "%s: no Server header" % path)
        self.check(headers.get("date"), "%s: no Date header" % path)

    def world(self, path, w):
        ok = isinstance(w, dict) and sorted(w.keys()) == ["id", "randomNumber"]
        ok = ok and all(isinstance(w[k], int) and 1 <= w[k] <= 10000 for k in w)
        self.check(ok, "%s: bad world %r" % (path, w))

    def simple(self):
        status, h, body = self.get("/plaintext")
        self.headers("/plaintext", h, "text/plain; charset=utf-8")
        self.check(status == 200 and body == "Hello, World!", "/plaintext: body %r" % body)
        status, h, body = self.get("/json")
        self.headers("/json", h, "application/json")
        self.check(json.loads(body) == {"message": "Hello, World!"}, "/json: body %r" % body)

    def queries(self, route):
        _, h, body = self.get("/db")
        self.headers("/db", h, "application/json")
        self.world("/db", json.loads(body))
        cases = [("", 1), ("?queries=", 1), ("?queries=foo", 1), ("?queries=0", 1),
                 ("?queries=1", 1), ("?queries=20", 20), ("?queries=501", 500)]
        for query, want in cases:
            path = route + query
            _, h, body = self.get(path)
            self.headers(path, h, "application/json")
            rows = json.loads(body)
            if self.check(isinstance(rows, list) and len(rows) == want,
                          "%s: want %d rows, got %.80r" % (path, want, body)):
                for w in rows:
                    self.world(path, w)
        return rows

    def fortunes(self):
        _, h, body = self.get("/fortunes")
        self.headers("/fortunes", h, "text/html; charset=utf-8")
        self.check(body == expected_fortunes(), "/fortunes: HTML differs:\n%s" % body)

    def persisted(self, rows, db_url):
        """The ids that appear once in an /updates response hold the returned numbers."""
        counts = {}
        for w in rows:
            counts[w["id"]] = counts.get(w["id"], 0) + 1
        unique = [w for w in rows if counts[w["id"]] == 1]
        ids = ",".join(str(w["id"]) for w in unique)
        sql = "SELECT id, randomnumber FROM world WHERE id IN (%s)" % ids
        stored = run_sql(db_url, sql)
        if stored is None:
            print("  (updates persistence not checked: no psql and no db container)",
                  file=sys.stderr)
            return
        got = {}
        for line in stored.split():
            i, n = line.split("|")
            got[int(i)] = int(n)
        for w in unique:
            self.check(got.get(w["id"]) == w["randomNumber"],
                       "/updates: id %d returned %d, stored %s"
                       % (w["id"], w["randomNumber"], got.get(w["id"])))


def run_sql(db_url, sql):
    if shutil.which("psql") and db_url:
        cmd = ["psql", db_url, "-tAc", sql]
    elif shutil.which("docker"):
        name = os.environ.get("PG_CONTAINER", "velt-web-pg")
        cmd = ["docker", "exec", name, "psql", "-U", "benchmarkdbuser", "-d", "hello_world",
               "-tAc", sql]
    else:
        return None
    try:
        return subprocess.run(cmd, check=True, capture_output=True, text=True).stdout
    except (OSError, subprocess.CalledProcessError):
        return None


def verify(base, db_url):
    c = Checker(base)
    try:
        c.simple()
        c.queries("/queries")
        c.fortunes()
        c.queries("/updates")
        _, _, body = c.get("/updates?queries=20")
        c.persisted(json.loads(body), db_url)
    except Exception as e:  # any transport or decode failure is a failed check
        c.failures.append("%s: %s" % (type(e).__name__, e))
    for f in c.failures:
        print("  FAIL " + f, file=sys.stderr)
    return 0 if not c.failures else 1


def process_table():
    """{pid: (ppid, rss_kb)} from ps, or from /proc where ps is missing (slim containers)."""
    table = {}
    if shutil.which("ps"):
        out = subprocess.run(["ps", "-A", "-o", "pid=,ppid=,rss="], capture_output=True,
                             text=True).stdout
        for line in out.splitlines():
            parts = line.split()
            if len(parts) == 3:
                table[int(parts[0])] = (int(parts[1]), int(parts[2]))
        return table
    page_kb = os.sysconf("SC_PAGE_SIZE") // 1024
    for entry in os.listdir("/proc"):
        if entry.isdigit():
            try:
                with open("/proc/%s/stat" % entry) as f:
                    fields = f.read().rsplit(")", 1)[1].split()
                table[int(entry)] = (int(fields[1]), int(fields[21]) * page_kb)
            except (OSError, IndexError, ValueError):
                pass
    return table


def tree_rss_kb(root):
    table = process_table()
    total, todo = 0, [root]
    while todo:
        pid = todo.pop()
        if pid in table:
            total += table[pid][1]
            todo.extend(p for p, (ppid, _) in table.items() if ppid == pid)
    return total


def rss(pid):
    peak = [0]

    def stop(*_):
        print("%.1f" % (peak[0] / 1024.0))
        sys.exit(0)

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    while True:
        peak[0] = max(peak[0], tree_rss_kb(pid))
        time.sleep(0.1)


def to_ms(value):
    m = re.match(r"([\d.]+)(us|ms|s|m)$", value)
    if not m:
        return float("nan")
    scale = {"us": 0.001, "ms": 1.0, "s": 1000.0, "m": 60000.0}[m.group(2)]
    return float(m.group(1)) * scale


def parse_wrk(text):
    rps = re.search(r"Requests/sec:\s+([\d.]+)", text)
    avg = re.search(r"Latency\s+(\S+)", text)
    p99 = re.search(r"^\s+99%\s+(\S+)", text, re.M)
    errors = 0
    sock = re.search(r"Socket errors: connect (\d+), read (\d+), write (\d+), timeout (\d+)", text)
    if sock:
        errors = sum(int(x) for x in sock.groups())
    non2xx = re.search(r"Non-2xx or 3xx responses: (\d+)", text)
    print("%.0f %.2f %.2f %d %d" % (
        float(rps.group(1)) if rps else 0.0,
        to_ms(avg.group(1)) if avg else float("nan"),
        to_ms(p99.group(1)) if p99 else float("nan"),
        errors,
        int(non2xx.group(1)) if non2xx else 0,
    ))


def main():
    if len(sys.argv) >= 3 and sys.argv[1] == "verify":
        return verify(sys.argv[2], sys.argv[3] if len(sys.argv) > 3 else "")
    if len(sys.argv) == 3 and sys.argv[1] == "rss":
        return rss(int(sys.argv[2]))
    if len(sys.argv) == 2 and sys.argv[1] == "wrk":
        return parse_wrk(sys.stdin.read())
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
