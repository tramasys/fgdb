module d_variable_viewer_target;

struct Particle {
    int id;
    double[3] position;
}

enum Mode { idle, ready }

pragma(inline, false)
extern(C) void d_values_ready() {
}

void main() {
    long counter = 42;
    bool enabled = true;
    double scale = 1.25;
    Mode mode = Mode.ready;
    int[5] values = [-20, -10, 0, 10, 20];
    int[] slice = values[1 .. 4];
    int[] empty;
    string text = "Hello from D";
    wstring wide = "Grüße"w;
    dstring unicode = "λ🦀"d;
    char[] editable = "mutable".dup;
    char[] truncated = new char[300];
    truncated[] = 'a';
    truncated[255 .. 259] = "🦀";
    char[] invalid = [cast(char) 0xff, cast(char) 0xfe];
    Particle particle = Particle(7, [1.0, 2.0, 3.0]);
    int* pointer = &values[2];
    int[8192] large;
    int[string] lookup = ["first": 1];

    foreach (index, ref value; large) {
        value = cast(int) index * 3;
    }

    int[] large_slice = large[];
    d_values_ready();
    counter += 1;
    enabled = false;
    values[2] = 99;
    d_values_ready();
}
