module d_return_value_target;

struct Pair {
    int x;
    int y;
}

pragma(inline, false)
Pair return_pair() {
    return Pair(7, 11);
}

int main() {
    auto pair = return_pair();
    return pair.x == 7 && pair.y == 11 ? 0 : 1;
}
