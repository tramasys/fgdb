#define _GNU_SOURCE

#include <errno.h>
#include <netinet/in.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#if defined(__GNUC__)
#define FGDB_NOINLINE __attribute__((noinline))
#else
#define FGDB_NOINLINE
#endif

struct SocketPair {
    const char *name;
    int listener;
    int client;
    int server;
};

struct NetworkFixture {
    struct SocketPair pairs[5];
    int shared_listener;
    int epoll_fd;
    int ready_count;
    struct epoll_event events[5];
};

static const char payload[] = "fgdb network payload";

static int checked(int result, const char *operation) {
    if (result < 0) {
        perror(operation);
        exit(EXIT_FAILURE);
    }

    return result;
}

static void expect(int condition, const char *message) {
    if (!condition) {
        fprintf(stderr, "network check failed: %s\n", message);
        exit(EXIT_FAILURE);
    }
}

static struct SocketPair socket_pair(int family, int type, const char *name) {
    struct SocketPair pair = {.name = name, .listener = -1, .client = -1, .server = -1};

    union {
        struct sockaddr address;
        struct sockaddr_in ipv4;
        struct sockaddr_in6 ipv6;
        struct sockaddr_un local;
    } address = {0};

    socklen_t length;

    if (family == AF_INET) {
        address.ipv4.sin_family = AF_INET;
        address.ipv4.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        length = sizeof(address.ipv4);
    } else if (family == AF_INET6) {
        address.ipv6.sin6_family = AF_INET6;
        address.ipv6.sin6_addr = in6addr_loopback;
        length = sizeof(address.ipv6);
    } else {
        address.local.sun_family = AF_UNIX;
        const int size = snprintf(
            address.local.sun_path + 1,
            sizeof(address.local.sun_path) - 1,
            "fgdb-network-%ld",
            (long)getpid()
        );

        expect(size > 0 && (size_t)size < sizeof(address.local.sun_path) - 1, "Unix name fits");
        length = (socklen_t)(offsetof(struct sockaddr_un, sun_path) + 1 + (size_t)size);
    }

    const int bound = socket(family, type | SOCK_CLOEXEC, 0);

    if (bound < 0 && family == AF_INET6 && (errno == EAFNOSUPPORT || errno == EPROTONOSUPPORT)) {
        printf("%s skipped: IPv6 unavailable\n", name);
        return pair;
    }

    checked(bound, "socket");

    if (family == AF_INET && type == SOCK_STREAM) {
        const int reuse_port = 1;
        checked(setsockopt(bound, SOL_SOCKET, SO_REUSEPORT, &reuse_port, sizeof(reuse_port)), "SO_REUSEPORT");
    }

    if (family == AF_INET6) {
        const int only_ipv6 = 1;
        checked(setsockopt(bound, IPPROTO_IPV6, IPV6_V6ONLY, &only_ipv6, sizeof(only_ipv6)), "setsockopt");
    }

    const int bound_result = bind(bound, &address.address, length);

    if (bound_result < 0 && family == AF_INET6 && errno == EADDRNOTAVAIL) {
        checked(close(bound), "close");
        printf("%s skipped: IPv6 loopback unavailable\n", name);
        return pair;
    }

    checked(bound_result, "bind");
    checked(getsockname(bound, &address.address, &length), "getsockname");

    if (type == SOCK_STREAM) {
        checked(listen(bound, 1), "listen");
        pair.listener = bound;
    }

    pair.client = checked(socket(family, type | SOCK_CLOEXEC, 0), "socket");
    checked(connect(pair.client, &address.address, length), "connect");

    pair.server = type == SOCK_STREAM
        ? checked(accept4(bound, NULL, NULL, SOCK_CLOEXEC), "accept4")
        : bound;

    return pair;
}

static int shared_listener(int original) {
    struct sockaddr_in address;
    socklen_t length = sizeof(address);
    checked(getsockname(original, (struct sockaddr *)&address, &length), "getsockname");
    const int listener = checked(socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0), "socket");
    const int reuse_port = 1;
    checked(setsockopt(listener, SOL_SOCKET, SO_REUSEPORT, &reuse_port, sizeof(reuse_port)), "SO_REUSEPORT");
    checked(bind(listener, (struct sockaddr *)&address, length), "shared bind");
    checked(listen(listener, 7), "shared listen");
    return listener;
}

static void send_payload(int descriptor) {
    size_t offset = 0;

    while (offset < sizeof(payload)) {
        const ssize_t sent = send(descriptor, payload + offset, sizeof(payload) - offset, MSG_NOSIGNAL);

        if (sent < 0 && errno == EINTR) {
            continue;
        }

        checked((int)sent, "send");
        expect(sent > 0, "send made progress");
        offset += (size_t)sent;
    }
}

