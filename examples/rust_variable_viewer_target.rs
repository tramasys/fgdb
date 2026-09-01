#![allow(dead_code)] // Every field exists for debugger inspection.
#![allow(improper_ctypes_definitions)]

use std::{
    cell::RefCell,
    collections::{
        BTreeMap, BTreeSet, BinaryHeap, HashMap, HashSet, LinkedList, VecDeque,
    },
    hint::black_box,
    ops::RangeInclusive,
    rc::{Rc, Weak},
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct Record {
    id: u64,
    name: String,
    enabled: bool,
    samples: Vec<i32>,
}

#[derive(Clone, Debug)]
enum Message {
    Empty,
    Text(String),
    Bytes(Vec<u8>),
    Coordinate { x: f64, y: f64 },
}

#[derive(Debug)]
pub struct Node {
    value: i32,
    label: String,
    next: Option<Rc<RefCell<Node>>>,
    previous: Weak<RefCell<Node>>,
}

#[derive(Clone, Copy, Debug)]
struct PrimitiveTypes {
    boolean: bool,
    character: char,
    signed: i128,
    unsigned: u128,
    pointer_sized: usize,
    float32: f32,
    float64: f64,
}

// Break here, then select caller frame #1. The caller deliberately uses all
// arguments and locals after this call, keeping their DWARF locations live.
#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn rust_types_ready() {
    black_box(());
}

#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn rust_variable_viewer_checkpoint(
    vector_arg: &Vec<i32>,
    deque_arg: &VecDeque<String>,
    linked_list_arg: &LinkedList<i64>,
    hash_map_arg: &HashMap<String, Record>,
    tree_map_arg: &BTreeMap<u32, String>,
    option_arg: &Option<Box<Record>>,
    result_arg: &Result<Vec<u8>, String>,
    string_arg: &String,
    slice_arg: &[u16],
    shared_arg: &Rc<RefCell<Node>>,
) {
    let local_vector = vec![2_i64, 3, 5, 7, 11, 13, 17, 19];
    let local_large_vector = (0_u32..300).collect::<Vec<_>>();
    let local_nested_vector = vec![vec![1_u8, 2, 3], vec![], vec![0xfe, 0xff]];
    let local_deque = VecDeque::from([
        String::from("front"),
        String::from("middle value"),
        String::from("back"),
    ]);
    let local_linked_list = LinkedList::from([10_i32, 20, 30, 40]);
    let local_binary_heap = BinaryHeap::from([29_i32, 7, 41, 13, 3]);

    let local_hash_map = HashMap::from([
        (String::from("alpha"), vec![1_u32, 2, 3]),
        (String::from("beta"), vec![5, 8, 13]),
    ]);
    let local_tree_map = BTreeMap::from([
        (1_u16, Message::Empty),
        (2, Message::Text(String::from("tree value"))),
        (3, Message::Bytes(vec![0, 1, 0x7f, 0x80, 0xff])),
    ]);
    let local_hash_set = HashSet::from(["red", "green", "blue"]);
    let local_tree_set = BTreeSet::from([-8_i32, -1, 0, 1, 8]);

    let local_string = String::from("UTF-8: Zürich λ 🚀");
    let local_str: &str = &local_string[0..15];
    let local_array = [10_u32, 20, 30, 40, 50, 60, 70, 80];
    let local_slice: &[u32] = &local_array[2..7];
    let local_tuple = (7_i32, true, 'λ', String::from("tuple member"));
    let local_range: RangeInclusive<i16> = -3..=9;
    let local_duration = Duration::new(12, 345_678_901);

    let local_option = Some(Box::new(Record {
        id: 501,
        name: String::from("optional record"),
        enabled: true,
        samples: vec![50, 51, 52],
    }));
    let local_none: Option<Record> = None;
    let local_result: Result<Record, String> = Ok(Record {
        id: 601,
        name: String::from("successful record"),
        enabled: false,
        samples: vec![-6, 0, 6],
    });
    let local_error: Result<u64, String> = Err(String::from("intentional error"));

    let local_box = Box::new(Record {
        id: 701,
        name: String::from("boxed record"),
        enabled: true,
        samples: vec![70, 71, 72],
    });
    let local_rc = Rc::clone(shared_arg);
    let local_arc = Arc::new(Mutex::new(Record {
        id: 801,
        name: String::from("shared record"),
        enabled: true,
        samples: vec![80, 81, 82],
    }));
    let local_raw_pointer = local_array.as_ptr();
    let local_reference = &local_tuple;
    let local_primitives = PrimitiveTypes {
        boolean: true,
        character: 'ß',
        signed: -(1_i128 << 100),
        unsigned: (1_u128 << 127) + 123,
        pointer_sized: 0xfeed_face,
        float32: 1.25,
        float64: -0.0,
    };

    rust_types_ready();

    // These reads occur after the marker so GDB can describe every value in
    // the caller frame even when a newer rustc performs basic liveness work.
    black_box((
        vector_arg,
        deque_arg,
        linked_list_arg,
        hash_map_arg,
        tree_map_arg,
        option_arg,
        result_arg,
        string_arg,
        slice_arg,
        shared_arg,
        &local_vector,
        &local_large_vector,
        &local_nested_vector,
        &local_deque,
        &local_linked_list,
        &local_binary_heap,
        &local_hash_map,
        &local_tree_map,
        &local_hash_set,
        &local_tree_set,
        &local_string,
        local_str,
        &local_array,
        local_slice,
        &local_tuple,
        &local_range,
        &local_duration,
        &local_option,
        &local_none,
        &local_result,
        &local_error,
        &local_box,
        &local_rc,
        &local_arc,
        local_raw_pointer,
        local_reference,
        &local_primitives,
    ));

    println!(
        "rust types: vector={} deque={} map={} local={} text={}",
        vector_arg.len(),
        deque_arg.len(),
        hash_map_arg.len() + tree_map_arg.len(),
        local_vector.len() + local_deque.len(),
        local_string
    );
}

