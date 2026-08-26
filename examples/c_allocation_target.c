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

enum { MAPPING_PAGES = 256 };

struct AllocationFixture {
    size_t page_size;
    size_t mapping_size;
    char *small_allocation;
    size_t small_size;
    uint32_t *number_allocation;
    size_t number_count;
    unsigned char *large_allocation;
    size_t large_size;
    unsigned char *mapping;
    unsigned char residency[MAPPING_PAGES];
    size_t resident_pages;
};

static void fail(const char *operation) {
    fprintf(stderr, "%s failed: %s\n", operation, strerror(errno));
    exit(EXIT_FAILURE);
}

static void refresh_residency(struct AllocationFixture *fixture) {
    memset(fixture->residency, 0, sizeof(fixture->residency));
    if (mincore(fixture->mapping, fixture->mapping_size, fixture->residency) != 0) {
        fail("mincore");
    }
    fixture->resident_pages = 0;
    for (size_t page = 0; page < MAPPING_PAGES; ++page) {
        fixture->resident_pages += (fixture->residency[page] & 1U) != 0;
    }
}

FGDB_NOINLINE void c_allocation_checkpoint(
    const char *stage,
    struct AllocationFixture *fixture
) {
    printf(
        "allocation checkpoint %-10s mmap=%p resident=%zu/%d heap=%p/%p/%p\n",
        stage,
        (void *)fixture->mapping,
        fixture->resident_pages,
        MAPPING_PAGES,
        (void *)fixture->small_allocation,
        (void *)fixture->number_allocation,
        (void *)fixture->large_allocation
    );
}

int main(void) {
    const long page_size_result = sysconf(_SC_PAGESIZE);
    if (page_size_result <= 0) {
        fail("sysconf");
    }

    struct AllocationFixture fixture = {
        .page_size = (size_t)page_size_result,
        .small_size = 64,
        .number_count = 16 * 1024,
        .large_size = 512 * 1024,
    };
    fixture.mapping_size = fixture.page_size * MAPPING_PAGES;

    fixture.small_allocation = malloc(fixture.small_size);
    fixture.number_allocation = calloc(fixture.number_count, sizeof(*fixture.number_allocation));
    fixture.large_allocation = malloc(fixture.large_size);
    if (fixture.small_allocation == NULL
        || fixture.number_allocation == NULL
        || fixture.large_allocation == NULL) {
        fail("heap allocation");
    }
    snprintf(
        fixture.small_allocation,
        fixture.small_size,
        "ordinary malloc allocation"
    );

    fixture.mapping = mmap(
        NULL,
        fixture.mapping_size,
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANONYMOUS,
        -1,
        0
    );
    if (fixture.mapping == MAP_FAILED) {
        fail("mmap");
    }
    (void)madvise(fixture.mapping, fixture.mapping_size, MADV_NOHUGEPAGE);

    refresh_residency(&fixture);
    c_allocation_checkpoint("reserved", &fixture);

    memset(fixture.mapping, 0x41, fixture.page_size);
    for (size_t index = 0; index < 32; ++index) {
        fixture.number_allocation[index] = (uint32_t)(index * index);
    }
    memset(fixture.large_allocation, 0x4c, fixture.page_size);
    refresh_residency(&fixture);
    c_allocation_checkpoint("first-page", &fixture);

    for (size_t page = 4; page < MAPPING_PAGES; page += 4) {
        fixture.mapping[page * fixture.page_size] = (unsigned char)page;
    }
    refresh_residency(&fixture);
    c_allocation_checkpoint("sparse", &fixture);

    memset(fixture.mapping, 0x44, fixture.mapping_size);
    for (size_t index = 0; index < fixture.number_count; ++index) {
        fixture.number_allocation[index] = (uint32_t)(index ^ 0x5a5aU);
    }
    memset(fixture.large_allocation, 0x4c, fixture.large_size);
    refresh_residency(&fixture);
    c_allocation_checkpoint("fully-used", &fixture);

    const size_t reclaim_offset = fixture.mapping_size / 4;
    const size_t reclaim_size = fixture.mapping_size / 2;
    if (madvise(fixture.mapping + reclaim_offset, reclaim_size, MADV_DONTNEED) != 0) {
        fail("MADV_DONTNEED");
    }
    refresh_residency(&fixture);
    c_allocation_checkpoint("reclaimed", &fixture);

    munmap(fixture.mapping, fixture.mapping_size);
    free(fixture.large_allocation);
    free(fixture.number_allocation);
    free(fixture.small_allocation);
    return 0;
}
