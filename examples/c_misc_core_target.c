#define _GNU_SOURCE

#include <inttypes.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#if defined(__GNUC__)
#define FGDB_NOINLINE __attribute__((noinline))
#else
#define FGDB_NOINLINE
#endif

struct CoreFixture {
    uint64_t marker;
    char *heap_message;
    unsigned char *mapping;
    size_t mapping_size;
};

FGDB_NOINLINE void c_misc_core_checkpoint(const struct CoreFixture *fixture) {
    printf(
        "core checkpoint: marker=%#" PRIx64 " heap=%p mapping=%p/%zu\n",
        fixture->marker,
        (void *)fixture->heap_message,
        (void *)fixture->mapping,
        fixture->mapping_size
    );
}

FGDB_NOINLINE static void core_crash_leaf(const struct CoreFixture *fixture) {
    fprintf(stderr, "intentional SIGSEGV after checkpoint: %s\n", fixture->heap_message);
    raise(SIGSEGV);
}

FGDB_NOINLINE static void core_crash_middle(const struct CoreFixture *fixture) {
    core_crash_leaf(fixture);
}

FGDB_NOINLINE static void core_crash_outer(const struct CoreFixture *fixture) {
    core_crash_middle(fixture);
}

int main(void) {
    const long page_size = sysconf(_SC_PAGESIZE);
    if (page_size <= 0) {
        return EXIT_FAILURE;
    }
    struct CoreFixture fixture = {
        .marker = UINT64_C(0xc0def00ddeadbeef),
        .heap_message = malloc(256),
        .mapping_size = (size_t)page_size * 8,
    };
    fixture.mapping = mmap(
        NULL,
        fixture.mapping_size,
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANONYMOUS,
        -1,
        0
    );
    if (fixture.heap_message == NULL || fixture.mapping == MAP_FAILED) {
        return EXIT_FAILURE;
    }
    snprintf(
        fixture.heap_message,
        256,
        "core fixture owned by pid %ld",
        (long)getpid()
    );
    memset(fixture.mapping, 0x43, fixture.mapping_size);
    c_misc_core_checkpoint(&fixture);
    core_crash_outer(&fixture);
    return EXIT_FAILURE;
}
