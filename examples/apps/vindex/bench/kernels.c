// The kernels of kernels.velt as plain C loops, which the compiler vectorizes for the CPU it
// runs on:
//
//   cc -O3 -march=native -ffast-math bench/kernels.c -o target/kernels_c && target/kernels_c
//   cc -O3 -march=x86-64-v2 -ffast-math ...     (128-bit, what Velt's default build may use)
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

enum { ROWS = 8192, DIM = 128, PASSES = 200 };

static double now(void) {
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return t.tv_sec * 1e3 + t.tv_nsec / 1e6;
}

__attribute__((noinline)) static float dot_f32(const float *a, const float *b) {
  float s = 0;
  for (int i = 0; i < DIM; i++) s += a[i] * b[i];
  return s;
}

__attribute__((noinline)) static int32_t dot_i8(const int8_t *a, const int8_t *b) {
  int32_t s = 0;
  for (int i = 0; i < DIM; i++) s += (int32_t)a[i] * b[i];
  return s;
}

int main(void) {
  float *rows = malloc(sizeof(float) * ROWS * DIM), q[DIM];
  int8_t *codes = malloc(ROWS * DIM), qc[DIM];
  uint64_t x = 88172645463325252ull;
  for (int i = 0; i < ROWS * DIM + DIM; i++) {
    x ^= x << 13; x ^= x >> 7; x ^= x << 17;
    float v = (float)(x >> 40) / 8388608.0f - 1;
    if (i < ROWS * DIM) { rows[i] = v; codes[i] = (int8_t)(v * 127); }
    else { q[i - ROWS * DIM] = v; qc[i - ROWS * DIM] = (int8_t)(v * 127); }
  }
  double best = 1e30, sum = 0;
  for (int round = 0; round < 5; round++) {
    double t = now();
    sum = 0;
    for (int p = 0; p < PASSES; p++)
      for (int r = 0; r < ROWS; r++) sum += dot_f32(rows + r * DIM, q);
    if (now() - t < best) best = now() - t;
  }
  printf("f32 %.2f ns/row (%.1f)\n", best * 1e6 / (PASSES * (double)ROWS), sum);
  best = 1e30;
  long long isum = 0;
  for (int round = 0; round < 5; round++) {
    double t = now();
    isum = 0;
    for (int p = 0; p < PASSES; p++)
      for (int r = 0; r < ROWS; r++) isum += dot_i8(codes + r * DIM, qc);
    if (now() - t < best) best = now() - t;
  }
  printf("int8 %.2f ns/row (%lld)\n", best * 1e6 / (PASSES * (double)ROWS), isum);
  return 0;
}
