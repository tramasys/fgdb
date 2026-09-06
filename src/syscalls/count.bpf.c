// SPDX-License-Identifier: (MIT OR GPL-2.0-only)
// Minimal CO-RE types keep host kernel headers out of the build.
typedef unsigned int u32;
typedef unsigned long long u64;

#define SEC(name) __attribute__((section(name), used))
#define TYPE(name, value) typeof(value) *name
#define UINT(name, value) int (*name)[value]
#define CORE __attribute__((preserve_access_index))
#define READ(dst, src) read_kernel(&(dst), sizeof(dst), __builtin_preserve_access_index(src))

struct thread_info {
#ifdef FGDB_x86_64
    unsigned int status;
#else
    unsigned long flags;
#endif
} CORE;

struct task_struct {
    struct thread_info thread_info;
    struct task_struct *group_leader;
    u64 start_boottime;
} CORE;

struct pidns_info { u32 pid, tgid; };
struct raw_args { u64 args[2]; };
struct configuration { u64 dev, ino, start_ticks, tick_ns, pid; };
struct counters { u64 counts[3 * 1024], lost; };
struct key { u64 number, abi; };

struct {
    UINT(type, 2);
    UINT(max_entries, 1);
    TYPE(key, u32);
    TYPE(value, struct configuration);
} config SEC(".maps");

// One read per CPU supplies all dense counters, not one allocation per syscall.
struct {
    UINT(type, 6);
    UINT(max_entries, 1);
    TYPE(key, u32);
    TYPE(value, struct counters);
} counts SEC(".maps");

// Invalid, future and architecture-private numbers retain their raw identity.
struct {
    UINT(type, 5);
    UINT(max_entries, 128);
    TYPE(key, struct key);
    TYPE(value, u64);
} unusual SEC(".maps");

static void *(*lookup)(void *, const void *) = (void *)1;
static long (*update)(void *, const void *, const void *, u64) = (void *)2;
static u64 (*current_pid)(void) = (void *)14;
static struct task_struct *(*current_task)(void) = (void *)35;
static long (*read_kernel)(void *, u32, const void *) = (void *)113;
static long (*namespace_pid)(u64, u64, struct pidns_info *, u32) = (void *)120;
static u32 host_tgid;

static __attribute__((always_inline)) void increment(u64 *value, u64 *lost) {
    if (*value != (u64)-1)
        *value += 1;
    else if (*lost != (u64)-1)
        *lost += 1;
}

SEC("raw_tracepoint/sys_enter")
int count_entry(struct raw_args *ctx) {
    u32 tgid = current_pid() >> 32;

    // After resolving the namespace once, unrelated processes need only a PID
    // check. The full birth identity remains checked for every matching entry.
    if (host_tgid && host_tgid != tgid)
        return 0;

    u32 zero = 0;
    struct configuration *cfg = lookup(&config, &zero);
    struct pidns_info identity = {};

    if (!cfg || !cfg->tick_ns)
        return 0;

    if (!host_tgid &&
        (namespace_pid(cfg->dev, cfg->ino, &identity, sizeof(identity)) ||
         identity.tgid != cfg->pid))
        return 0;

    struct task_struct *task = current_task();
    struct task_struct *leader = 0;
    u64 started = 0;

    if (READ(leader, &task->group_leader) ||
        READ(started, &leader->start_boottime) ||
        started / cfg->tick_ns != cfg->start_ticks)
        return 0;

    host_tgid = tgid;
    struct counters *values = lookup(&counts, &zero);

    if (!values)
        return 0;

    u64 number = ctx->args[1];
    u64 abi = 0;

#ifdef FGDB_x86_64
    u32 status = 0;

    if (READ(status, &task->thread_info.status)) {
        increment(&values->lost, &values->lost);
        return 0;
    }

    if (status & 2)
        abi = 1;
    else if (number & 0x40000000)
        abi = 2;
#else
    unsigned long flags = 0;

    if (READ(flags, &task->thread_info.flags)) {
        increment(&values->lost, &values->lost);
        return 0;
    }

    if (flags & (1UL << 22))
        abi = 1;
#endif

    u64 index = abi == 2 ? number & ~0x40000000ULL : number;

    if (index < 1024) {
        increment(&values->counts[abi * 1024 + index], &values->lost);
        return 0;
    }

    struct key key = { .number = number, .abi = abi };
    u64 *value = lookup(&unusual, &key);

    if (!value) {
        u64 initial = 0;
        update(&unusual, &key, &initial, 1);
        value = lookup(&unusual, &key);
    }

    if (value)
        increment(value, &values->lost);
    else
        increment(&values->lost, &values->lost);

    return 0;
}

char program_license[] SEC("license") = "Dual MIT/GPL";
