module c3_variable_viewer_target;

struct Particle {
    int id;
    double[3] position;
}

enum Mode { IDLE, READY }

fn void c3_values_ready() @noinline {
}

fn int main() {
    long counter = 42;
    bool enabled = true;
    double scale = 1.25;
    Mode mode = Mode.READY;
    int[5] values = {-20, -10, 0, 10, 20};
    int[] slice = values[1..3];
    int[] empty;
    String text = "Hello from C3";
    char[7] editable_bytes = {'m', 'u', 't', 'a', 'b', 'l', 'e'};
    char[] editable = editable_bytes[..];
    char[300] truncated_bytes;
    char[] truncated = truncated_bytes[..];
    char[2] invalid_bytes = {0xff, 0xfe};
    char[] invalid = invalid_bytes[..];
    Particle particle = {7, {1.0, 2.0, 3.0}};
    Particle[2] particles = {particle, {9, {4.0, 5.0, 6.0}}};
    Particle[] records = particles[..];
    int[][2] rows = {slice, empty};
    int[][] nested = rows[..];
    String raw = `raw `` literal`;
    int* pointer = &values[2];
    int* missing = null;
    int[8192] large;

    for (usz index = 0; index < large.len; index++) {
        large[index] = (int) index * 3;
    }

    for (usz index = 0; index < truncated.len; index++) {
        truncated[index] = 'a';
    }

    truncated[255] = 0xf0;
    truncated[256] = 0x9f;
    truncated[257] = 0xa6;
    truncated[258] = 0x80;
    int[] large_slice = large[..];
    c3_values_ready();
    counter += 1;
    enabled = false;
    values[2] = 99;
    c3_values_ready();
    return 0;
}
