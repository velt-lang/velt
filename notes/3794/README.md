# Scripts and logs behind PR #3794 (#3498 follow-up)

- `brk.sh <break> <runs> <pause> prog...`: builds a program against a std copy with break `<break>` (mkbreak.py; probes p_* in mkbreak2.py / patch_*.py) and runs it counted at 4 and 8 workers under a 60 s cap. Programs come from the test files via extract.py.
- `vmrows.sh build|rows|prof|clean`: arm64 cachegrind rows in the OrbStack VM, base std (eb9c6e769) vs head std, one compiler. `dis.sh` / `calls.sh`: disassembly and call diffs of one function. `t4reps.sh`: interleaved T4 counts.
- logs/: break checks (breaks9, combo, refs), rows (vmrows3, pp), the scheduler test run on the rebased tree (sched-all3).
- The x86 rows were run on the builder with the maintainers' cg252-cg.sh.
