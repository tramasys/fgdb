#[derive(Clone, Copy)]
struct Pair {
    x: i32,
    y: i32,
}

#[inline(never)]
fn return_pair() -> Pair {
    Pair { x: 7, y: 11 }
}

struct Floats {
    x: f32,
    y: f32,
}

#[inline(never)]
fn return_floats() -> Floats {
    Floats { x: 3.25, y: -1.5 }
}

fn main() {
    let pair = return_pair();
    std::hint::black_box((pair.x, pair.y));
    let floats = return_floats();
    std::hint::black_box((floats.x, floats.y));
}
