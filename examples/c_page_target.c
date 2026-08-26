#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
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

enum PageStage {
    PAGE_STAGE_RESERVED,
    PAGE_STAGE_POPULATED,
    PAGE_STAGE_RECLAIMED,
};

struct PageFixture {
    size_t page_size;
    size_t page_count;
    size_t length;
    unsigned char *anonymous;
    unsigned char *shared_file;
    unsigned char *private_file;
    unsigned char *guard_page;
    unsigned char *locked_page;
    int backing_fd;
    int page_locked;
    unsigned char residency[2048];
    enum PageStage stage;
};

static void fail(const char *operation) {
    fprintf(stderr, "%s failed: %s\n", operation, strerror(errno));
    exit(EXIT_FAILURE);
}

static void refresh_residency(struct PageFixture *fixture) {
    memset(fixture->residency, 0, sizeof(fixture->residency));
    if (mincore(fixture->anonymous, fixture->length, fixture->residency) != 0) {
        fail("mincore");
    }
}

static void reset_soft_dirty(void) {
    const int descriptor = open("/proc/self/clear_refs", O_WRONLY | O_CLOEXEC);
    if (descriptor < 0) {
        return;
    }
    (void)write(descriptor, "4", 1);
    close(descriptor);
}

FGDB_NOINLINE void c_page_checkpoint(const char *stage_name, struct PageFixture *fixture) {
    size_t resident = 0;
    for (size_t page = 0; page < fixture->page_count; ++page) {
        resident += (fixture->residency[page] & 1U) != 0;
    }
    printf(
        "page checkpoint %-9s mapping=%p resident=%zu/%zu locked=%d\n",
        stage_name,
        (void *)fixture->anonymous,
        resident,
        fixture->page_count,
        fixture->page_locked
    );
}

int main(void) {
    const long page_size_result = sysconf(_SC_PAGESIZE);
    if (page_size_result <= 0) {
        fail("sysconf");
    }
    struct PageFixture fixture = {
        .page_size = (size_t)page_size_result,
        .page_count = 2048,
        .stage = PAGE_STAGE_RESERVED,
    };
    fixture.length = fixture.page_size * fixture.page_count;

    fixture.anonymous = mmap(
        NULL,
        fixture.length,
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANONYMOUS,
        -1,
        0
    );
    if (fixture.anonymous == MAP_FAILED) {
        fail("anonymous mmap");
    }
    (void)madvise(fixture.anonymous, fixture.length, MADV_HUGEPAGE);

    fixture.backing_fd = memfd_create("fgdb-page-backing", MFD_CLOEXEC);
    if (fixture.backing_fd < 0 || ftruncate(fixture.backing_fd, (off_t)fixture.length) != 0) {
        fail("page backing memfd");
    }
    fixture.shared_file = mmap(
        NULL,
        fixture.length,
        PROT_READ | PROT_WRITE,
        MAP_SHARED,
        fixture.backing_fd,
        0
    );
    fixture.private_file = mmap(
        NULL,
        fixture.length,
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE,
        fixture.backing_fd,
        0
    );
    if (fixture.shared_file == MAP_FAILED || fixture.private_file == MAP_FAILED) {
        fail("file-backed mmap");
    }

    fixture.guard_page = fixture.anonymous + fixture.length - fixture.page_size;
    if (mprotect(fixture.guard_page, fixture.page_size, PROT_NONE) != 0) {
        fail("guard-page mprotect");
    }
    refresh_residency(&fixture);
    c_page_checkpoint("reserved", &fixture);

    reset_soft_dirty();
    for (size_t page = 0; page + 1 < fixture.page_count; page += 2) {
        fixture.anonymous[page * fixture.page_size] = (unsigned char)(page & 0xffU);
    }
    for (size_t page = 0; page < 64; ++page) {
        fixture.shared_file[page * fixture.page_size] = 0x53;
        fixture.private_file[page * fixture.page_size] = 0x50;
    }
    fixture.locked_page = fixture.anonymous;
    fixture.page_locked = mlock(fixture.locked_page, fixture.page_size) == 0;
    fixture.stage = PAGE_STAGE_POPULATED;
    refresh_residency(&fixture);
    c_page_checkpoint("populated", &fixture);

    if (fixture.page_locked) {
        if (munlock(fixture.locked_page, fixture.page_size) != 0) {
            fail("munlock");
        }
        fixture.page_locked = 0;
    }
    const size_t reclaim_offset = fixture.length / 2;
    const size_t reclaim_length = fixture.length / 4;
    if (madvise(fixture.anonymous + reclaim_offset, reclaim_length, MADV_DONTNEED) != 0) {
        fail("MADV_DONTNEED");
    }
    (void)madvise(fixture.private_file, fixture.length, MADV_DONTNEED);
    fixture.stage = PAGE_STAGE_RECLAIMED;
    refresh_residency(&fixture);
    c_page_checkpoint("reclaimed", &fixture);

    munmap(fixture.private_file, fixture.length);
    munmap(fixture.shared_file, fixture.length);
    close(fixture.backing_fd);
    munmap(fixture.anonymous, fixture.length);
    return 0;
}
