// A probe whose second thread (with $CT_THREAD set), started just before the secret is read,
// works for a while and then waits: what scripts/ct-shim.c counts must not include that work
// (crates/veltc/tests/ct_counts.rs, `a_thread_started_before_the_secret_is_not_counted`).
#include <fcntl.h>
#include <pthread.h>
#include <stdlib.h>
#include <unistd.h>

static volatile unsigned long sink;

static void *work(void *arg) {
    (void)arg;
    for (unsigned long i = 0; i < 2000000; i++) sink += i;
    for (;;) pause();
    return NULL;
}

int main(int argc, char **argv) {
    if (argc < 3) return 2;
    pthread_t t;
    if (getenv("CT_THREAD") && pthread_create(&t, NULL, work, NULL) != 0) return 3;
    unsigned char secret[32] = {0};
    int fd = open(argv[1], O_RDONLY);
    if (fd < 0 || read(fd, secret, sizeof secret) < 0) return 4;
    close(fd);
    for (unsigned long i = 0; i < 2000000; i++) sink += secret[i & 31];
    fd = open(argv[2], O_RDONLY);
    if (fd < 0) return 5;
    char stop;
    (void)!read(fd, &stop, 1);
    close(fd);
    _exit(0);
}
