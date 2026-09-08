CC ?= cc
CXX ?= g++
RUSTC ?= rustc
GDB ?= gdb
FORTRAN ?= gfortran
ZIG ?= zig
ODIN ?= odin
MUSL_CC ?= musl-gcc

CFLAGS ?= -std=c17 -g3 -O0 -fno-omit-frame-pointer -fno-inline -Wall -Wextra -Wpedantic
CXXFLAGS ?= -std=c++20 -g3 -O0 -fno-omit-frame-pointer -fno-inline -Wall -Wextra -Wpedantic
RUSTFLAGS ?= --edition=2024 -C debuginfo=2 -C opt-level=0 -C force-frame-pointers=yes
CPPFLAGS ?=
LDFLAGS ?=

# Optional allocator fixtures can use custom debug builds without LD_PRELOAD.
# Put header flags in *_CFLAGS and library paths / runtime paths in *_LIBS.
JEMALLOC_CFLAGS ?=
JEMALLOC_LIBS ?= -ljemalloc
TCMALLOC_CFLAGS ?=
TCMALLOC_LIBS ?= -ltcmalloc
MIMALLOC_CFLAGS ?=
MIMALLOC_LIBS ?= -lmimalloc

TARGET_DIR := ../target/debug-fixtures
C_TARGETS := \
	$(TARGET_DIR)/c-simd-target \
	$(TARGET_DIR)/c-allocation-target \
	$(TARGET_DIR)/c-memory-target \
	$(TARGET_DIR)/c-page-target \
	$(TARGET_DIR)/c-misc-allocator-target \
	$(TARGET_DIR)/c-misc-auxv-target \
	$(TARGET_DIR)/c-misc-core-target \
	$(TARGET_DIR)/c-misc-frame-target \
	$(TARGET_DIR)/c-misc-locks-target \
	$(TARGET_DIR)/c-tls-target \
	$(TARGET_DIR)/c-thread-target \
	$(TARGET_DIR)/c-process-target
CXX_TARGETS := \
	$(TARGET_DIR)/cpp-debug-target \
	$(TARGET_DIR)/cpp-object-target \
	$(TARGET_DIR)/cpp-variable-viewer-target
RUST_TARGETS := \
	$(TARGET_DIR)/rust-debug-target \
	$(TARGET_DIR)/rust-variable-viewer-target
OPTIMIZED_TARGETS := \
	$(TARGET_DIR)/cpp-debug-target-o2 \
	$(TARGET_DIR)/rust-debug-target-o2
TARGETS := $(C_TARGETS) $(CXX_TARGETS) $(RUST_TARGETS) $(OPTIMIZED_TARGETS)
LANGUAGE_TARGETS := \
	$(TARGET_DIR)/fortran-variable-viewer-target \
	$(TARGET_DIR)/zig-variable-viewer-target \
	$(TARGET_DIR)/odin-variable-viewer-target
ALLOCATOR_TARGETS := \
	$(TARGET_DIR)/c-glibc-allocator-target \
	$(TARGET_DIR)/c-musl-allocator-target \
	$(TARGET_DIR)/c-jemalloc-allocator-target \
	$(TARGET_DIR)/c-tcmalloc-allocator-target \
	$(TARGET_DIR)/c-mimalloc-allocator-target
CORE_DUMP := $(TARGET_DIR)/c-misc-core-target.core

.PHONY: all clean core languages allocators \
	allocator-glibc allocator-musl allocator-jemalloc allocator-tcmalloc allocator-mimalloc

all: $(TARGETS)

languages: $(LANGUAGE_TARGETS)

allocators: $(ALLOCATOR_TARGETS)

allocator-glibc: $(TARGET_DIR)/c-glibc-allocator-target
allocator-musl: $(TARGET_DIR)/c-musl-allocator-target
allocator-jemalloc: $(TARGET_DIR)/c-jemalloc-allocator-target
allocator-tcmalloc: $(TARGET_DIR)/c-tcmalloc-allocator-target
allocator-mimalloc: $(TARGET_DIR)/c-mimalloc-allocator-target

$(TARGET_DIR)/c-glibc-allocator-target: c_allocator_target.c | $(TARGET_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) -DFGDB_ALLOCATOR_GLIBC $< $(LDFLAGS) -o $@

# Static linking makes this fixture independent of the host's musl loader path.
$(TARGET_DIR)/c-musl-allocator-target: c_allocator_target.c | $(TARGET_DIR)
	$(MUSL_CC) $(CPPFLAGS) $(CFLAGS) -DFGDB_ALLOCATOR_MUSL $< $(LDFLAGS) -static -o $@

$(TARGET_DIR)/c-jemalloc-allocator-target: c_allocator_target.c | $(TARGET_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) $(JEMALLOC_CFLAGS) -DFGDB_ALLOCATOR_JEMALLOC $< $(LDFLAGS) $(JEMALLOC_LIBS) -o $@

