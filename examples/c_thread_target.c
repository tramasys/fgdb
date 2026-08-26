#define _GNU_SOURCE

#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#if defined(__GNUC__)
#define FGDB_NOINLINE __attribute__((noinline))
#else
#define FGDB_NOINLINE
#endif

struct ThreadFixture {
    pthread_mutex_t mutex;
    pthread_cond_t changed;
    int pipe_fds[2];
    unsigned ready;
    int release;
    atomic_uint_fast64_t counter;
};

static volatile sig_atomic_t signal_seen;

static void handle_signal(int signal_number) {
    signal_seen = signal_number;
}

static void mark_ready(struct ThreadFixture *fixture) {
    pthread_mutex_lock(&fixture->mutex);
    ++fixture->ready;
    pthread_cond_broadcast(&fixture->changed);
    pthread_mutex_unlock(&fixture->mutex);
}

static void *condition_worker(void *argument) {
    struct ThreadFixture *fixture = argument;
    pthread_setname_np(pthread_self(), "fgdb-cond");
    pthread_mutex_lock(&fixture->mutex);
    ++fixture->ready;
    pthread_cond_broadcast(&fixture->changed);
    while (!fixture->release) {
        pthread_cond_wait(&fixture->changed, &fixture->mutex);
    }
    pthread_mutex_unlock(&fixture->mutex);
    atomic_fetch_add_explicit(&fixture->counter, 11, memory_order_relaxed);
    return NULL;
}

static void *poll_worker(void *argument) {
    struct ThreadFixture *fixture = argument;
    pthread_setname_np(pthread_self(), "fgdb-poll");
    mark_ready(fixture);
    struct pollfd descriptor = {.fd = fixture->pipe_fds[0], .events = POLLIN};
    (void)poll(&descriptor, 1, -1);
    atomic_fetch_add_explicit(&fixture->counter, 31, memory_order_relaxed);
    return NULL;
}

FGDB_NOINLINE void c_threads_checkpoint(
    struct ThreadFixture *fixture,
    const pthread_t workers[2]
) {
    printf(
        "thread checkpoint: ready=%u workers=%#lx/%#lx pending SIGUSR2 counter=%llu\n",
        fixture->ready,
        (unsigned long)workers[0],
        (unsigned long)workers[1],
        (unsigned long long)atomic_load_explicit(&fixture->counter, memory_order_relaxed)
    );
}

int main(void) {
    struct sigaction action = {0};
    action.sa_handler = handle_signal;
    sigemptyset(&action.sa_mask);
    sigaction(SIGUSR2, &action, NULL);

    sigset_t blocked;
    sigemptyset(&blocked);
    sigaddset(&blocked, SIGUSR2);
    pthread_sigmask(SIG_BLOCK, &blocked, NULL);

    struct ThreadFixture fixture = {
        .mutex = PTHREAD_MUTEX_INITIALIZER,
        .changed = PTHREAD_COND_INITIALIZER,
        .ready = 0,
        .release = 0,
        .counter = 1,
    };
    if (pipe(fixture.pipe_fds) != 0) {
        perror("pipe");
        return EXIT_FAILURE;
    }

    pthread_t workers[2];
    if (pthread_create(&workers[0], NULL, condition_worker, &fixture) != 0
        || pthread_create(&workers[1], NULL, poll_worker, &fixture) != 0) {
        fputs("pthread_create failed\n", stderr);
        return EXIT_FAILURE;
    }

    pthread_mutex_lock(&fixture.mutex);
    while (fixture.ready != 2) {
        pthread_cond_wait(&fixture.changed, &fixture.mutex);
    }
    pthread_mutex_unlock(&fixture.mutex);
    pthread_kill(workers[1], SIGUSR2);

    c_threads_checkpoint(&fixture, workers);

    pthread_mutex_lock(&fixture.mutex);
    fixture.release = 1;
    pthread_cond_broadcast(&fixture.changed);
    pthread_mutex_unlock(&fixture.mutex);
    const char release = 'x';
    (void)write(fixture.pipe_fds[1], &release, 1);
    pthread_join(workers[0], NULL);
    pthread_join(workers[1], NULL);
    close(fixture.pipe_fds[0]);
    close(fixture.pipe_fds[1]);
    pthread_cond_destroy(&fixture.changed);
    pthread_mutex_destroy(&fixture.mutex);
    printf("threads complete: counter=%llu signal_seen=%d\n",
        (unsigned long long)atomic_load(&fixture.counter), (int)signal_seen);
    return 0;
}
