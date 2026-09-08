
def inspect(reader):
    try:
        storage = reader.symbol("tcmalloc::Static::pageheap_")
    except UnsupportedLayout:
        return inspect_google(reader)

    # gperftools constructs PageHeap in static, aligned storage. Only accept
    # storage whose DWARF size can contain the matching PageHeap type.
    typ = gdb.lookup_type("tcmalloc::PageHeap", reader.block)
    if typ.objfile != storage.type.objfile or typ.sizeof > storage.type.sizeof:
        raise UnsupportedLayout("gperftools page heap storage does not match its debug type")
    memory = reader.field(storage, "memory", "bytes_")
    if memory.type.sizeof < typ.sizeof:
        raise UnsupportedLayout("gperftools page heap storage is too small")
    heap = memory.address.cast(typ.pointer()).dereference()
    stats = reader.field(heap, "stats_")
    reader.row("Runtime", "gperftools tcmalloc", "Page heap and thread caches",
               "Read-only metadata", "No MallocExtension methods or target functions are called")
    if not reader.counters(stats, "Page heap", (
        ("System", ("system_bytes",), "bytes"),
        ("Free mapped", ("free_bytes",), "bytes"),
        ("Unmapped", ("unmapped_bytes",), "bytes"),
        ("Committed", ("committed_bytes",), "bytes"),
        ("Scavenges", ("scavenge_count",), ""),
        ("Commits", ("commit_count",), ""),
        ("Decommits", ("decommit_count",), ""),
    )):
        raise UnsupportedLayout("Unknown gperftools page heap statistics layout")
    try:
        head = reader.symbol("tcmalloc::ThreadCache::thread_heaps_")
    except UnsupportedLayout:
        return "gperftools page heap counters. Thread cache symbols are unavailable"

    count = 0
    for address, cache in reader.walk(head, "next_"):
        size = reader.number(cache, "size_")
        maximum = reader.number(cache, "max_size_")
        reader.row("Thread cache", hex(address), str(size) + " bytes", "Cached free objects",
                   "limit " + str(maximum) + " bytes")
        count += 1
    return "gperftools page heap counters and " + str(count) + " thread caches"


def inspect_google(reader):
    state = reader.symbol("tcmalloc::tcmalloc_internal::tc_globals")
    reader.row("Runtime", "Google TCMalloc", "Global metadata", "Read-only metadata",
               "Google TCMalloc has a different layout from gperftools. Page heap offsets are not reused")
    count = reader.counters(state, "Global state", (
        ("Initialized", ("inited_",), ""),
        ("Per-CPU cache active", ("cpu_cache_active_",), ""),
    ))
    if not count:
        raise UnsupportedLayout("This Google TCMalloc layout has no supported typed counters")
    return "Google TCMalloc global counters. Per-CPU slab and huge-page layouts are not decoded"
