#include <stdio.h>

struct LocationRecord {
    int count;
    unsigned flags : 3;
    int values[4];
};

int location_global = 42;
int location_probe_calls = 0;

__attribute__((noinline)) int location_probe(void) {
    ++location_probe_calls;
    return 99;
}

__attribute__((noinline)) void location_checkpoint(
    int *pointer, struct LocationRecord *record, int *null_pointer
) {
    /* Break here. Location for pointer is &pointer, not the pointer value.
     * Expand record: flags is a bitfield sharing its containing storage unit.
     * Watch 1 + 2 for a computed value without addressable storage.
     */
    printf("%d %u %p\n", *pointer, record->flags, (void *)null_pointer);
}

int main(void) {
    int local = 17;
    struct LocationRecord record = {7, 5, {10, 20, 30, 40}};
    location_checkpoint(&local, &record, NULL);
    return location_probe_calls;
}
