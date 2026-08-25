#include <algorithm>
#include <array>
#include <atomic>
#include <bit>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <csignal>
#include <condition_variable>
#include <map>
#include <memory>
#include <mutex>
#include <numeric>
#include <optional>
#include <stdexcept>
#include <string>
#include <thread>
#include <variant>
#include <vector>

#if defined(_MSC_VER)
#define FGDB_NOINLINE __declspec(noinline)
#else
#define FGDB_NOINLINE __attribute__((noinline))
#endif

namespace {

volatile std::sig_atomic_t signal_seen = 0;
std::uint64_t watched_value = 0x1111'2222'3333'4444ULL;

struct IntegerSamples {
    std::int8_t i8 = -8;
    std::uint8_t u8 = 250;
    std::int16_t i16 = -16'000;
    std::uint16_t u16 = 65'000;
    std::int32_t i32 = -2'000'000'000;
    std::uint32_t u32 = 4'000'000'000U;
    std::int64_t i64 = -9'000'000'000'000'000'000LL;
    std::uint64_t u64 = 0xfedc'ba98'7654'3210ULL;

    std::int_least16_t least16 = -12'345;
    std::uint_least32_t least32 = 3'000'000'000U;
    std::int_fast16_t fast16 = -23'456;
    std::uint_fast32_t fast32 = 0xdead'beefU;
    std::intptr_t intptr = -1;
    std::uintptr_t uintptr = 0;
    std::intmax_t intmax = INTMAX_MIN + 123;
    std::uintmax_t uintmax = UINTMAX_MAX - 123;

    signed char signed_char = -42;
    unsigned char unsigned_char = 0xe1;
    short signed_short = -30'000;
    unsigned short unsigned_short = 60'000;
    int signed_int = -123'456'789;
    unsigned int unsigned_int = 3'456'789'012U;
    long signed_long = -1'234'567L;
    unsigned long unsigned_long = 0xf000'0000UL;
    long long signed_long_long = -8'000'000'000LL;
    unsigned long long unsigned_long_long = 16'000'000'000ULL;

