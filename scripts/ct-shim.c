// Marks what read() returns from the file named by $VELT_CT_SECRET as undefined for memcheck,
// so that every branch and address that depends on it is reported (ctgrind's method). Under
// cachegrind run with --instr-at-start=no, counts from that read to the first read of the file
// named by $VELT_CT_STOP (or to the end): the code under test, without the process's start and
// end, whose threads' work varies from run to run.
#define _GNU_SOURCE
#include <dirent.h>
#include <dlfcn.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>
#include <valgrind/cachegrind.h>
#include <valgrind/memcheck.h>

// Valgrind before 3.22 has no such requests; ct-counts.py then counts whole processes.
#ifndef CACHEGRIND_START_INSTRUMENTATION
#define CACHEGRIND_START_INSTRUMENTATION
#define CACHEGRIND_STOP_INSTRUMENTATION
#endif

// Whether `fd` is the file named by the environment variable `var`.
static int is_file(int fd, const char *var) {
    const char *want = getenv(var);
    if (!want) return 0;
    char link[64], path[PATH_MAX];
    snprintf(link, sizeof link, "/proc/self/fd/%d", fd);
    ssize_t n = readlink(link, path, sizeof path - 1);
    if (n <= 0) return 0;
    path[n] = 0;
    return strcmp(path, want) == 0;
}

// The CPU time the process's other threads have used (ns, /proc/self/task/<tid>/schedstat), and
// in `all_waiting` whether each of them is waiting (state S or D in .../stat); -1 when the
// threads can't be listed.
static long long others(int *all_waiting) {
    *all_waiting = 1;
    DIR *dir = opendir("/proc/self/task");
    if (!dir) return -1;
    long long total = 0;
    pid_t self = (pid_t)syscall(SYS_gettid);
    struct dirent *e;
    while ((e = readdir(dir))) {
        pid_t tid = (pid_t)atoi(e->d_name);
        if (tid <= 0 || tid == self) continue;
        char path[64], text[512];
        snprintf(path, sizeof path, "/proc/self/task/%d/schedstat", tid);
        FILE *f = fopen(path, "r");
        long long ns = 0;
        if (f) {
            if (fscanf(f, "%lld", &ns) != 1) ns = 0;
            fclose(f);
        }
        total += ns;
        snprintf(path, sizeof path, "/proc/self/task/%d/stat", tid);
        f = fopen(path, "r");
        if (f) {
            size_t n = fread(text, 1, sizeof text - 1, f);
            fclose(f);
            text[n] = 0;
            char *end = strrchr(text, ')');
            if (end && end[1] == ' ' && end[2] != 'S' && end[2] != 'D') *all_waiting = 0;
        }
    }
    closedir(dir);
    return total;
}

// Waits, before counting starts, until the process's other threads have stopped running. The
// runtime's host thread (velt_rt's `velt-host`, which parks in epoll_wait) starts just before a
// test probe reads its secret, and otherwise runs the rest of its start inside what is counted:
// more or less of it as valgrind schedules the threads, so that on a busy machine one secret's
// count changes from run to run by thousands of instructions, and ct-counts.py reports it as a
// count that depends on the secret (#3942). Valgrind runs one thread at a time: while this one
// sleeps, the others run until they wait. Settled is three sleeps of a millisecond in a row in
// which no other thread used CPU time and each was waiting; at most two seconds. What it runs
// is not counted (instrumentation is still off).
static void settle(void) {
    int stable = 0;
    long long before = -2;
    for (int i = 0; i < 2000 && stable < 3; i++) {
        struct timespec ms = {0, 1000000};
        nanosleep(&ms, NULL);
        int waiting;
        long long now = others(&waiting);
        stable = now == before && waiting ? stable + 1 : 0;
        before = now;
    }
}

ssize_t read(int fd, void *buf, size_t count) {
    static ssize_t (*real)(int, void *, size_t);
    if (!real) {
        real = dlsym(RTLD_NEXT, "read");
        if (!real) {
            fputs("ct-shim: dlsym(RTLD_NEXT, \"read\") found no read\n", stderr);
            abort();
        }
    }
    ssize_t n = real(fd, buf, count);
    static int started, stopped;
    if (n > 0 && is_file(fd, "VELT_CT_SECRET")) {
        VALGRIND_MAKE_MEM_UNDEFINED(buf, n);
        if (!started) {
            started = 1;
            settle();
            CACHEGRIND_START_INSTRUMENTATION;
        }
    } else if (started && !stopped && is_file(fd, "VELT_CT_STOP")) {
        stopped = 1;
        CACHEGRIND_STOP_INSTRUMENTATION;
    }
    return n;
}
