const std = @import("std");

const Particle = struct { id: u32, position: [3]f64 };
const Choice = union(enum) { count: i32, point: Particle };

pub fn main() void {
    var values = [_]i32{ -20, -10, 0, 10, 20 };
    var slice: []i32 = &values;
    var text: []const u8 = "Hello from Zig";
    var present: ?i32 = 42;
    var absent: ?i32 = null;
    var success: error{Failed}!i32 = 7;
    var failure: error{Failed}!i32 = error.Failed;
    var choice: Choice = .{ .count = 17 };
    var particle: Particle = .{ .id = 3, .position = .{ 1, 2, 3 } };

    // Set a breakpoint on this call to inspect initialized values.
    std.mem.doNotOptimizeAway(.{ &slice, &text, &present, &absent, &success, &failure, &choice, &particle });
    slice[2] = 99;
    present = null;
    choice = .{ .point = particle };
    std.mem.doNotOptimizeAway(.{ &slice, &present, &choice });
}
