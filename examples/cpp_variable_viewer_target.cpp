#include <array>
#include <cstdint>
#include <cstdio>
#include <string>
#include <vector>

#if defined(_MSC_VER)
#define FGDB_NOINLINE __declspec(noinline)
#else
#define FGDB_NOINLINE __attribute__((noinline))
#endif

struct ViewerNode {
    std::uint32_t id;
    const char* label;
    std::array<int, 3> samples;
    ViewerNode* next;
};

FGDB_NOINLINE void variable_viewer_checkpoint(
    int (&native_values)[10],
    std::array<int, 8>& fixed_values,
    std::vector<std::string>& words,
    ViewerNode* linear_head,
    ViewerNode* cycle_head
) {
    // Break on this function. Every argument is intentionally kept live so
    // its locals/arguments context menu and dedicated viewer can be exercised.
    const auto checksum = native_values[3]
        + fixed_values[4]
        + static_cast<int>(words.size())
        + static_cast<int>(linear_head->id)
        + static_cast<int>(cycle_head->id);
    std::printf(
        "variable viewer checkpoint: checksum=%d linear=%p cycle=%p\n",
        checksum,
        static_cast<void*>(linear_head),
        static_cast<void*>(cycle_head)
    );
}

int main() {
    int native_values[10] = {3, 1, 4, 1, 5, 9, 2, 6, 5, 3};
    std::array<int, 8> fixed_values = {10, 20, 30, 40, 50, 60, 70, 80};
    std::vector<std::string> words = {
        "zero",
        "one",
        "two words",
        "three",
        "four",
    };

    std::array<ViewerNode, 4> linear_nodes = {{
        {1, "linear-a", {11, 12, 13}, nullptr},
        {2, "linear-b", {21, 22, 23}, nullptr},
        {3, "linear-c", {31, 32, 33}, nullptr},
        {4, "linear-d", {41, 42, 43}, nullptr},
    }};
    for (std::size_t index = 0; index + 1 < linear_nodes.size(); ++index) {
        linear_nodes[index].next = &linear_nodes[index + 1];
    }

    std::array<ViewerNode, 3> cycle_nodes = {{
        {101, "cycle-a", {101, 102, 103}, nullptr},
        {102, "cycle-b", {201, 202, 203}, nullptr},
        {103, "cycle-c", {301, 302, 303}, nullptr},
    }};
    for (std::size_t index = 0; index < cycle_nodes.size(); ++index) {
        cycle_nodes[index].next = &cycle_nodes[(index + 1) % cycle_nodes.size()];
    }

    variable_viewer_checkpoint(
        native_values,
        fixed_values,
        words,
        linear_nodes.data(),
        cycle_nodes.data()
    );
    return 0;
}
