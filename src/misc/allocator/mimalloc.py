
def inspect(reader):
    # v2 stores page queues on mi_heap_t. v3 also has layouts which separate
    # process heaps from their thread-local mi_theap_t page queues.
    root = reader.symbol("mi_process_heap_main", "_mi_heap_main", "heap_main", "_mi_theap_main", "theap_main")
    if root.type.strip_typedefs().code == gdb.TYPE_CODE_PTR:
        root = reader.dereference(root)
    fields = reader.fields(root)
    heaps = []
    if "theaps" in fields:
        for address, heap in reader.walk(reader.field(root, "theaps"), "hnext"):
            heaps.append((address, heap))
        scope = "main process heap's thread heaps"
    elif "pages" in fields:
        # Read the selected thread's default heap where TLS is available.
        try:
            default = reader.symbol("_mi_heap_default", "_mi_theap_default")
            root = reader.dereference(default)
            scope = "selected thread's default heap"
        except UnsupportedLayout:
            scope = "main thread heap"
        heaps.append((reader.scalar(root.address), root))
    else:
        raise UnsupportedLayout("Unknown mimalloc heap layout")

    reader.row("Runtime", "mimalloc", str(len(heaps)) + " heap" + ("s" if len(heaps) != 1 else ""),
               "Read-only metadata", "Scope: " + scope
               + ". Remote frees may still be included in used counts. Special queues may mix block sizes")
    page_total = 0
    for address, heap in heaps:
        queues = reader.field(heap, "pages")
        indices = reader.array(queues, 256)
        location = hex(address)
        page_count = reader.number(heap, "page_count")
        reader.row("Heap", location, str(page_count) + " page" + ("s" if page_count != 1 else ""),
                   "Thread-local page queues")
        reader.counters(heap, location, (
            ("Owner thread key", ("thread_id", "tld.thread_id"), ""),
            ("Full pages", ("pages_full_size",), "bytes"),
            ("Allocation slow paths", ("generic_count",), ""),
        ))
        for index in indices:
            queue = queues[index]
            first = reader.field(queue, "first")
            if not reader.scalar(first):
                continue
            pages = used = capacity = reserved = 0
            previous = 0
            for page_address, page in reader.walk(first, "next"):
                if reader.number(page, "prev") != previous:
                    raise UnsupportedLayout("mimalloc page queue backlink does not match")
                page_used = reader.number(page, "used")
                page_capacity = reader.number(page, "capacity")
                page_reserved = reader.number(page, "reserved")
                if not 0 <= page_used <= page_capacity <= page_reserved:
                    raise UnsupportedLayout("Inconsistent mimalloc page block counts")
                used += page_used
                capacity += page_capacity
                reserved += page_reserved
                pages += 1
                previous = page_address
            if reader.number(queue, "last") != previous:
                raise UnsupportedLayout("mimalloc page queue tail does not match")
            block_size = reader.number(queue, "block_size")
            reader.row("Page queue", location + " / " + str(index),
                       str(pages) + " page" + ("s" if pages != 1 else ""),
                       str(used) + " used / " + str(capacity) + " committed blocks",
                       "block size " + str(block_size) + " bytes / " + str(reserved) + " reserved blocks")
            page_total += pages
    return str(page_total) + " queued pages across " + str(len(heaps)) + " inspected heaps"
