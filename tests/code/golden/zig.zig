const std = @import("std");
const mem = @import("std/mem.zig");

pub const MAX_SIZE = 1024;

var global_counter: u32 = 0;

pub const Point = struct {
    x: f32,
    y: f32,
};

const Color = enum {
    red,
    green,
    blue,
};

const MyError = error{
    OutOfMemory,
    InvalidArgument,
};

pub fn add(a: i32, b: i32) i32 {
    return a + b;
}

fn internal_helper() void {}

test "basic math" {
    try std.testing.expect(add(2, 2) == 4);
}
