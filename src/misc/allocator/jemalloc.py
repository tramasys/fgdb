
def inspect(reader):
    arenas = reader.symbol("je_arenas", "arenas", "_rjem_je_arenas", "_rjem_arenas")
    indices = reader.array(arenas, 8192)
    total = reader.scalar(reader.symbol("je_narenas_total", "narenas_total",
                                        "_rjem_je_narenas_total", "_rjem_narenas_total"))
    sized = arenas.type.sizeof > 0
    if total < 0 or total > 8192 or (sized and total > len(indices)):
        raise UnsupportedLayout("jemalloc arena count exceeds the supported table bounds")

    # An external DWARF declaration can leave the array unsized. Its element
    # type and the maintained arena count still describe a bounded table.
    # Read at most 256 typed entries and verify each arena's own index.
    entries = arenas.address.cast(arenas.type.strip_typedefs().target().pointer())

    # The table contains atomic void pointers in jemalloc 5.x. Resolve the arena
    # type from that table's debug-info object, not from an unrelated allocator.
    objfile = arenas.type.objfile
    if objfile is None:
        raise UnsupportedLayout("jemalloc arena type has no debug-info owner")
    arena_type = None
    for name in ("arena_t", "struct arena_s"):
        try:
            candidate = gdb.lookup_type(name, reader.block)
            if candidate.objfile == objfile:
                arena_type = candidate
                break
        except gdb.error:
            pass
    if arena_type is None:
        raise UnsupportedLayout("Matching jemalloc arena debug types are unavailable")

    try:
        stats_enabled = bool(reader.scalar(reader.symbol("config_stats")))
    except UnsupportedLayout:
        stats_enabled = False

    reader.row("Runtime", "jemalloc", str(total) + " arena slots", "Read-only metadata",
               "No mallctl or epoch update is called. " + ("Statistics enabled" if stats_enabled
               else "Statistics disabled or unavailable, showing maintained arena metadata"))
    initialized = 0
    for index in range(min(total, 256)):
        address = reader.scalar(entries[index])
        if not address:
            continue
        arena = gdb.Value(address).cast(arena_type.pointer()).dereference()
        actual_index = reader.number(arena, "ind")
        if actual_index != index:
            raise UnsupportedLayout("jemalloc arena index does not match its table entry")
        nthreads = reader.field(arena, "nthreads")
        if len(reader.array(nthreads, 2)) != 2:
            raise UnsupportedLayout("Unknown jemalloc thread-count layout")
        threads = reader.scalar(nthreads[0])
        internal = reader.scalar(nthreads[1])
        location = hex(address)
        reader.row("Arena " + str(index), location,
                   str(threads) + " app thread" + ("s" if threads != 1 else ""),
                   "Initialized", str(internal) + " internal thread assignments")
        reader.counters(arena, location, (
            ("Active", ("pa_shard.nactive", "nactive"), "pages"),
            ("Dirty", ("pa_shard.pac.ecache_dirty.eset.npages", "ecache_dirty.eset.npages"), "pages"),
            ("Muzzy", ("pa_shard.pac.ecache_muzzy.eset.npages", "ecache_muzzy.eset.npages"), "pages"),
            ("Retained", ("pa_shard.pac.ecache_retained.eset.npages", "ecache_retained.eset.npages"), "pages"),
            ("Guarded dirty", ("pa_shard.pac.ecache_dirty.guarded_eset.npages",), "pages"),
            ("Guarded muzzy", ("pa_shard.pac.ecache_muzzy.guarded_eset.npages",), "pages"),
            ("Guarded retained", ("pa_shard.pac.ecache_retained.guarded_eset.npages",), "pages"),
        ))
        if stats_enabled:
            reader.counters(arena, location, (
                ("PAC mapped", ("pa_shard.stats.pac_stats.pac_mapped",), "bytes"),
                ("Internal allocations", ("stats.internal",), "bytes"),
            ))
        initialized += 1
    if total > 256:
        raise ReadBudget("Showing the first 256 jemalloc arena slots")
    return str(initialized) + " initialized arenas. Counters are not a live allocation census"
