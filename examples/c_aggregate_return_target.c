#if defined(__GNUC__)
#define FGDB_NOINLINE __attribute__((noinline))
#else
#define FGDB_NOINLINE
#endif

typedef struct { long long x; long long y; } Wide;
typedef struct { double x; double y; } Floats;
typedef struct { long long x; double y; } Mixed;
typedef struct { double x; long long y; } Reversed;
typedef struct { int x[2]; float y[2]; } Nested;
typedef struct { long long x; long long y; long long z; } Large;
typedef union { long long x; double y; } Choice;

FGDB_NOINLINE Wide return_wide(void) {
    Wide value = {0x1122334455667788LL, 0x2233445566778899LL};
    return value;
}

FGDB_NOINLINE Floats return_floats(void) {
    Floats value = {3.25, -1.5};
    return value;
}

FGDB_NOINLINE Mixed return_mixed(void) {
    Mixed value = {42, 3.25};
    return value;
}

FGDB_NOINLINE Reversed return_reversed(void) {
    Reversed value = {3.25, 42};
    return value;
}

FGDB_NOINLINE Nested return_nested(void) {
    Nested value = {{7, 11}, {3.25F, -1.5F}};
    return value;
}

FGDB_NOINLINE Large return_large(void) {
    Large value = {7, 11, 13};
    return value;
}

FGDB_NOINLINE Choice return_union(void) {
    Choice value = {42};
    return value;
}

#ifdef __cplusplus
struct Nontrivial {
    int x = 7;
    int y = 11;
    ~Nontrivial() {}
};

FGDB_NOINLINE Nontrivial return_nontrivial() {
    return {};
}
#endif

int main(void) {
    volatile Wide wide = return_wide();
    volatile Floats floats = return_floats();
    volatile Mixed mixed = return_mixed();
    volatile Reversed reversed = return_reversed();
    volatile Nested nested = return_nested();
    volatile Large large = return_large();
    volatile Choice choice = return_union();

#ifdef __cplusplus
    Nontrivial nontrivial = return_nontrivial();
    (void)nontrivial;
#endif

    return wide.x == 0x1122334455667788LL && floats.x == 3.25 && mixed.x == 42
        && reversed.y == 42 && nested.x[1] == 11 && large.z == 13 && choice.x == 42 ? 0 : 1;
}
