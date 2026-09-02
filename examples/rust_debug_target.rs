#![allow(improper_ctypes_definitions)]
#![allow(dead_code)] // Fields are intentionally retained for debugger inspection.

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, VecDeque},
    hint::black_box,
    rc::{Rc, Weak},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

#[unsafe(no_mangle)]
pub static RUST_WATCHED: AtomicU64 = AtomicU64::new(0x1234_5678_9abc_def0);

#[derive(Debug, Clone, Copy)]
#[repr(u16)]
enum PacketKind {
    Control = 0x10,
    Payload = 0x20,
    Shutdown = 0xff,
}

#[derive(Debug)]
enum WireValue {
    Empty,
    Signed(i64),
    Text(String),
    Bytes(Vec<u8>),
    Coordinates { x: f64, y: f64 },
}

#[derive(Debug)]
struct Node {
    id: u32,
    label: String,
    next: Option<Rc<RefCell<Node>>>,
    previous: Weak<RefCell<Node>>,
}

trait Describe: std::fmt::Debug {
    fn describe(&self) -> String;
}

#[derive(Debug)]
struct Temperature(f64);

impl Describe for Temperature {
    fn describe(&self) -> String {
        format!("{:.2} °C", self.0)
    }
}

#[derive(Debug)]
#[repr(C)]
struct PrimitiveSamples {
    signed: i128,
    unsigned: u128,
    pointer_sized: usize,
    byte: u8,
    character: char,
    enabled: bool,
    ratio: f32,
    precise: f64,
}

#[derive(Debug)]
pub struct RustState {
    name: String,
    kind: PacketKind,
    values: Vec<WireValue>,
    optional: Option<Box<WireValue>>,
    result: Result<u64, String>,
    hash: HashMap<String, usize>,
    ordered: BTreeMap<u32, String>,
    queue: VecDeque<i16>,
    root: Rc<RefCell<Node>>,
    trait_object: Box<dyn Describe>,
    primitives: PrimitiveSamples,
    byte_string: Vec<u8>,
}

#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn rust_debugger_checkpoint(state: &mut RustState, worker_total: u64) {
    let watched = RUST_WATCHED.fetch_add(worker_total, Ordering::Relaxed);
    let description = state.trait_object.describe();
    black_box(&state.values);

    println!(
        "rust checkpoint: {} {description} watched={watched:#x} worker={worker_total}",
        state.name
    );
}

fn main() {
    let first = Rc::new(RefCell::new(Node {
        id: 1,
        label: "first".to_owned(),
        next: None,
        previous: Weak::new(),
    }));

    let second = Rc::new(RefCell::new(Node {
        id: 2,
        label: "second".to_owned(),
        next: Some(Rc::clone(&first)),
        previous: Rc::downgrade(&first),
    }));

    first.borrow_mut().next = Some(Rc::clone(&second));

    let mut state = RustState {
        name: "rust-debug-target 🚀".to_owned(),
        kind: PacketKind::Payload,
        values: vec![
            WireValue::Empty,
            WireValue::Signed(-9_223_372_036_854_775_000),
            WireValue::Text("nested Rust string".to_owned()),
            WireValue::Bytes(vec![0, 1, 2, 0x7f, 0x80, 0xfe, 0xff]),
            WireValue::Coordinates { x: 12.25, y: -48.5 },
        ],
        optional: Some(Box::new(WireValue::Text("boxed option".to_owned()))),
        result: Err("intentional Result error".to_owned()),
        hash: HashMap::from([("threads".to_owned(), 1), ("packets".to_owned(), 37)]),
        ordered: BTreeMap::from([(10, "ten".to_owned()), (20, "twenty".to_owned())]),
        queue: VecDeque::from([-3, -2, -1, 0, 1, 2, 3]),
        root: first,
        trait_object: Box::new(Temperature(21.375)),
        primitives: PrimitiveSamples {
            signed: -(1_i128 << 100),
            unsigned: (1_u128 << 127) + 99,
            pointer_sized: 0xfeed_face,
            byte: 0xe1,
            character: 'λ',
            enabled: true,
            ratio: 1.25,
            precise: -0.0,
        },
        byte_string: b"Rust bytes with an embedded \0 NUL".to_vec(),
    };

    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let worker_gate = Arc::clone(&gate);

    let worker = thread::Builder::new()
        .name("fgdb-rust-worker".to_owned())
        .spawn(move || {
            let (lock, changed) = &*worker_gate;
            let mut release = lock.lock().expect("worker gate poisoned");

            while !*release {
                release = changed.wait(release).expect("worker gate poisoned");
            }

            (1_u64..=32).sum::<u64>()
        })
        .expect("cannot start worker");

    rust_debugger_checkpoint(&mut state, 528);

    for iteration in 0..6 {
        RUST_WATCHED.fetch_xor(iteration * 0x101, Ordering::Relaxed);
    }

    let (lock, changed) = &*gate;
    *lock.lock().expect("gate poisoned") = true;
    changed.notify_all();
    let worker_total = worker.join().expect("worker panicked");
    println!("rust complete: worker={worker_total} state={}", state.name);

    // Keep every enum discriminant represented in the DWARF.
    black_box([
        PacketKind::Control,
        PacketKind::Payload,
        PacketKind::Shutdown,
    ]);
}
