#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>

/* Break at simd_sse_checkpoint, simd_avx_checkpoint or simd_avx512_checkpoint.
 * Inspect lane widths, signed integers, floats, negative zero and NaN payloads.
 * Edit a lane and continue. The captured words include the edits, with no call
 * between each checkpoint and the register stores. Optional ISAs are gated.
 */

const uint32_t simd_float_bits[4] = {
    UINT32_C(0x3f800000), UINT32_C(0xc0200000),
    UINT32_C(0x80000000), UINT32_C(0x7fc00042)
};

const uint64_t simd_double_bits[2] = {
    UINT64_C(0x7ff0000000000000), UINT64_C(0xc00a000000000000)
};

const uint64_t simd_integer_bits[8] = {
    UINT64_C(0x0706050403020100), UINT64_C(0xfffefdfcfbfaf9f8),
    UINT64_C(0x7fffffffffffffff), UINT64_C(0x8000000000000000),
    UINT64_C(0x1122334455667788), UINT64_C(0x8877665544332211),
    UINT64_C(0x00000000ffffffff), UINT64_C(0xffff0000ffff0000)
};

uint64_t simd_captured[3][8];

#if defined(__x86_64__) || defined(__i386__)

__attribute__((noinline, target("sse2")))
void simd_sse(void)
{
    __asm__ volatile (
        "movdqu %[floats], %%xmm0\n\t"
        "movdqu %[doubles], %%xmm1\n\t"
        "movdqu %[integers], %%xmm2\n\t"
        ".globl simd_sse_checkpoint\n"
        "simd_sse_checkpoint:\n\t"
        "nop\n\t"
        "movdqu %%xmm0, %[out0]\n\t"
        "movdqu %%xmm1, %[out1]\n\t"
        "movdqu %%xmm2, %[out2]\n\t"
        : [out0] "=m" (*(uint64_t (*)[2])simd_captured[0]),
          [out1] "=m" (*(uint64_t (*)[2])simd_captured[1]),
          [out2] "=m" (*(uint64_t (*)[2])simd_captured[2])
        : [floats] "m" (simd_float_bits),
          [doubles] "m" (simd_double_bits),
          [integers] "m" (simd_integer_bits)
        : "xmm0", "xmm1", "xmm2"
    );
}

__attribute__((noinline, target("avx")))
void simd_avx(void)
{
    __asm__ volatile (
        "vmovdqu %[integers], %%ymm0\n\t"
        ".globl simd_avx_checkpoint\n"
        "simd_avx_checkpoint:\n\t"
        "nop\n\t"
        "vmovdqu %%ymm0, %[out]\n\t"
        : [out] "=m" (*(uint64_t (*)[4])simd_captured[0])
        : [integers] "m" (simd_integer_bits)
        : "ymm0"
    );
}

__attribute__((noinline, target("avx512f")))
void simd_avx512(void)
{
    __asm__ volatile (
        "vmovdqu64 %[integers], %%zmm0\n\t"
        ".globl simd_avx512_checkpoint\n"
        "simd_avx512_checkpoint:\n\t"
        "nop\n\t"
        "vmovdqu64 %%zmm0, %[out]\n\t"
        : [out] "=m" (simd_captured[0])
        : [integers] "m" (simd_integer_bits)
        : "zmm0"
    );
}

static void show_capture(const char *phase, unsigned registers, unsigned words)
{
    puts(phase);

    for (unsigned reg = 0; reg < registers; ++reg) {
        printf("  vector %u", reg);

        for (unsigned word = 0; word < words; ++word) {
            printf("  %016" PRIx64, simd_captured[reg][word]);
        }

        putchar('\n');
    }
}

int main(void)
{
    if (__builtin_cpu_supports("sse2")) {
        simd_sse();
        show_capture("SSE2 capture", 3, 2);
    }

    if (__builtin_cpu_supports("avx")) {
        simd_avx();
        show_capture("AVX capture", 1, 4);
    } else {
        puts("AVX is unavailable on this CPU / OS");
    }

    if (__builtin_cpu_supports("avx512f")) {
        simd_avx512();
        show_capture("AVX-512 capture", 1, 8);
    } else {
        puts("AVX-512 is unavailable on this CPU / OS");
    }

    return 0;
}

#else

int main(void)
{
    puts("This fixture exercises x86 SSE2, AVX and AVX-512 registers");
    return 0;
}

#endif
