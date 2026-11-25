import re,sys
src=open(sys.argv[1]).read()
out=sys.argv[2]
for m in re.finditer(r'const (\w+): &str = r#"(.*?)"#;', src, re.S):
    open(f"{out}/{m.group(1).lower()}.velt","w").write(m.group(2))
    print(m.group(1))
