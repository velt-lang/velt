"""Writes handed4.velt and handed5.velt from the issue comment saved as ../3207-probes.md."""
import os
import re

here = os.path.dirname(os.path.abspath(__file__))
s = open(os.path.join(here, "..", "3207-probes.md")).read()
for name in ("handed4", "handed5"):
    m = re.search(r"### %s\.velt\n```ts\n(.*?)```" % name, s, re.S)
    open(os.path.join(here, name + ".velt"), "w").write(m.group(1))
    print(name, len(m.group(1)))
