
def inspect(reader):
    try:
        context = reader.symbol("__malloc_context")
    except UnsupportedLayout:
        return inspect_legacy(reader)

    active = reader.field(context, "active")
    usage = reader.field(context, "usage_by_class")
    classes = reader.array(active, 48)
    if len(classes) != 48 or len(reader.array(usage, 48)) != 48:
        raise UnsupportedLayout("Unknown musl mallocng size-class layout")
    reader.row("Runtime", "musl mallocng", "48 size classes", "Read-only metadata",
               "Only active group rings are traversed. Direct mappings are not an allocation census")
    reader.counters(context, "mallocng context", (
        ("Initialized", ("init_done",), ""),
        ("Available metadata", ("avail_meta_count",), "slots"),
        ("Available metadata areas", ("avail_meta_area_count",), "areas"),
        ("Mapping sequence", ("mmap_counter",), ""),
        ("Page size", ("pagesize",), "bytes"),
    ))
    groups = 0
    for index in classes:
        class_usage = reader.scalar(usage[index])
        pointer = active[index]
        if not reader.scalar(pointer) and not class_usage:
            continue
        reader.row("Size class", str(index), str(class_usage) + " slots in groups",
                   "Active ring" if reader.scalar(pointer) else "No active group")
        for address, meta in reader.walk(pointer, "next", circular=True):
            last = reader.number(meta, "last_idx")
            sizeclass = reader.number(meta, "sizeclass")
            available = reader.number(meta, "avail_mask") & 0xffffffff
            freed = reader.number(meta, "freed_mask") & 0xffffffff
            if last < 0 or last > 31 or sizeclass != index:
                raise UnsupportedLayout("Inconsistent musl group size class or slot count")
            mask = (1 << (last + 1)) - 1
            if (available | freed) & ~mask or available & freed:
                raise UnsupportedLayout("Inconsistent musl group slot masks")
            mem = reader.field(meta, "mem")
            group = reader.dereference(mem)
            if reader.number(group, "meta") != address:
                raise UnsupportedLayout("musl group metadata backlink does not match")
            available_count = available.bit_count()
            freed_count = freed.bit_count()
            occupied = last + 1 - available_count - freed_count
            reader.row("Group", hex(reader.scalar(mem)), str(occupied) + " occupied slots",
                       "class " + str(index), str(last + 1) + " slots / "
                       + str(available_count) + " available / " + str(freed_count) + " freed")
            groups += 1
    return str(groups) + " active mallocng groups. Slot state can be transitional inside malloc or free"


def inspect_legacy(reader):
    state = reader.symbol("mal")
    bins = reader.field(state, "bins")
    if len(reader.array(bins, 64)) != 64:
        raise UnsupportedLayout("Unknown musl legacy bin layout")
    bitmap = reader.number(state, "binmap")
    reader.row("Runtime", "musl legacy malloc", "64 bins", "Read-only metadata",
               "Bin heads and tails only. Private chunk offsets are not inferred")
    count = 0
    for index in range(64):
        if not bitmap & (1 << index):
            continue
        head = reader.number(bins[index], "head")
        tail = reader.number(bins[index], "tail")
        reader.row("Bin", str(index), "head " + hex(head), "Nonempty",
                   "tail " + hex(tail))
        count += 1
    return str(count) + " nonempty legacy musl bins. Bin heads and tails only"
