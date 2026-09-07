package main

import "core:fmt"

Particle :: struct {
    id: i32,
    position: [3]f64,
}

main :: proc() {
    values := [5]i32{-20, -10, 0, 10, 20}
    slice := values[:]
    text := "Hello from Odin"
    growing := make([dynamic]i32)
    defer delete(growing)
    append(&growing, 5, 4, 3, 2, 1)
    lookup := make(map[string]i32)
    defer delete(lookup)
    lookup["first"] = 1
    particle := Particle{3, {1, 2, 3}}
    choice: union{i32, Particle} = i32(17)
    required: union #no_nil {i32, Particle} = i32(19)
    enabled := true
    empty: []i32

    // Set a breakpoint on this print to inspect initialized values.
    fmt.println(values, slice, text, growing, lookup, particle, choice, required, enabled, empty)
    slice[2] = 99
    choice = particle
    enabled = false
    fmt.println(slice, choice, enabled)
}
