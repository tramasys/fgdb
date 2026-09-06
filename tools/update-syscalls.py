#!/usr/bin/env python3
"""Regenerate the offline syscall catalog from a pinned Linux revision."""

import functools
import pathlib
import re
import time
import urllib.error
import urllib.request

REVISION = "9f0346dcbea363787186c94ef94dd01aaa215afa"
ROOT = pathlib.Path(__file__).resolve().parents[1]


@functools.cache
def linux_file(path):
    url = f"https://raw.githubusercontent.com/torvalds/linux/{REVISION}/{path}"
    for attempt in range(4):
        try:
            with urllib.request.urlopen(url, timeout=30) as response:
                return response.read().decode("utf-8")
        except urllib.error.HTTPError as error:
            if error.code not in {429, 503} or attempt == 3:
                raise
            time.sleep(2 ** attempt)


def table(path, abi, accepted, base=0):
    rows = []
    for line in linux_file(path).splitlines():
        fields = line.split("#", 1)[0].split()
        if len(fields) >= 3 and fields[0].isdigit() and fields[1] in accepted:
            rows.append((abi, int(fields[0]) + base, fields[2]))
    return rows


def arguments():
    source = re.sub(r"/\*.*?\*/", "", linux_file("include/linux/syscalls.h"), flags=re.S)
    signatures = {}
    for name, params in re.findall(r"asmlinkage\s+long\s+sys_(\w+)\s*\((.*?)\)\s*;", source, re.S):
        if params.strip() == "void":
            signatures.setdefault(name, set()).add("None")
            continue
        names = []
        for param in params.split(","):
            match = re.search(r"\b(\w+)\s*(?:\[.*?\])?\s*$", param)
            if not match or match[1] in {"int", "long", "char", "size_t", "unsigned", "void"}:
                names = []
                break
            names.append(match[1])
        if names:
            signatures.setdefault(name, set()).add(", ".join(names))
        else:
            signatures.setdefault(name, set()).add("-")
    # Conditional prototypes can use different argument layouts across ABIs.
    # Missing metadata is preferable to selecting an arbitrary prototype.
    return {name: next(iter(variants)) for name, variants in signatures.items() if len(variants) == 1}


def category(name):
    groups = {
        "Network": "socket socketpair bind connect listen accept accept4 shutdown sendto recvfrom sendmsg recvmsg sendmmsg recvmmsg getsockname getpeername setsockopt getsockopt socketcall",
        "Memory": "brk mmap mmap2 munmap mremap mprotect pkey_mprotect pkey_alloc pkey_free madvise process_madvise mincore mlock mlock2 munlock mlockall munlockall msync remap_file_pages membarrier memfd_create memfd_secret move_pages migrate_pages get_mempolicy set_mempolicy set_mempolicy_home_node mbind userfaultfd mseal map_shadow_stack",
        "Process": "clone clone3 fork vfork execve execveat exit exit_group wait4 waitid waitpid getpid getppid gettid set_tid_address set_robust_list get_robust_list rseq prctl arch_prctl ptrace process_vm_readv process_vm_writev unshare setns pidfd_open pidfd_getfd pidfd_send_signal pidfd_send_signal process_mrelease",
        "IPC": "pipe pipe2 eventfd eventfd2 futex futex_time64 futex_waitv futex_wait futex_wake futex_requeue shmget shmat shmdt shmctl semget semop semtimedop semctl msgget msgsnd msgrcv msgctl ipc",
        "Files": "read write readv writev pread64 pwrite64 preadv preadv2 pwritev pwritev2 open openat openat2 creat close close_range dup dup2 dup3 fcntl fcntl64 ioctl lseek _llseek fsync fdatasync sync syncfs sync_file_range getdents getdents64 readdir sendfile sendfile64 splice tee vmsplice copy_file_range truncate truncate64 ftruncate ftruncate64 fallocate readlink readlinkat link linkat unlink unlinkat rename renameat renameat2 symlink symlinkat mkdir mkdirat rmdir chdir fchdir getcwd access faccessat faccessat2 chroot fchroot pivot_root mount umount umount2 stat lstat fstat stat64 lstat64 fstat64 newfstatat fstatat64 statx statfs fstatfs statfs64 fstatfs64 chmod fchmod fchmodat fchmodat2 chown lchown fchown fchownat flock readahead fadvise64 fadvise64_64",
        "Polling": "poll ppoll ppoll_time64 select _newselect pselect6 pselect6_time64 epoll_create epoll_create1 epoll_ctl epoll_wait epoll_pwait epoll_pwait2",
        "Signals": "kill tkill tgkill signal sigaction sigreturn sigprocmask sigpending sigsuspend sigaltstack signalfd signalfd4 pause",
        "Time": "time times gettimeofday settimeofday nanosleep nanosleep_time64 alarm getitimer setitimer adjtimex",
    }
    for label, names in groups.items():
        if name in names.split():
            return label
    for prefix, label in [("rt_sig", "Signals"), ("clock_", "Time"), ("timer", "Time"), ("sched_", "Scheduling"), ("io_", "Async I/O"), ("mq_", "IPC"), ("inotify_", "Files"), ("fanotify_", "Files")]:
        if name.startswith(prefix):
            return label
    return "-"


def main():
    rows = table("arch/x86/entry/syscalls/syscall_64.tbl", "x86_64", {"common", "64"})
    rows += table("arch/x86/entry/syscalls/syscall_64.tbl", "x32", {"common", "x32"}, 0x40000000)
    rows += table("arch/x86/entry/syscalls/syscall_32.tbl", "i386", {"i386"})
    rows += table("scripts/syscall.tbl", "aarch64", {"common", "64", "renameat", "rlimit", "memfd_secret"})
    rows += table("arch/arm/tools/syscall.tbl", "arm", {"common", "eabi"})
    args = arguments()
    lines = [f"# Linux syscall ABI catalog at {REVISION}", "# Generated by tools/update-syscalls.py", "# ABI\tnumber\tname\tcategory\tlogical argument names"]
    keys = set()
    for abi, number, name in sorted(rows):
        assert (abi, number) not in keys, (abi, number)
        keys.add((abi, number))
        lines.append(f"{abi}\t{number}\t{name}\t{category(name)}\t{args.get(name, '-')}")
    (ROOT / "src/syscalls/catalog.tsv").write_text("\n".join(lines) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
