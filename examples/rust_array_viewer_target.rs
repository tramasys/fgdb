use std::hint::black_box;

#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn rust_arrays_ready() {
    black_box(());
}

fn main() {
    let large: [i32; 8192] = std::array::from_fn(|i| i as i32 * 3);
    let matrix: [[i32; 32]; 24] = std::array::from_fn(|i| {
        std::array::from_fn(|j| i as i32 * 100 + j as i32)
    });

    let cube: [[[i32; 6]; 5]; 4] = std::array::from_fn(|i| {
        std::array::from_fn(|j| std::array::from_fn(|k| (i * 100 + j * 10 + k) as i32))
    });

    // Break at rust_arrays_ready, then select caller frame 1.
    rust_arrays_ready();
    black_box((&large, &matrix, &cube));
}
