#include <stddef.h>
#include <stdint.h>
#include <stdio.h>

/* Break at linked_list_checkpoint, then open a Linked list viewer on a head.
 * linear_head crosses the default page boundary. Use prev on reverse_head.
 * custom_head needs tailward. cyclic_head and broken_head exercise recovery.
 */
struct Node {
    int value;
    const char *label;
    struct Node *next;
    struct Node *prev;
};

struct CustomNode {
    int value;
    struct CustomNode *tailward;
};

/* The viewer must not call this while resolving a path. */
int linked_list_probe(void) {
    return 42;
}

__attribute__((noinline))
void linked_list_checkpoint(struct Node *linear_head, struct Node *reverse_head,
                            struct Node *cyclic_head, struct Node *broken_head,
                            struct Node *empty_head, struct CustomNode *custom_head) {
    printf("linear=%d reverse=%d cycle=%d broken=%d empty=%p custom=%d\n",
           linear_head->value, reverse_head->value, cyclic_head->value,
           broken_head->value, (void *)empty_head, custom_head->value);
}

int main(void) {
    struct Node linear[300];
    struct Node cyclic[3];
    struct CustomNode custom[5];

    for (size_t i = 0; i < 300; ++i) {
        linear[i] = (struct Node){(int)i, "linear node",
            i + 1 < 300 ? &linear[i + 1] : NULL,
            i > 0 ? &linear[i - 1] : NULL};
    }

    for (size_t i = 0; i < 3; ++i) {
        cyclic[i] = (struct Node){(int)i + 1000, "cyclic node",
            &cyclic[(i + 1) % 3], &cyclic[(i + 2) % 3]};
    }

    for (size_t i = 0; i < 5; ++i) {
        custom[i] = (struct CustomNode){(int)i + 2000,
            i + 1 < 5 ? &custom[i + 1] : NULL};
    }

    /* Deliberately invalid link, never dereferenced by the fixture. */
    struct Node broken = {3000, "broken node", (struct Node *)(uintptr_t)1, NULL};
    linked_list_checkpoint(linear, &linear[299], cyclic, &broken, NULL, custom);
    return 0;
}
