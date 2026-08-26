#define _GNU_SOURCE

#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#if defined(__GNUC__)
#define FGDB_NOINLINE __attribute__((noinline))
#else
#define FGDB_NOINLINE
#endif

enum { WORKER_COUNT = 2, TLS_BLOCK_SIZE = 4096 };

struct TlsRecord {
    uint64_t sequence;
    double ratio;
    char tag[24];
};

struct ThreadFixture {
    pthread_barrier_t ready;
    pthread_barrier_t release;
};

_Thread_local uint64_t tls_counter = UINT64_C(0x1122334455667788);
_Thread_local int32_t tls_signed = -17;
_Thread_local double tls_ratio = 1.25;
_Thread_local char tls_name[32] = "fgdb-main";
_Thread_local struct TlsRecord tls_record = {
    .sequence = UINT64_C(0x0102030405060708),
    .ratio = 0.5,
    .tag = "initial-record",
};
_Thread_local unsigned char tls_zero_block[TLS_BLOCK_SIZE];

static void initialize_tls(unsigned worker_index) {
    tls_counter += UINT64_C(0x1000) * worker_index;
    tls_signed -= (int32_t)(worker_index * 10U);
    tls_ratio += (double)worker_index / 8.0;
    (void)snprintf(tls_name, sizeof(tls_name), "fgdb-worker-%u", worker_index);
    tls_record.sequence += worker_index;
    tls_record.ratio += (double)worker_index;
    (void)snprintf(tls_record.tag, sizeof(tls_record.tag), "record-%u", worker_index);
    memset(tls_zero_block, (int)(0x40U + worker_index), 32U * worker_index);
}

static void *tls_worker(void *argument) {
    struct ThreadFixture *fixture = argument;
    static pthread_mutex_t index_mutex = PTHREAD_MUTEX_INITIALIZER;
    static unsigned next_index = 1;

    pthread_mutex_lock(&index_mutex);
    const unsigned worker_index = next_index++;
    pthread_mutex_unlock(&index_mutex);

    char thread_name[16];
    (void)snprintf(thread_name, sizeof(thread_name), "fgdb-tls-%u", worker_index);
    (void)pthread_setname_np(pthread_self(), thread_name);
    initialize_tls(worker_index);
    (void)pthread_barrier_wait(&fixture->ready);
    (void)pthread_barrier_wait(&fixture->release);
    return (void *)(uintptr_t)tls_counter;
}

FGDB_NOINLINE void c_tls_checkpoint(const char *stage) {
    printf(
        "TLS checkpoint %-12s thread=%#lx counter=%#llx signed=%d ratio=%.3f "
        "name=%s record={%#llx, %.3f, %s} block=%p\n",
        stage,
        (unsigned long)pthread_self(),
        (unsigned long long)tls_counter,
        tls_signed,
        tls_ratio,
        tls_name,
        (unsigned long long)tls_record.sequence,
        tls_record.ratio,
        tls_record.tag,
        (void *)tls_zero_block
    );
}

int main(void) {
    struct ThreadFixture fixture;
    if (pthread_barrier_init(&fixture.ready, NULL, WORKER_COUNT + 1U) != 0
        || pthread_barrier_init(&fixture.release, NULL, WORKER_COUNT + 1U) != 0) {
        fputs("pthread_barrier_init failed\n", stderr);
        return EXIT_FAILURE;
    }

    pthread_t workers[WORKER_COUNT];
    for (size_t index = 0; index < WORKER_COUNT; ++index) {
        if (pthread_create(&workers[index], NULL, tls_worker, &fixture) != 0) {
            fputs("pthread_create failed\n", stderr);
            return EXIT_FAILURE;
        }
    }

    (void)pthread_barrier_wait(&fixture.ready);
    c_tls_checkpoint("threads-ready");

    tls_counter = UINT64_C(0xfedcba9876543210);
    tls_signed = 2026;
    tls_ratio = 3.141592653589793;
    (void)snprintf(tls_name, sizeof(tls_name), "fgdb-main-mutated");
    tls_record.sequence = UINT64_C(0xaabbccddeeff0011);
    tls_record.ratio = 9.5;
    (void)snprintf(tls_record.tag, sizeof(tls_record.tag), "mutated-record");
    memset(tls_zero_block, 0xa5, sizeof(tls_zero_block));
    c_tls_checkpoint("main-mutated");

    (void)pthread_barrier_wait(&fixture.release);
    for (size_t index = 0; index < WORKER_COUNT; ++index) {
        void *result = NULL;
        (void)pthread_join(workers[index], &result);
        printf("worker %zu returned TLS counter %#lx\n", index + 1U, (unsigned long)result);
    }
    (void)pthread_barrier_destroy(&fixture.release);
    (void)pthread_barrier_destroy(&fixture.ready);
    return 0;
}