static void receive_payload(int descriptor, int flags) {
    char received[sizeof(payload)];
    ssize_t count;

    do {
        count = recv(descriptor, received, sizeof(received), flags | MSG_WAITALL);
    } while (count < 0 && errno == EINTR);

    checked((int)count, "recv");
    expect(count == sizeof(payload), "complete payload received");
    expect(memcmp(received, payload, sizeof(payload)) == 0, "payload unchanged");
}

static void check_readiness(struct NetworkFixture *fixture, int expected) {
    do {
        fixture->ready_count = epoll_wait(fixture->epoll_fd, fixture->events, 5, 0);
    } while (fixture->ready_count < 0 && errno == EINTR);

    checked(fixture->ready_count, "epoll_wait");
    expect(fixture->ready_count == expected, "expected number of readable sockets");
}

FGDB_NOINLINE void c_network_checkpoint(const struct NetworkFixture *fixture, const char *phase) {
    printf("network checkpoint: %s pid=%ld epoll=%d ready=%d\n",
        phase, (long)getpid(), fixture->epoll_fd, fixture->ready_count);
    printf("  SO_REUSEPORT listener=%d\n", fixture->shared_listener);

    for (size_t index = 0; index < 5; ++index) {
        const struct SocketPair *pair = &fixture->pairs[index];

        printf("  %s: listener=%d client=%d server=%d\n",
            pair->name, pair->listener, pair->client, pair->server);
    }
}

int main(void) {
    struct NetworkFixture fixture = {
        .pairs = {
            socket_pair(AF_INET, SOCK_STREAM, "TCP"),
            socket_pair(AF_INET, SOCK_DGRAM, "UDP"),
            socket_pair(AF_INET6, SOCK_STREAM, "TCP6"),
            socket_pair(AF_INET6, SOCK_DGRAM, "UDP6"),
            socket_pair(AF_UNIX, SOCK_STREAM, "UNIX"),
        },
        .epoll_fd = checked(epoll_create1(EPOLL_CLOEXEC), "epoll_create1"),
    };

    // Add the second listener after accept so it cannot steal the fixture's connection.
    fixture.shared_listener = shared_listener(fixture.pairs[0].listener);
    int active = 0;
    int streams = 0;

    for (size_t index = 0; index < 5; ++index) {
        const struct SocketPair *pair = &fixture.pairs[index];

        if (pair->server < 0) {
            continue;
        }

        struct epoll_event event = {.events = EPOLLIN | EPOLLRDHUP, .data.fd = pair->server};
        checked(epoll_ctl(fixture.epoll_fd, EPOLL_CTL_ADD, pair->server, &event), "epoll_ctl");
        send_payload(pair->client);

        // Confirm delivery without draining the receive queue before the checkpoint.
        receive_payload(pair->server, MSG_PEEK);
        ++active;
        streams += pair->listener >= 0;
    }

    check_readiness(&fixture, active);
    c_network_checkpoint(&fixture, "queued");

    for (size_t index = 0; index < 5; ++index) {
        if (fixture.pairs[index].server >= 0) {
            receive_payload(fixture.pairs[index].server, 0);
        }
    }

    check_readiness(&fixture, 0);
    c_network_checkpoint(&fixture, "drained");

    for (size_t index = 0; index < 5; ++index) {
        const struct SocketPair *pair = &fixture.pairs[index];

        if (pair->listener < 0) {
            continue;
        }

        checked(shutdown(pair->client, SHUT_WR), "shutdown");
        char byte;
        ssize_t count;

        do {
            count = recv(pair->server, &byte, 1, 0);
        } while (count < 0 && errno == EINTR);

        checked((int)count, "recv EOF");
        expect(count == 0, "half-closed stream reached EOF");

        // The opposite direction still works after shutdown(SHUT_WR).
        send_payload(pair->server);
        receive_payload(pair->client, 0);
    }

    check_readiness(&fixture, streams);
    c_network_checkpoint(&fixture, "half-closed");

    for (size_t index = 0; index < 5; ++index) {
        struct SocketPair *pair = &fixture.pairs[index];
        const int descriptors[] = {pair->listener, pair->client, pair->server};

        for (size_t descriptor = 0; descriptor < 3; ++descriptor) {
            if (descriptors[descriptor] >= 0) {
                checked(close(descriptors[descriptor]), "close");
            }
        }

        pair->listener = -1;
        pair->client = -1;
        pair->server = -1;
    }

    check_readiness(&fixture, 0);
    checked(close(fixture.shared_listener), "close shared listener");
    fixture.shared_listener = -1;
    checked(close(fixture.epoll_fd), "close epoll");
    fixture.epoll_fd = -1;
    c_network_checkpoint(&fixture, "closed");
    puts("network checks passed");
    return EXIT_SUCCESS;
}
