"""One bounded, non-interactive operation on an explicitly identified module."""

import contextlib
import gdb
import json
import os


def identity(obj):
    # Object addresses can be reused after unload or exec. Tokens cannot.
    token = getattr(obj, "_fgdb_symbol_resolution_identity", None)

    if token is None:
        token = getattr(gdb, "_fgdb_symbol_resolution_sequence", 0) + 1
        gdb._fgdb_symbol_resolution_sequence = token
        obj._fgdb_symbol_resolution_identity = token

    return token


def track_program_instances():
    if hasattr(gdb, "_fgdb_symbol_resolution_clear_handler"):
        return

    def cleared(event):
        space = event.progspace

        if hasattr(space, "_fgdb_symbol_resolution_identity"):
            del space._fgdb_symbol_resolution_identity

    gdb.events.clear_objfiles.connect(cleared)
    gdb._fgdb_symbol_resolution_clear_handler = cleared


def parameter(name, fallback="unavailable"):
    try:
        return gdb.parameter(name)
    except gdb.error:
        return fallback


def emit(values):
    def encode(value):
        if isinstance(value, list):
            return "[" + ",".join(encode(item) for item in value) + "]"
        return json.dumps(str(value), ensure_ascii=False)

    record = "FGDB_SYMBOLS^done," + ",".join(
        name + "=" + encode(value) for name, value in values.items()
    )

    if len(record) > 65536:
        raise gdb.GdbError("Symbol metadata exceeded its output budget")

    gdb.write(record + "\n")


def configuration():
    directories = str(parameter("debug-file-directory", "")).split(os.pathsep)
    directories = list(dict.fromkeys(os.path.abspath(os.path.expanduser(directory))
                                    for directory in extra_directories + directories if directory))

    if len(directories) > 64:
        raise gdb.GdbError("Too many debug-file search directories")

    cache = os.environ.get("DEBUGINFOD_CACHE_PATH")

    if cache is not None:
        caches = [cache]
    else:
        caches = []
        xdg = os.environ.get("XDG_CACHE_HOME")
        home = os.environ.get("HOME")

        if xdg:
            caches.append(os.path.join(xdg, "debuginfod_client"))
        if home:
            caches += [os.path.join(home, ".cache/debuginfod_client"),
                       os.path.join(home, ".debuginfod_client_cache")]

    return {
        "debuginfod": parameter("debuginfod enabled"),
        "urls": parameter("debuginfod urls", os.environ.get("DEBUGINFOD_URLS", "")),
        "auto-solib": int(bool(parameter("auto-solib-add", False))),
        "directories": directories,
        "caches": [os.path.abspath(path) for path in caches if path],
        "cache-override": cache or "",
    }


def library():
    if not hasattr(gdb, "execute_mi"):
        raise gdb.GdbError("Verified module resolution requires GDB's Python MI interface")

    result = gdb.execute_mi("-file-list-shared-libraries", "--thread-group", inferior_id, pattern)
    libraries = result.get("shared-libraries", [])
    matches = [item for item in libraries if item.get("target-name") == target]

    if len(matches) != 1:
        raise gdb.GdbError("The selected module is no longer uniquely mapped")

    item = matches[0]
    ranges = item.get("ranges", [])
    first = ranges[0] if ranges else {}

    for name, expected in (("from", mapping_from), ("to", mapping_to)):
        actual = first.get(name, "")

        if actual != expected:
            raise gdb.GdbError("The selected module mapping changed")

    return item


def object_file(space, item):
    objects = space.objfiles()

    if len(objects) > 20000:
        raise gdb.GdbError("Too many symbol objects to resolve this module safely")

    names = {target, item.get("host-name", "")}
    matches = [obj for obj in objects if obj.is_valid() and obj.owner is None
               and (obj.filename in names or getattr(obj, "username", None) in names)]

    if len(matches) > 1:
        raise gdb.GdbError("More than one symbol object matches the selected module")

    return matches[0] if matches else None


def run():
    values = configuration()

    if action == "configuration":
        emit(values)
        return

    inferior = gdb.selected_inferior()
    space = inferior.progspace
    track_program_instances()

    if "i" + str(inferior.num) != inferior_id:
        raise gdb.GdbError("The selected inferior changed")
    if any(thread.is_running() for thread in inferior.threads()):
        raise gdb.GdbError("Pause all threads before loading debug information")
    if expected_space and (identity(space) != expected_space or inferior.pid != expected_pid):
        raise gdb.GdbError("The module belongs to a different program instance")

    item = library()
    obj = object_file(space, item)

    if expected_object and (obj is None or identity(obj) != expected_object):
        raise gdb.GdbError("The module's symbol object changed")
    if expected_build_id and obj is not None and obj.build_id != expected_build_id:
        raise gdb.GdbError("The loaded module's build ID changed")

    with contextlib.ExitStack() as scope:
        # Network access is owned by fgdb's cancellable download stage.
        if values["debuginfod"] != "unavailable":
            scope.enter_context(gdb.with_parameter("debuginfod enabled", "off"))
        scope.enter_context(gdb.with_parameter("may-call-functions", False))
        scope.enter_context(gdb.with_parameter("debug-file-directory", os.pathsep.join(values["directories"])))

        if action == "load":
            # sharedlibrary consumes a raw regex, not shell-quoted words.
            gdb.execute("sharedlibrary " + pattern, from_tty=False)
        elif action == "attach":
            if obj is None or not hasattr(obj, "add_separate_debug_file"):
                raise gdb.GdbError("This GDB cannot attach a separate debug file to this module")

            stat = os.stat(debug_file)
            actual = ":".join(str(value) for value in
                              (stat.st_dev, stat.st_ino, stat.st_size, stat.st_mtime_ns))

            if actual != debug_stamp:
                raise gdb.GdbError("The debug file changed after validation")

            obj.add_separate_debug_file(debug_file)

    item = library()
    obj = object_file(space, item)
    debug_files = [entry.filename for entry in space.objfiles()
                   if obj is not None and entry.is_valid() and entry.owner == obj]

    if len(debug_files) > 32:
        raise gdb.GdbError("Too many separate debug files for this module")

    values.update({
        "programspace": identity(space), "pid": inferior.pid,
        "object": identity(obj) if obj is not None else 0,
        "loaded": int(str(item.get("symbols-loaded", "0")) == "1"),
        "build-id": (obj.build_id or "") if obj is not None else "",
        "filename": obj.filename if obj is not None else item.get("host-name", target),
        "debug-files": debug_files,
    })
    emit(values)


run()
