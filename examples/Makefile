CXX ?= g++
CXXFLAGS ?= -std=c++20 -g3 -O0 -fno-omit-frame-pointer -fno-inline
CPPFLAGS ?=
LDFLAGS ?=
LDLIBS ?= -pthread

TARGET := ../target/debug-fixtures/cpp-debug-target
SOURCE := cpp_debug_target.cpp

.PHONY: all clean

all: $(TARGET)

$(TARGET): $(SOURCE)
	mkdir -p $(dir $@)
	$(CXX) $(CPPFLAGS) $(CXXFLAGS) $< $(LDFLAGS) $(LDLIBS) -o $@

clean:
	$(RM) $(TARGET)
