import gdb
import time

deadline = time.monotonic() + 1.0
lines = ["FGDB_ALLOCATORS\t1"]
attempted = 0
for index, expression in enumerate(probes):
    if time.monotonic() > deadline:
        break
    attempted = index + 1
    try:
        value = gdb.parse_and_eval(expression)
        address = int(value)
        if not address:
            continue
        spelling = value.format_string(raw=True, symbols=True, address=True).lower()
        indirect = any(marker in spelling for marker in
                       ("@plt", ".plt>", "<plt", "@got", ".got>", "<got"))
        lines.append("S\t" + str(index) + "\t" + format(address, "x")
                     + "\t" + str(int(indirect)))
    except gdb.error:
        pass

lines.append("E\t" + str(attempted))

gdb.write("\n".join(lines) + "\n")
