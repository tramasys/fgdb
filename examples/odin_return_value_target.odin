package main

import "core:fmt"

Pair :: struct {
    x: i32,
    y: i32,
}

return_pair :: #force_no_inline proc() -> Pair {
    return Pair{7, 11}
}

main :: proc() {
    pair := return_pair()
    fmt.println(pair)
}
