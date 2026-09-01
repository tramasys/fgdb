CC ?= cc
CXX ?= g++
RUSTC ?= rustc
GDB ?= gdb

CFLAGS ?= -std=c17 -g3 -O0 -fno-omit-frame-pointer -fno-inline -Wall -Wextra -Wpedantic
CXXFLAGS ?= -std=c++20 -g3 -O0 -fno-omit-frame-pointer -fno-inline -Wall -Wextra -Wpedantic
RUSTFLAGS ?= --edition=2024 -C debuginfo=2 -C opt-level=0 -C force-frame-pointers=yes
CPPFLAGS ?=
LDFLAGS ?=

TARGET_DIR := ../target/debug-fixtures
C_TARGETS := \
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
CORE_DUMP := $(TARGET_DIR)/c-misc-core-target.core

.PHONY: all clean core

all: $(TARGETS)

$(TARGET_DIR):
	mkdir -p $@

$(TARGET_DIR)/c-memory-target: c_memory_target.c | $(TARGET_DIR)
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
	$(RM) $(TARGETS) $(CORE_DUMP)