$(TARGET_DIR)/c-tcmalloc-allocator-target: c_allocator_target.c | $(TARGET_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) $(TCMALLOC_CFLAGS) -DFGDB_ALLOCATOR_TCMALLOC $< $(LDFLAGS) $(TCMALLOC_LIBS) -o $@

$(TARGET_DIR)/c-mimalloc-allocator-target: c_allocator_target.c | $(TARGET_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) $(MIMALLOC_CFLAGS) -DFGDB_ALLOCATOR_MIMALLOC $< $(LDFLAGS) $(MIMALLOC_LIBS) -o $@

$(TARGET_DIR)/fortran-variable-viewer-target: fortran_variable_viewer_target.f90 | $(TARGET_DIR)
	$(FORTRAN) -g3 -O0 -fcheck=all $< -o $@

$(TARGET_DIR)/zig-variable-viewer-target: zig_variable_viewer_target.zig | $(TARGET_DIR)
	$(ZIG) build-exe $< -O Debug -fllvm -femit-bin=$@

$(TARGET_DIR)/odin-variable-viewer-target: odin_variable_viewer_target.odin | $(TARGET_DIR)
	$(ODIN) build $< -file -debug -o:none -out:$@

$(TARGET_DIR):
	mkdir -p $@

$(TARGET_DIR)/c-memory-target: c_memory_target.c | $(TARGET_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) $< $(LDFLAGS) -o $@

$(TARGET_DIR)/c-simd-target: c_simd_target.c | $(TARGET_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) $< $(LDFLAGS) -o $@

$(TARGET_DIR)/c-allocation-target: c_allocation_target.c | $(TARGET_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) $< $(LDFLAGS) -o $@

$(TARGET_DIR)/c-page-target: c_page_target.c | $(TARGET_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) $< $(LDFLAGS) -o $@

$(TARGET_DIR)/c-misc-allocator-target: c_misc_allocator_target.c | $(TARGET_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) $< $(LDFLAGS) -o $@

$(TARGET_DIR)/c-misc-auxv-target: c_misc_auxv_target.c | $(TARGET_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) $< $(LDFLAGS) -o $@

$(TARGET_DIR)/c-misc-core-target: c_misc_core_target.c | $(TARGET_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) $< $(LDFLAGS) -o $@

$(TARGET_DIR)/c-misc-frame-target: c_misc_frame_target.c | $(TARGET_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) $< $(LDFLAGS) -o $@

$(TARGET_DIR)/c-misc-locks-target: c_misc_locks_target.c | $(TARGET_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) $< $(LDFLAGS) -pthread -o $@

$(TARGET_DIR)/c-thread-target: c_thread_target.c | $(TARGET_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) $< $(LDFLAGS) -pthread -o $@

$(TARGET_DIR)/c-tls-target: c_tls_target.c | $(TARGET_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) $< $(LDFLAGS) -pthread -o $@

$(TARGET_DIR)/c-process-target: c_process_target.c | $(TARGET_DIR)
	$(CC) $(CPPFLAGS) $(CFLAGS) $< $(LDFLAGS) -o $@

$(TARGET_DIR)/cpp-debug-target: cpp_debug_target.cpp | $(TARGET_DIR)
	$(CXX) $(CPPFLAGS) $(CXXFLAGS) $< $(LDFLAGS) -pthread -o $@

$(TARGET_DIR)/cpp-object-target: cpp_object_target.cpp | $(TARGET_DIR)
	$(CXX) $(CPPFLAGS) $(CXXFLAGS) $< $(LDFLAGS) -o $@

$(TARGET_DIR)/cpp-variable-viewer-target: cpp_variable_viewer_target.cpp | $(TARGET_DIR)
	$(CXX) $(CPPFLAGS) $(CXXFLAGS) $< $(LDFLAGS) -o $@

$(TARGET_DIR)/rust-debug-target: rust_debug_target.rs | $(TARGET_DIR)
	$(RUSTC) $(RUSTFLAGS) $< -o $@

$(TARGET_DIR)/rust-variable-viewer-target: rust_variable_viewer_target.rs | $(TARGET_DIR)
	$(RUSTC) $(RUSTFLAGS) $< -o $@

$(TARGET_DIR)/cpp-debug-target-o2: cpp_debug_target.cpp | $(TARGET_DIR)
	$(CXX) $(CPPFLAGS) -std=c++20 -g3 -O2 -fno-omit-frame-pointer $< $(LDFLAGS) -pthread -o $@

$(TARGET_DIR)/rust-debug-target-o2: rust_debug_target.rs | $(TARGET_DIR)
	$(RUSTC) --edition=2024 -C debuginfo=2 -C opt-level=2 -C force-frame-pointers=yes $< -o $@

core: $(CORE_DUMP)

$(CORE_DUMP): $(TARGET_DIR)/c-misc-core-target
	$(GDB) -q -nx -batch $< \
		-ex "set confirm off" \
		-ex run \
		-ex "generate-core-file $(abspath $@)"

clean:
	$(RM) $(TARGETS) $(LANGUAGE_TARGETS) $(ALLOCATOR_TARGETS) $(CORE_DUMP)
