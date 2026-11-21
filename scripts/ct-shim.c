// Marks what read() returns from the file named by $VELT_CT_SECRET as undefined for memcheck,
// so that every branch and address that depends on it is reported (ctgrind's method).
#define _GNU_SOURCE
#include <dlfcn.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <valgrind/memcheck.h>

static int is_secret(int fd) {
    const char *want = getenv("VELT_CT_SECRET");
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
    if (n > 0 && is_secret(fd)) VALGRIND_MAKE_MEM_UNDEFINED(buf, n);
    return n;
}
