"""Variants of stress_two.velt, to find what the settle race (#3360) needs:
  one:    one promise and one dropper per round
  const:  the settled values taken into constants before the droppers are spawned
  nojoin: main does not await the maker's join before it awaits the promises
python3 mkvariants.py"""
src = open("stress_two.velt").read()


def write(name, pairs):
    s = src
    for old, new in pairs:
        assert s.count(old) == 1, (name, old)
        s = s.replace(old, new)
    open(f"s2_{name}.velt", "w").write(s)


write(
    "one",
    [
        ("  b(new Holder(pb));\n", ""),
        ("    const db = spawn(dropper(await gb.promise, r), { worker: 3 });\n", ""),
        ("    sum += (await da) + (await db);\n", "    sum += await da;\n"),
    ],
)
write(
    "const",
    [
        (
            "    const da = spawn(dropper(await ga.promise, r), { worker: 2 });\n"
            "    const db = spawn(dropper(await gb.promise, r), { worker: 3 });\n",
            "    const ha = await ga.promise;\n    const hb = await gb.promise;\n"
            "    const da = spawn(dropper(ha, r), { worker: 2 });\n"
            "    const db = spawn(dropper(hb, r), { worker: 3 });\n",
        )
    ],
)
write(
    "nojoin",
    [
        ("    sum += await spawn(maker(ga.resolve, gb.resolve), { worker: 1 });\n", "    const mk = spawn(maker(ga.resolve, gb.resolve), { worker: 1 });\n"),
        ("    sum += (await da) + (await db);\n", "    sum += (await da) + (await db) + (await mk);\n"),
    ],
)
