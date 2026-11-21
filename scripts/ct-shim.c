// Marks what read() returns from the file named by $VELT_CT_SECRET as undefined for memcheck,
// so that every branch and address that depends on it is reported (ctgrind's method). Under
// cachegrind run with --instr-at-start=no, counts from that read to the first read of the file
// named by $VELT_CT_STOP (or to the end): the code under test, without the process's start and
// end, whose threads' work varies from run to run.
#define _GNU_SOURCE
#include <dlfcn.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
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
            CACHEGRIND_START_INSTRUMENTATION;
        }
    } else if (started && !stopped && is_file(fd, "VELT_CT_STOP")) {
        stopped = 1;
        CACHEGRIND_STOP_INSTRUMENTATION;
    }
    return n;
}
