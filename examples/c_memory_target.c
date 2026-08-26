#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <unistd.h>

#if defined(__GNUC__)
#define FGDB_NOINLINE __attribute__((noinline))
#else
#define FGDB_NOINLINE
#endif

volatile uint64_t c_memory_watch = UINT64_C(0x1122334455667788);

union WordView {
    uint64_t integer;
    double floating;
    unsigned char bytes[8];
};

struct PacketFlags {
    unsigned readable : 1;
    unsigned writable : 1;
    unsigned executable : 1;
    unsigned priority : 5;
};

struct MemoryFixture {
    void *anonymous_mapping;
    size_t anonymous_size;
    void *shared_mapping;
    size_t shared_size;
    char *heap_buffer;
    size_t heap_size;
    int memfd;
    int pipe_fds[2];
    int socket_fds[2];
    int event_fd;
    int epoll_fd;
    union WordView word;
    struct PacketFlags flags;
};

static void fail(const char *operation) {
    fprintf(stderr, "%s failed: %s\n", operation, strerror(errno));
    exit(EXIT_FAILURE);
}

FGDB_NOINLINE void c_memory_checkpoint(struct MemoryFixture *fixture) {
    c_memory_watch ^= (uintptr_t)fixture->anonymous_mapping;
    printf(
        "memory checkpoint: anonymous=%p shared=%p heap=%p watch=%#llx\n",
        fixture->anonymous_mapping,
        fixture->shared_mapping,
        (void *)fixture->heap_buffer,
        (unsigned long long)c_memory_watch
    );
}

int main(void) {
    const long page_size_result = sysconf(_SC_PAGESIZE);
    if (page_size_result <= 0) {
        fail("sysconf");
    }
    const size_t page_size = (size_t)page_size_result;
    struct MemoryFixture fixture = {0};

    fixture.anonymous_size = page_size * 16;
    fixture.anonymous_mapping = mmap(
        NULL,
        fixture.anonymous_size,
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANONYMOUS,
        -1,
        0
    );
    if (fixture.anonymous_mapping == MAP_FAILED) {
        fail("mmap anonymous");
    }
    memset(fixture.anonymous_mapping, 0x41, fixture.anonymous_size - page_size);
    if (mprotect(
            (unsigned char *)fixture.anonymous_mapping + fixture.anonymous_size - page_size,
            page_size,
            PROT_NONE
        ) != 0) {
        fail("mprotect guard page");
    }
    (void)madvise(fixture.anonymous_mapping, fixture.anonymous_size - page_size, MADV_HUGEPAGE);

    fixture.memfd = memfd_create("fgdb-shared-fixture", MFD_CLOEXEC);
    if (fixture.memfd < 0) {
        fail("memfd_create");
    }
    fixture.shared_size = page_size * 4;
    if (ftruncate(fixture.memfd, (off_t)fixture.shared_size) != 0) {
        fail("ftruncate");
    }
    fixture.shared_mapping = mmap(
        NULL,
        fixture.shared_size,
        PROT_READ | PROT_WRITE,
        MAP_SHARED,
        fixture.memfd,
        0
    );
    if (fixture.shared_mapping == MAP_FAILED) {
        fail("mmap shared");
    }
    static const char shared_message[] = "shared mapping from a memfd";
    memcpy(fixture.shared_mapping, shared_message, sizeof(shared_message));

    fixture.heap_size = 256 * 1024;
    fixture.heap_buffer = malloc(fixture.heap_size);
    if (fixture.heap_buffer == NULL) {
        fail("malloc");
    }
    memset(fixture.heap_buffer, 0x42, fixture.heap_size);
    static const char heap_message[] = "heap buffer: independently selectable bytes";
    memcpy(fixture.heap_buffer, heap_message, sizeof(heap_message));

    if (pipe2(fixture.pipe_fds, O_CLOEXEC | O_NONBLOCK) != 0) {
        fail("pipe2");
    }
    if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, fixture.socket_fds) != 0) {
        fail("socketpair");
    }
    fixture.event_fd = eventfd(7, EFD_CLOEXEC | EFD_NONBLOCK);
    if (fixture.event_fd < 0) {
        fail("eventfd");
    }
    fixture.epoll_fd = epoll_create1(EPOLL_CLOEXEC);
    if (fixture.epoll_fd < 0) {
        fail("epoll_create1");
    }
    struct epoll_event event = {.events = EPOLLIN, .data.u64 = UINT64_C(0xfeedface)};
    if (epoll_ctl(fixture.epoll_fd, EPOLL_CTL_ADD, fixture.event_fd, &event) != 0) {
        fail("epoll_ctl");
    }
    const char socket_message[] = "unix socket payload";
    if (write(fixture.socket_fds[0], socket_message, sizeof(socket_message)) < 0) {
        fail("socket write");
    }

    fixture.word.integer = UINT64_C(0x4142434445464748);
    fixture.flags = (struct PacketFlags){.readable = 1, .writable = 1, .priority = 17};
    c_memory_checkpoint(&fixture);

    close(fixture.epoll_fd);
    close(fixture.event_fd);
    close(fixture.socket_fds[0]);
    close(fixture.socket_fds[1]);
    close(fixture.pipe_fds[0]);
    close(fixture.pipe_fds[1]);
    munmap(fixture.shared_mapping, fixture.shared_size);
    close(fixture.memfd);
    munmap(fixture.anonymous_mapping, fixture.anonymous_size);
    free(fixture.heap_buffer);
    return 0;
}
