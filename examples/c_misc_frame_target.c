#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>

#if defined(__GNUC__)
#define FGDB_NOINLINE __attribute__((noinline))
#else
#define FGDB_NOINLINE
#endif

struct FramePacket {
    uint64_t sequence;
    uint32_t flags;
    double ratio;
    const char *label;
};

static volatile uint64_t frame_result;

FGDB_NOINLINE void c_misc_frame_checkpoint(
    const struct FramePacket *packet,
    uint64_t accumulated,
    unsigned depth
) {
    printf(
        "frame checkpoint: depth=%u sequence=%#" PRIx64
        " accumulated=%#" PRIx64 " ratio=%.3f label=%s\n",
        depth,
        packet->sequence,
        accumulated,
        packet->ratio,
        packet->label
    );
}

FGDB_NOINLINE static uint64_t frame_leaf(
    const struct FramePacket *packet,
    uint64_t accumulated,
    unsigned depth
) {
    c_misc_frame_checkpoint(packet, accumulated, depth);
    return accumulated ^ packet->sequence ^ (uint64_t)packet->flags;
}

FGDB_NOINLINE static uint64_t frame_recursive(
    const struct FramePacket *packet,
    uint64_t accumulated,
    unsigned depth
) {
    const uint64_t local_cookie = UINT64_C(0x1111000000000000) | depth;
    if (depth == 0) {
        return frame_leaf(packet, accumulated ^ local_cookie, depth);
    }
    return frame_recursive(packet, accumulated + local_cookie, depth - 1) + depth;
}

FGDB_NOINLINE static uint64_t frame_middle(
    const struct FramePacket *packet,
    uint64_t seed
) {
    const uint64_t transformed = (seed << 7U) ^ UINT64_C(0xa5a55a5adeadbeef);
    return frame_recursive(packet, transformed, 3);
}

FGDB_NOINLINE static uint64_t frame_outer(const struct FramePacket *packet) {
    return frame_middle(packet, packet->sequence + packet->flags);
}

int main(void) {
    const struct FramePacket packet = {
        .sequence = UINT64_C(0x0123456789abcdef),
        .flags = UINT32_C(0x5a17c0de),
        .ratio = 3.141592653589793,
        .label = "nested-frame-fixture",
    };
    frame_result = frame_outer(&packet);
    printf("frame result=%#" PRIx64 "\n", frame_result);
    return frame_result == 0;
}
