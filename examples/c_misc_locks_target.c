#define _GNU_SOURCE

#include <errno.h>
#include <limits.h>
#include <linux/futex.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

#if defined(__GNUC__)
#define FGDB_NOINLINE __attribute__((noinline))
#else
#define FGDB_NOINLINE
#endif

struct LockFixture {
    _Atomic uint32_t shared_gate;
    _Atomic uint32_t separate_gate;
    _Atomic unsigned ready;
    pthread_t workers[3];
};

struct WorkerArgument {
    struct LockFixture *fixture;
    _Atomic uint32_t *gate;
    const char *name;
};

static int futex_wait(_Atomic uint32_t *address, uint32_t expected) {
    return (int)syscall(
        SYS_futex,
        (uint32_t *)(void *)address,
        FUTEX_WAIT_PRIVATE,
        expected,
        NULL,
        NULL,
        0
    );
}

static int futex_wake(_Atomic uint32_t *address) {
    return (int)syscall(
        SYS_futex,
        (uint32_t *)(void *)address,
        FUTEX_WAKE_PRIVATE,
        INT_MAX,
        NULL,
        NULL,
        0
    );
}

static void *waiter(void *opaque) {
    struct WorkerArgument *argument = opaque;
    pthread_setname_np(pthread_self(), argument->name);
    atomic_fetch_add_explicit(&argument->fixture->ready, 1, memory_order_release);
    while (atomic_load_explicit(argument->gate, memory_order_acquire) == 0) {
        if (futex_wait(argument->gate, 0) != 0 && errno != EAGAIN && errno != EINTR) {
            perror("futex wait");
            return (void *)(uintptr_t)1;
        }
    }
    return NULL;
}

FGDB_NOINLINE void c_misc_locks_checkpoint(const struct LockFixture *fixture) {
    printf(
        "locks checkpoint: ready=%u shared=%p separate=%p workers=%#lx/%#lx/%#lx\n",
        atomic_load_explicit(&fixture->ready, memory_order_acquire),
        (const void *)&fixture->shared_gate,
        (const void *)&fixture->separate_gate,
        (unsigned long)fixture->workers[0],
        (unsigned long)fixture->workers[1],
        (unsigned long)fixture->workers[2]
    );
}

int main(void) {
    struct LockFixture fixture = {0};
    struct WorkerArgument arguments[3] = {
        {.fixture = &fixture, .gate = &fixture.shared_gate, .name = "fgdb-lock-a"},
        {.fixture = &fixture, .gate = &fixture.shared_gate, .name = "fgdb-lock-b"},
        {.fixture = &fixture, .gate = &fixture.separate_gate, .name = "fgdb-lock-c"},
    };
    for (size_t index = 0; index < 3; ++index) {
        if (pthread_create(&fixture.workers[index], NULL, waiter, &arguments[index]) != 0) {
            fputs("pthread_create failed\n", stderr);
            return EXIT_FAILURE;
        }
    }
    while (atomic_load_explicit(&fixture.ready, memory_order_acquire) != 3) {
        sched_yield();
    }
    const struct timespec settle = {.tv_sec = 0, .tv_nsec = 100 * 1000 * 1000};
    nanosleep(&settle, NULL);
    c_misc_locks_checkpoint(&fixture);

    atomic_store_explicit(&fixture.shared_gate, 1, memory_order_release);
    atomic_store_explicit(&fixture.separate_gate, 1, memory_order_release);
    (void)futex_wake(&fixture.shared_gate);
    (void)futex_wake(&fixture.separate_gate);
    for (size_t index = 0; index < 3; ++index) {
        void *result = NULL;
        pthread_join(fixture.workers[index], &result);
        if (result != NULL) {
            return EXIT_FAILURE;
        }
    }
    return EXIT_SUCCESS;
}
