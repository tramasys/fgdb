#if defined(__GNUC__)
#define FGDB_NOINLINE __attribute__((noinline))
#else
#define FGDB_NOINLINE
#endif

typedef struct {
    int x;
    int y;
} ReturnPair;

static volatile int checkpoint;

FGDB_NOINLINE int return_integer(void) {
    return 42;
}

FGDB_NOINLINE double return_float(void) {
    return 3.25;
}

FGDB_NOINLINE const char *return_string(void) {
    return "fgdb return value";
}

FGDB_NOINLINE ReturnPair return_pair(void) {
    ReturnPair result = {7, 11};
    return result;
}

FGDB_NOINLINE void return_void(void) {
    checkpoint = 1;
}

int main(void) {
    volatile int integer = return_integer();
    volatile double real = return_float();
    const char *volatile string = return_string();
    volatile ReturnPair pair = return_pair();
    return_void();

    return integer == 42 && real == 3.25 && string[0] == 'f'
        && pair.x == 7 && pair.y == 11 && checkpoint == 1 ? 0 : 1;
}
