#!/bin/bash
# The units fix (branch units-inline-import): its release velt in the VM, $HOME/b3097fix/tgt-units.
# The runtime is the same as main's (the fix changes only velt_codegen_llvm), so count.sh's RT works.
#   orb -m velt-prof bash notes/3097/vm-fix/units-build.sh
set -eu
source "$HOME/.cargo/env"
cd /Users/andii/dev/velt/branches/units-inline-import
CARGO_TARGET_DIR=$HOME/b3097fix/tgt-units CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 nice -n 19 \
  cargo build --release -q -p veltc --bin velt
ls -la "$HOME/b3097fix/tgt-units/release/velt"
