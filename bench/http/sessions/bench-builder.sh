#!/bin/bash
export BASE=~/velt/branches/smp-base PROTO=~/velt/branches/shared-marked-proto
export BASE_VELT=~/velt-build/smp-base2/release/velt PROTO_VELT=~/velt-build/smp-proto/release/velt
export SERVER_CPUS=0,1,6,7,8,9,10,11 CLIENT_CPUS=2-5 OHA=$HOME/velt-build/smp/oha116
export RUNS=${RUNS:-3} DURATION=${DURATION:-5s}
cd ~/velt/branches/shared-marked-proto
exec bash bench/http/sessions/run.sh
