module c3_return_value_target;

struct Pair {
    int x;
    int y;
}

fn Pair return_pair() @noinline {
    return {7, 11};
}

fn int return_integer() @noinline {
    return 42;
}

fn int main() {
    Pair pair = return_pair();
    int integer = return_integer();
    return pair.x == 7 && pair.y == 11 && integer == 42 ? 0 : 1;
}