    bool boolean = true;
    char ascii = 'A';
    wchar_t wide = L'W';
    char8_t utf8 = u8'8';
    char16_t utf16 = u'λ';
    char32_t utf32 = U'🚀';

#if defined(__SIZEOF_INT128__)
    __int128 signed_128 = -(static_cast<__int128>(1) << 100);
    unsigned __int128 unsigned_128 = (static_cast<unsigned __int128>(1) << 127) + 123;
#endif
};

enum class PacketKind : std::uint16_t {
    control = 0x10,
    payload = 0x20,
    shutdown = 0xff,
};

struct Flags {
    unsigned ready : 1 = 1;
    unsigned error : 1 = 0;
    unsigned priority : 3 = 5;
    unsigned reserved : 27 = 0;
};

union NumberBits {
    double floating;
    std::uint64_t bits;
};

struct Payload {
    std::int32_t sequence = 7;
    std::array<std::uint16_t, 5> samples{10, 20, 30, 40, 50};
    std::string label = "fgdb nested value";
    Flags flags{};
};

struct Node {
    std::uint32_t id = 0;
    Payload payload{};
    Node* next = nullptr;
};

using FlexibleValue = std::variant<std::int64_t, std::string, std::vector<int>>;

struct DebugState {
    std::string name = "cpp-debug-target";
    std::vector<int> numbers{9, 1, 8, 2, 7, 3, 6, 4, 5};
    std::map<std::string, std::uint64_t> counters{{"frames", 12}, {"packets", 0x1234}};
    std::optional<Payload> optional_payload = Payload{};
    FlexibleValue flexible = std::vector<int>{11, 22, 33};
    std::unique_ptr<Payload> owned_payload = std::make_unique<Payload>();
    std::array<Node, 2> nodes{};
    PacketKind kind = PacketKind::payload;
    NumberBits number{.bits = 0x4009'21fb'5444'2d18ULL};
};

struct WorkerGate {
    std::mutex mutex;
    std::condition_variable changed;
    bool started = false;
    bool release = false;
    std::uint64_t result = 0;
};

extern "C" void signal_handler(int signal_number) {
    signal_seen = signal_number;
}

FGDB_NOINLINE std::uint64_t mix_value(std::uint64_t value, std::uint32_t round) {
    value ^= value >> 29;
    value *= 0x9e37'79b9'7f4a'7c15ULL;
    return std::rotl(value, static_cast<int>(round & 63U));
}

FGDB_NOINLINE void update_watch_value(int iteration) {
    // Set a conditional breakpoint here with: break update_watch_value if iteration == 5
    watched_value = mix_value(watched_value, static_cast<std::uint32_t>(iteration));
}

FGDB_NOINLINE void worker_body(WorkerGate& gate, const std::vector<int>& input) {
    std::unique_lock lock(gate.mutex);
    gate.started = true;
    gate.changed.notify_all();
    gate.changed.wait(lock, [&gate] { return gate.release; });
    lock.unlock();

    gate.result = static_cast<std::uint64_t>(std::accumulate(input.begin(), input.end(), 0));
}

FGDB_NOINLINE void debugger_checkpoint(
    const IntegerSamples& integers,
    DebugState& state,
    void* large_allocation
) {
    // Break here by function name. The arguments and nested state are intentionally inspectable.
    const auto allocation_address = reinterpret_cast<std::uintptr_t>(large_allocation);
    const auto combined = integers.u64 ^ state.counters.at("packets") ^ allocation_address;
    std::string summary = state.name + ": paused at debugger_checkpoint";
    std::printf("%s (combined=%#llx)\n", summary.c_str(), static_cast<unsigned long long>(combined));
}

}  // namespace

int main(int argc, char** argv) {
    std::signal(SIGUSR1, signal_handler);

    IntegerSamples integers{};
    DebugState state{};
    state.nodes[0].id = 1;
    state.nodes[1].id = 2;
    state.nodes[0].next = &state.nodes[1];
    state.nodes[1].next = &state.nodes[0];  // Intentional cycle for pointer-tree handling.
    integers.uintptr = reinterpret_cast<std::uintptr_t>(&state);

    constexpr std::size_t allocation_size = 1'300'000;
    void* large_allocation = std::malloc(allocation_size);
    if (large_allocation == nullptr) {
        std::fputs("malloc failed\n", stderr);
        return 1;
    }
    std::memset(large_allocation, 0x41, allocation_size);

    WorkerGate gate{};
    std::thread worker(worker_body, std::ref(gate), std::cref(state.numbers));
    {
        std::unique_lock lock(gate.mutex);
        gate.changed.wait(lock, [&gate] { return gate.started; });
    }

    std::sort(state.numbers.begin(), state.numbers.end());
    debugger_checkpoint(integers, state, large_allocation);  // Primary breakpoint.

    for (int iteration = 0; iteration < 8; ++iteration) {
        update_watch_value(iteration);  // Watch `watched_value`, or use the condition above.
    }

    const bool suppress_signal = argc > 1 && std::strcmp(argv[1], "--no-signal") == 0;
    if (!suppress_signal) {
        std::raise(SIGUSR1);  // Exercise fgdb's Signals view, then continue into the handler.
    }

    try {
        throw std::runtime_error("intentional debugger test exception");
    } catch (const std::exception& error) {
        state.name = error.what();
    }

    {
        std::lock_guard lock(gate.mutex);
        gate.release = true;
    }
    gate.changed.notify_all();
    worker.join();

    std::free(large_allocation);  // Step into this call to reproduce the large-free workload.
    std::printf(
        "done: watched=%#llx worker=%llu signal=%d\n",
        static_cast<unsigned long long>(watched_value),
        static_cast<unsigned long long>(gate.result),
        static_cast<int>(signal_seen)
    );
    return 0;
}
