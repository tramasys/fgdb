#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

// Build with make -C examples allocator-glibc, allocator-musl,
// allocator-jemalloc, allocator-tcmalloc, or allocator-mimalloc.
// Break at c_allocator_checkpoint to inspect each stage in Misc / Allocator.
// Private allocator state requires matching debug symbols for that library.

#if (defined(FGDB_ALLOCATOR_GLIBC) + defined(FGDB_ALLOCATOR_MUSL) \
    + defined(FGDB_ALLOCATOR_JEMALLOC) + defined(FGDB_ALLOCATOR_TCMALLOC) \
    + defined(FGDB_ALLOCATOR_MIMALLOC)) != 1
#error "Select exactly one allocator through the examples Makefile"
#endif

#if defined(FGDB_ALLOCATOR_JEMALLOC)
#define JEMALLOC_NO_DEMANGLE
#include <jemalloc/jemalloc.h>
#define ALLOCATOR_NAME "jemalloc"
#define heap_malloc je_malloc
#define heap_calloc je_calloc
#define heap_realloc je_realloc
#define heap_free je_free
#elif defined(FGDB_ALLOCATOR_TCMALLOC)
#include <gperftools/tcmalloc.h>
#define ALLOCATOR_NAME "gperftools tcmalloc"
#define heap_malloc tc_malloc
#define heap_calloc tc_calloc
#define heap_realloc tc_realloc
#define heap_free tc_free
#elif defined(FGDB_ALLOCATOR_MIMALLOC)
#include <mimalloc.h>
#define ALLOCATOR_NAME "mimalloc"
#define heap_malloc mi_malloc
#define heap_calloc mi_calloc
#define heap_realloc mi_realloc
#define heap_free mi_free
#else
#if defined(FGDB_ALLOCATOR_GLIBC)
#ifndef __GLIBC__
#error "The glibc fixture needs a compiler targeting glibc"
#endif
#define ALLOCATOR_NAME "glibc"
#else
#ifdef __GLIBC__
#error "The musl fixture needs a musl compiler. Set MUSL_CC to musl-gcc or musl-clang"
#endif
#define ALLOCATOR_NAME "musl"
#endif
#define heap_malloc malloc
#define heap_calloc calloc
#define heap_realloc realloc
#define heap_free free
#endif

#if defined(__GNUC__)
#define FGDB_NOINLINE __attribute__((noinline))
#else
#define FGDB_NOINLINE
#endif

enum { BLOCK_COUNT = 48 };

struct HeapFixture {
    const char *allocator;
    unsigned char *blocks[BLOCK_COUNT];
    size_t block_sizes[BLOCK_COUNT];
    size_t live_blocks;
    uint64_t *numbers;
    size_t number_count;
    unsigned char *large;
    size_t large_size;
};

static void *require_allocation(void *pointer) {
    if (pointer == NULL) {
        fprintf(stderr, "%s allocation failed\n", ALLOCATOR_NAME);
        exit(EXIT_FAILURE);
    }

    return pointer;
}

FGDB_NOINLINE void c_allocator_checkpoint(
    const char *stage,
    const struct HeapFixture *fixture
) {
    printf(
        "%s / %s: live blocks=%zu numbers=%p/%zu large=%p/%zu\n",
        fixture->allocator,
        stage,
        fixture->live_blocks,
        (void *)fixture->numbers,
        fixture->number_count,
        (void *)fixture->large,
        fixture->large_size
    );
}

int main(void) {
    // Initialize stdout before constructing the heap snapshot workload.
    puts(ALLOCATOR_NAME " fixture - break at c_allocator_checkpoint");

    const size_t sizes[] = {32, 96, 512, 4096};

    struct HeapFixture fixture = {
        .allocator = ALLOCATOR_NAME,
        .number_count = 32,
        .large_size = 4 * 1024 * 1024,
    };

    for (size_t index = 0; index < BLOCK_COUNT; ++index) {
        fixture.block_sizes[index] = sizes[index % (sizeof(sizes) / sizeof(sizes[0]))];
        fixture.blocks[index] = require_allocation(heap_malloc(fixture.block_sizes[index]));
        memset(fixture.blocks[index], (int)(0x40 + index), fixture.block_sizes[index]);
        ++fixture.live_blocks;
    }

    fixture.numbers = require_allocation(heap_calloc(fixture.number_count, sizeof(*fixture.numbers)));
    fixture.large = require_allocation(heap_malloc(fixture.large_size));
    memset(fixture.large, 0x4c, fixture.large_size);
    c_allocator_checkpoint("allocated", &fixture);

    // Leave alternating live blocks around the freed slots. More than seven
    // frees per size class also exercise glibc paths beyond a small tcache.
    for (size_t index = 0; index < BLOCK_COUNT; index += 2) {
        heap_free(fixture.blocks[index]);
        fixture.blocks[index] = NULL;
        --fixture.live_blocks;
    }

    heap_free(fixture.large);
    fixture.large = NULL;
    fixture.large_size = 0;
    c_allocator_checkpoint("partly-freed", &fixture);

    const size_t grown_count = 4096;
    uint64_t *grown = heap_realloc(fixture.numbers, grown_count * sizeof(*fixture.numbers));
    fixture.numbers = require_allocation(grown);
    fixture.number_count = grown_count;

    for (size_t index = 0; index < fixture.number_count; ++index) {
        fixture.numbers[index] = UINT64_C(0x100000000) + index;
    }

    for (size_t index = 0; index < BLOCK_COUNT; index += 2) {
        fixture.blocks[index] = require_allocation(heap_malloc(fixture.block_sizes[index]));
        memset(fixture.blocks[index], 0x72, fixture.block_sizes[index]);
        ++fixture.live_blocks;
    }

    c_allocator_checkpoint("reused-and-resized", &fixture);

    for (size_t index = 0; index < BLOCK_COUNT; ++index) {
        heap_free(fixture.blocks[index]);
        fixture.blocks[index] = NULL;
    }

    fixture.live_blocks = 0;
    heap_free(fixture.numbers);
    fixture.numbers = NULL;
    fixture.number_count = 0;
    c_allocator_checkpoint("released", &fixture);
    return EXIT_SUCCESS;
}
