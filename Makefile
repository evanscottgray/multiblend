# Builds the C++ reference binary used by the test suite.
#
#   make            -> build/multiblend       (flags from build.txt)
#   make asan       -> build/multiblend-asan  (AddressSanitizer + UBSan; some
#                      known-bug tests use it to make memory errors deterministic)
#   make venv       -> .venv with the pinned test dependencies
#   make test       -> run the pytest suite against build/multiblend
#   make rust       -> rust/target/release/multiblend (the Rust port; needs cargo/rustup)
#   make test-rust  -> run the pytest suite against the Rust port
#
# Library prefix defaults to Homebrew; override with PREFIX=/usr etc.

PREFIX   ?= $(shell brew --prefix 2>/dev/null || echo /usr)
CXX      ?= c++
INCLUDES  = -I$(PREFIX)/opt/jpeg-turbo/include -I$(PREFIX)/opt/libpng/include -I$(PREFIX)/opt/libtiff/include -I$(PREFIX)/include
LIBDIRS   = -L$(PREFIX)/opt/jpeg-turbo/lib -L$(PREFIX)/opt/libpng/lib -L$(PREFIX)/opt/libtiff/lib -L$(PREFIX)/lib
LIBS      = -lpng -ltiff -ljpeg -lm
BASEFLAGS = -std=c++14 -msse4.1 -pthread -w
SOURCES   = $(wildcard src/*.cpp src/*.h)
# rustup installs to ~/.cargo/bin, which may not be on PATH
CARGO    ?= $(shell command -v cargo 2>/dev/null || echo $(HOME)/.cargo/bin/cargo)

.PHONY: all asan venv test rust test-rust clean

all: build/multiblend

build/multiblend: $(SOURCES)
	@mkdir -p build
	$(CXX) $(BASEFLAGS) -ffast-math -Ofast $(INCLUDES) $(LIBDIRS) -o $@ src/multiblend.cpp $(LIBS)

asan: build/multiblend-asan

build/multiblend-asan: $(SOURCES)
	@mkdir -p build
	$(CXX) $(BASEFLAGS) -O1 -g -fno-omit-frame-pointer -fsanitize=address,undefined $(INCLUDES) $(LIBDIRS) -o $@ src/multiblend.cpp $(LIBS)

venv: .venv/.installed

.venv/.installed: tests/requirements.txt
	python3 -m venv .venv
	.venv/bin/pip install -q -r tests/requirements.txt
	@touch $@

test: build/multiblend build/multiblend-asan .venv/.installed
	.venv/bin/python -m pytest

rust:
	cd rust && $(CARGO) build --release

test-rust: rust .venv/.installed
	MULTIBLEND_BIN=rust/target/release/multiblend .venv/bin/python -m pytest

clean:
	rm -rf build
