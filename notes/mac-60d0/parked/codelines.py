import sys


def code_lines(text):
    n = 0
    in_doc = False
    lines = text.split("\n")
    for raw in lines[:-1]:
        line = raw.strip()
        if not in_doc and line.startswith("/**"):
            in_doc = (not line.endswith("*/")) or line == "/**"
            continue
        if in_doc:
            if line.endswith("*/"):
                in_doc = False
            continue
        n += 1
    return n


for path in sys.argv[1:]:
    with open(path) as f:
        print(code_lines(f.read()), path)
