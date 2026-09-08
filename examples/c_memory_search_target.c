#include <stdint.h>
#include <string.h>

/* Break at search_checkpoint after the buffer has been initialized.
 * Search search_bytes through search_bytes + sizeof search_bytes (end exclusive).
 * The text fgdb-needle-42 appears at offsets 65533 and 65590. The first match
 * crosses a 64 KiB read boundary when searching from the buffer's start.
 * search_number, search_float and search_pointer exercise typed searches.
 * Continue to the second checkpoint to check invalidation after a value change.
 */

_Alignas(65536) unsigned char search_bytes[131072];
uint64_t search_number = UINT64_C(0x1122334455667788);
double search_float = 1.5;
void *search_pointer = &search_number;

__attribute__((noinline)) void search_checkpoint(void) {
    __asm__ volatile("" ::: "memory");
}

int main(void) {
    memset(search_bytes, 0x5a, sizeof search_bytes);
    memcpy(search_bytes + 65533, "fgdb-needle-42", 14);
    memcpy(search_bytes + 65590, "fgdb-needle-42", 14);
    search_checkpoint();
    search_number += 1;
    search_checkpoint();
    return 0;
}
