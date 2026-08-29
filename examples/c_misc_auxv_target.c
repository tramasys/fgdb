#define _GNU_SOURCE

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/auxv.h>
#include <unistd.h>

#if defined(__GNUC__)
#define FGDB_NOINLINE __attribute__((noinline))
#else
#define FGDB_NOINLINE
#endif

extern char **environ;

struct AuxvFixture {
    unsigned long page_size;
    unsigned long program_headers;
    unsigned long program_header_count;
    unsigned long entry;
    unsigned long interpreter_base;
    unsigned long hardware_capabilities;
    unsigned long hardware_capabilities_2;
    unsigned long secure_execution;
    unsigned long vdso;
    const char *first_argument;
    const char *fixture_message;
    size_t environment_count;
};

FGDB_NOINLINE void c_misc_auxv_checkpoint(
    int argc,
    char **argv,
    char **envp,
    const struct AuxvFixture *fixture
) {
    printf(
        "auxv checkpoint: argc=%d argv=%p envp=%p pagesz=%lu entry=%#lx "
        "vdso=%#lx environment=%zu message=%s\n",
        argc,
        (void *)argv,
        (void *)envp,
        fixture->page_size,
        fixture->entry,
        fixture->vdso,
        fixture->environment_count,
        fixture->fixture_message
    );
}

int main(int argc, char **argv, char **envp) {
    size_t environment_count = 0;
    while (environ[environment_count] != NULL) {
        ++environment_count;
    }

    const char *message = getenv("FGDB_FIXTURE_MESSAGE");
    if (message == NULL) {
        message = "set FGDB_FIXTURE_MESSAGE in the launch environment";
    }

    struct AuxvFixture fixture = {
        .page_size = getauxval(AT_PAGESZ),
        .program_headers = getauxval(AT_PHDR),
        .program_header_count = getauxval(AT_PHNUM),
        .entry = getauxval(AT_ENTRY),
        .interpreter_base = getauxval(AT_BASE),
        .hardware_capabilities = getauxval(AT_HWCAP),
#ifdef AT_HWCAP2
        .hardware_capabilities_2 = getauxval(AT_HWCAP2),
#endif
        .secure_execution = getauxval(AT_SECURE),
#ifdef AT_SYSINFO_EHDR
        .vdso = getauxval(AT_SYSINFO_EHDR),
#endif
        .first_argument = argc > 0 ? argv[0] : NULL,
        .fixture_message = message,
        .environment_count = environment_count,
    };
    c_misc_auxv_checkpoint(argc, argv, envp, &fixture);
    return fixture.page_size == 0 ? EXIT_FAILURE : EXIT_SUCCESS;
}
