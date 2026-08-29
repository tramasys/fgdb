#define _GNU_SOURCE

#include <errno.h>
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

struct AllocatorFixture {
    char *small;
    uint64_t *medium;
    unsigned char *large;
    unsigned char *mapping;
    size_t small_size;
    size_t medium_size;
    size_t large_size;
    size_t mapping_size;
};

static void fail(const char *operation) {
    fprintf(stderr, "%s failed: %s\n", operation, strerror(errno));
    exit(EXIT_FAILURE);
}

FGDB_NOINLINE void c_misc_allocator_checkpoint(
    const char *stage,
    const struct AllocatorFixture *fixture
) {
    printf(
        "allocator checkpoint %s: small=%p medium=%p large=%p mmap=%p/%zu\n",
        stage,
        (void *)fixture->small,
        (void *)fixture->medium,
        (void *)fixture->large,
        (void *)fixture->mapping,
        fixture->mapping_size
    );
}

int main(void) {
    const long page_size = sysconf(_SC_PAGESIZE);
    if (page_size <= 0) {
        fail("sysconf");
    }
    struct AllocatorFixture fixture = {
        .small_size = 96,
        .medium_size = 32 * 1024,
        .large_size = 2 * 1024 * 1024,
        .mapping_size = (size_t)page_size * 32,
    };
    fixture.small = malloc(fixture.small_size);
    fixture.medium = calloc(fixture.medium_size / sizeof(*fixture.medium), sizeof(*fixture.medium));
    fixture.large = malloc(fixture.large_size);
    fixture.mapping = mmap(
        NULL,
        fixture.mapping_size,
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANONYMOUS,
        -1,
        0
    );
    if (fixture.small == NULL || fixture.medium == NULL || fixture.large == NULL) {
        fail("malloc/calloc");
    }
    if (fixture.mapping == MAP_FAILED) {
        fail("mmap");
    }

    snprintf(fixture.small, fixture.small_size, "small allocation on the brk heap");
    for (size_t index = 0; index < fixture.medium_size / sizeof(*fixture.medium); ++index) {
        fixture.medium[index] = UINT64_C(0x100000000) + index;
    }
    memset(fixture.large, 0x4c, fixture.large_size);
    for (size_t page = 0; page < 32; page += 2) {
        fixture.mapping[page * (size_t)page_size] = (unsigned char)(0x40 + page);
    }
    c_misc_allocator_checkpoint("allocated", &fixture);

    if (madvise(fixture.mapping, fixture.mapping_size, MADV_DONTNEED) != 0) {
        fail("madvise");
    }
    c_misc_allocator_checkpoint("mapping-reclaimed", &fixture);

    munmap(fixture.mapping, fixture.mapping_size);
    free(fixture.large);
    free(fixture.medium);
    free(fixture.small);
    return EXIT_SUCCESS;
}
