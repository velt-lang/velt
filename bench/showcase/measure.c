// measure: runs one benchmark process and reports its own wall time, CPU time and peak RSS.
//
//   measure <result file> <program> [args...]
//
// The harness (Python) cannot take peak RSS from its own wait4: Python starts children with
// vfork, and Linux starts a child's ru_maxrss at the high-water RSS of the address space the
// exec replaced, which is the harness's own (hundreds of MB once it has read a large output),
// so every small program reported the harness's size. This helper is a small process: the child
// it forks starts from its address space, so ru_maxrss is the program's own (plus at most this
// helper's ~1 MB). stdin, stdout and stderr pass through. It writes one line to the result file,
// `<wall ns> <maxrss KiB> <user us> <sys us>`, and exits with the program's exit code (128 +
// the signal number when a signal ended it). The child is killed when this process dies
// (PR_SET_PDEATHSIG), so the harness's timeout kill reaches the program.
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static long long now_ns(void) {
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return (long long)t.tv_sec * 1000000000LL + t.tv_nsec;
}

int main(int argc, char **argv) {
  if (argc < 3) {
    fprintf(stderr, "usage: measure <result file> <program> [args...]\n");
    return 2;
  }
  pid_t parent = getpid();
  long long t0 = now_ns();
  pid_t pid = fork();
  if (pid < 0) {
    perror("measure: fork");
    return 126;
  }
  if (pid == 0) {
    prctl(PR_SET_PDEATHSIG, SIGKILL);
    if (getppid() != parent) _exit(126);
    execvp(argv[2], argv + 2);
    fprintf(stderr, "measure: %s: %s\n", argv[2], strerror(errno));
    _exit(127);
  }
  int status;
  struct rusage ru;
  while (wait4(pid, &status, 0, &ru) < 0) {
    if (errno != EINTR) {
      perror("measure: wait4");
      return 126;
    }
  }
  long long wall = now_ns() - t0;
  FILE *f = fopen(argv[1], "w");
  if (!f) {
    perror("measure: result file");
    return 126;
  }
  fprintf(f, "%lld %ld %lld %lld\n", wall, ru.ru_maxrss,
          (long long)ru.ru_utime.tv_sec * 1000000LL + ru.ru_utime.tv_usec,
          (long long)ru.ru_stime.tv_sec * 1000000LL + ru.ru_stime.tv_usec);
  fclose(f);
  if (WIFEXITED(status)) return WEXITSTATUS(status);
  if (WIFSIGNALED(status)) return 128 + WTERMSIG(status);
  return 126;
}