fn main() {
    let vector = vec![1_i32, 1, 2, 3, 5, 8, 13, 21];
    let deque = VecDeque::from([
        String::from("zero"),
        String::from("one"),
        String::from("two words"),
        String::from("three"),
    ]);
    let linked_list = LinkedList::from([-5_i64, -3, -1, 1, 3, 5]);
    let hash_map = HashMap::from([
        (
            String::from("first"),
            Record {
                id: 101,
                name: String::from("hash record one"),
                enabled: true,
                samples: vec![1, 10, 100],
            },
        ),
        (
            String::from("second"),
            Record {
                id: 102,
                name: String::from("hash record two"),
                enabled: false,
                samples: vec![2, 20, 200],
            },
        ),
    ]);
    let tree_map = BTreeMap::from([
        (10_u32, String::from("ten")),
        (20, String::from("twenty")),
        (30, String::from("thirty")),
    ]);
    let option = Some(Box::new(Record {
        id: 201,
        name: String::from("argument option"),
        enabled: true,
        samples: vec![2, 0, 1],
    }));
    let result = Err(String::from("argument Result error"));
    let string = String::from("argument String with UTF-8 λ");
    let slice_storage = [0_u16, 1, 2, 3, 5, 8, 13, 21, 34];

    let first = Rc::new(RefCell::new(Node {
        value: 1,
        label: String::from("first node"),
        next: None,
        previous: Weak::new(),
    }));
    let second = Rc::new(RefCell::new(Node {
        value: 2,
        label: String::from("second node"),
        next: None,
        previous: Rc::downgrade(&first),
    }));
    first.borrow_mut().next = Some(second);

    rust_variable_viewer_checkpoint(
        &vector,
        &deque,
        &linked_list,
        &hash_map,
        &tree_map,
        &option,
        &result,
        &string,
        &slice_storage[1..8],
        &first,
    );
}
