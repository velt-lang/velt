// half_arith.velt's kernels in C, a check on the Rust numbers: axpy, act, dot with FP16's own
// instructions, and axpy_wide, act_wide, dot_wide widened to float32x4_t, FMA in f32, narrowed.
//   clang -O2 -mcpu=apple-m1 -ffp-contract=off half_arith.c -o half_arith && ./half_arith axpy 100
#include <arm_neon.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#define N 1048576
typedef uint64_t kernel(const float16_t *, float16_t *, float16_t *);
#define SPLAT(b) vreinterpretq_f16_u16(vdupq_n_u16(b))
#define LO(v) vcvt_f32_f16(vget_low_f16(v))
#define HI(v) vcvt_high_f32_f16(v)
#define BACK(l, h) vcvt_high_f16_f32(vcvt_f16_f32(l), h)
static uint64_t bits(float16x8_t s) {
  uint16_t b[8]; uint64_t sum = 0; vst1q_u16(b, vreinterpretq_u16_f16(s));
  for (int k = 0; k < 8; k++) sum += b[k];
  return sum;
}
__attribute__((noinline)) static uint64_t axpy(const float16_t *x, float16_t *y, float16_t *z) {
  float16x8_t a = SPLAT(0x1400);
  for (int i = 0; i + 8 <= N; i += 8) vst1q_f16(y + i, vfmaq_f16(vld1q_f16(y + i), a, vld1q_f16(x + i)));
  return 0;
}
__attribute__((noinline)) static uint64_t act(const float16_t *x, float16_t *y, float16_t *z) {
  float16x8_t s = SPLAT(0x3e00), b = SPLAT(0x3000), o = SPLAT(0);
  for (int i = 0; i + 8 <= N; i += 8) vst1q_f16(z + i, vmaxq_f16(vfmaq_f16(b, vld1q_f16(x + i), s), o));
  return 0;
}
__attribute__((noinline)) static uint64_t dot(const float16_t *x, float16_t *y, float16_t *z) {
  float16x8_t s0 = SPLAT(0), s1 = s0, s2 = s0, s3 = s0;
  for (int i = 0; i + 32 <= N; i += 32) {
    s0 = vfmaq_f16(s0, vld1q_f16(x + i), vld1q_f16(y + i));
    s1 = vfmaq_f16(s1, vld1q_f16(x + i + 8), vld1q_f16(y + i + 8));
    s2 = vfmaq_f16(s2, vld1q_f16(x + i + 16), vld1q_f16(y + i + 16));
    s3 = vfmaq_f16(s3, vld1q_f16(x + i + 24), vld1q_f16(y + i + 24));
  }
  return bits(vaddq_f16(vaddq_f16(s0, s1), vaddq_f16(s2, s3)));
}
static inline float16x8_t fma_wide(float16x8_t c, float16x8_t a, float16x8_t b) {
  return BACK(vfmaq_f32(LO(c), LO(a), LO(b)), vfmaq_f32(HI(c), HI(a), HI(b)));
}
__attribute__((noinline)) static uint64_t axpy_wide(const float16_t *x, float16_t *y, float16_t *z) {
  float16x8_t a = SPLAT(0x1400);
  for (int i = 0; i + 8 <= N; i += 8) vst1q_f16(y + i, fma_wide(vld1q_f16(y + i), a, vld1q_f16(x + i)));
  return 0;
}
__attribute__((noinline)) static uint64_t act_wide(const float16_t *x, float16_t *y, float16_t *z) {
  float16x8_t s = SPLAT(0x3e00), b = SPLAT(0x3000);
  float32x4_t o = vdupq_n_f32(0);
  for (int i = 0; i + 8 <= N; i += 8) {
    float16x8_t v = vld1q_f16(x + i);
    vst1q_f16(z + i, BACK(vmaxq_f32(vfmaq_f32(LO(b), LO(v), LO(s)), o), vmaxq_f32(vfmaq_f32(HI(b), HI(v), HI(s)), o)));
  }
  return 0;
}
__attribute__((noinline)) static uint64_t dot_wide(const float16_t *x, float16_t *y, float16_t *z) {
  float16x8_t s0 = SPLAT(0), s1 = s0, s2 = s0, s3 = s0;
  for (int i = 0; i + 32 <= N; i += 32) {
    s0 = fma_wide(s0, vld1q_f16(x + i), vld1q_f16(y + i));
    s1 = fma_wide(s1, vld1q_f16(x + i + 8), vld1q_f16(y + i + 8));
    s2 = fma_wide(s2, vld1q_f16(x + i + 16), vld1q_f16(y + i + 16));
    s3 = fma_wide(s3, vld1q_f16(x + i + 24), vld1q_f16(y + i + 24));
  }
  return bits(vaddq_f16(vaddq_f16(s0, s1), vaddq_f16(s2, s3)));
}
int main(int argc, char **argv) {
  static uint16_t x[N], y[N], z[N];
  for (int i = 0; i < N; i++) {
    x[i] = (0x3000 + (i % 97) * 31) | (i % 5 == 0 ? 0x8000 : 0);
    y[i] = 0x2c00 + (i % 89) * 37;
  }
  const char *names[] = {"axpy", "act", "dot", "axpy_wide", "act_wide", "dot_wide"};
  kernel *const kernels[] = {axpy, act, dot, axpy_wide, act_wide, dot_wide};
  kernel *volatile k = 0;
  for (int i = 0; i < 6; i++) if (!strcmp(argv[1], names[i])) k = kernels[i];
  uint64_t sum = 0;
  for (int p = atoi(argv[2]); p > 0; p--) sum += k((float16_t *)x, (float16_t *)y, (float16_t *)z);
  for (int i = 0; i < N; i++) sum += (uint64_t)y[i] + z[i];
  printf("%llu\n", (unsigned long long)sum);
}
