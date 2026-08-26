#define _GNU_SOURCE

#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/prctl.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#if defined(__GNUC__)
#define FGDB_NOINLINE __attribute__((noinline))
#else
#define FGDB_NOINLINE
#endif

static void wait_for_release(int descriptor) {
    char byte;
    while (read(descriptor, &byte, 1) < 0 && errno == EINTR) {
    }
}

FGDB_NOINLINE void c_process_checkpoint(pid_t child_pid) {
    printf("process checkpoint: parent=%ld child=%ld\n", (long)getpid(), (long)child_pid);
}

int main(void) {
    int ready_pipe[2];
    int release_pipe[2];
    if (pipe(ready_pipe) != 0 || pipe(release_pipe) != 0) {
        perror("pipe");
        return EXIT_FAILURE;
    }

    const pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return EXIT_FAILURE;
    }
    if (child == 0) {
        prctl(PR_SET_NAME, "fgdb-child", 0, 0, 0);
        close(ready_pipe[0]);
        close(release_pipe[1]);
        const pid_t grandchild = fork();
        if (grandchild < 0) {
            _exit(2);
        }
        if (grandchild == 0) {
            prctl(PR_SET_NAME, "fgdb-grandchild", 0, 0, 0);
            close(ready_pipe[1]);
            wait_for_release(release_pipe[0]);
            _exit(0);
        }
        const char ready = 'r';
        (void)write(ready_pipe[1], &ready, 1);
        close(ready_pipe[1]);
        wait_for_release(release_pipe[0]);
        waitpid(grandchild, NULL, 0);
        _exit(0);
    }

    close(ready_pipe[1]);
    close(release_pipe[0]);
    char ready;
    while (read(ready_pipe[0], &ready, 1) < 0 && errno == EINTR) {
    }
    c_process_checkpoint(child);

    const char release[2] = {'a', 'b'};
    (void)write(release_pipe[1], release, sizeof(release));
    close(release_pipe[1]);
    close(ready_pipe[0]);
    waitpid(child, NULL, 0);
    return 0;
}
