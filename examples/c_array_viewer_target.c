#include <stdio.h>

__attribute__((noinline)) void c_arrays_ready(void) {
    volatile int ready = 1;
    (void)ready;
}

int main(void) {
    int large[8192];
    int matrix[24][32];
    int cube[4][5][6];

    for (int i = 0; i < 8192; ++i) {
        large[i] = i * 3;
    }

    for (int i = 0; i < 24; ++i) {
        for (int j = 0; j < 32; ++j) {
            matrix[i][j] = i * 100 + j;
        }
    }

    for (int i = 0; i < 4; ++i) {
        for (int j = 0; j < 5; ++j) {
            for (int k = 0; k < 6; ++k) {
                cube[i][j][k] = i * 100 + j * 10 + k;
            }
        }
    }

    // Break at c_arrays_ready, then select caller frame 1.
    c_arrays_ready();
    printf("%d %d %d\n", large[8000], matrix[23][31], cube[3][4][5]);
    return 0;
}
