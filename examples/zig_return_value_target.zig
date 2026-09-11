const std = @import("std");
const Pair = extern struct { x: i32, y: i32 };

noinline fn return_pair() callconv(.c) Pair {
    return .{ .x = 7, .y = 11 };
}

noinline fn return_native_pair() Pair {
    return .{ .x = 13, .y = 17 };
}

pub fn main() void {
    const pair = return_pair();
    std.mem.doNotOptimizeAway(&pair);
    const native_pair = return_native_pair();
    std.mem.doNotOptimizeAway(&native_pair);
}
