#!/bin/bash
# perop.sh <exe>: X25519 instructions per operation (1001 vs 1 iterations)
a=$(./count.sh "$1" 1 1); b=$(./count.sh "$1" 1001 1)
python3 -c "print(f'{($b-$a)/1000/1e6:.3f} M/op')"
