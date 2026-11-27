// A probe whose second thread (with $CT_THREAD set), started just before the secret is read,
// works for a while and then waits: what scripts/ct-shim.c counts must not include that work
// (crates/veltc/tests/ct_counts.rs, `a_thread_started_before_the_secret_is_not_counted`).
// With $CT_THREAD set to `spin` the thread never waits, and the shim must say it never settled.
#include <fcntl.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static volatile unsigned long sink;

static void *work(void *arg) {
    for (unsigned long i = 0; arg || i < 2000000; i++) sink += i;
    for (;;) pause();
    return NULL;
}

int main(int argc, char **argv) {
    // The secret's file, and the stop file: by default the secret's with `.stop` added, as
    // scripts/ct-counts.py names it.
    if (argc < 2 || strlen(argv[1]) > 4000) return 2;
    char stop_path[4100];
    strcpy(stop_path, argv[1]);
    strcat(stop_path, ".stop");
    const char *stop_file = argc > 2 ? argv[2] : stop_path;
    pthread_t t;
    const char *thread = getenv("CT_THREAD");
    void *spin = thread && strcmp(thread, "spin") == 0 ? (void *)1 : NULL;
    if (thread && pthread_create(&t, NULL, work, spin) != 0) return 3;
    unsigned char secret[32] = {0};
    int fd = open(argv[1], O_RDONLY);
    if (fd < 0 || read(fd, secret, sizeof secret) < 0) return 4;
    close(fd);
    for (unsigned long i = 0; i < 2000000; i++) sink += secret[i & 31];
    fd = open(stop_file, O_RDONLY);
    if (fd < 0) return 5;
    char stop;
    (void)!read(fd, &stop, 1);
    close(fd);
    _exit(0);
}
