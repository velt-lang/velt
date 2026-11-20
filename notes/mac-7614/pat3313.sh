#!/bin/bash
# The fixed smoke-clean patterns against the real stats lines from dry run 44656592401.
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/76140236-8161-4f16-8c05-0234cd4a600f/scratchpad
grep -h "velt: prebuilt hooks:" "$S/cm_x86_64-unknown-linux.log" | sed 's/^[0-9T:.-]*Z //' > "$S/stats_real.txt"
cat "$S/stats_real.txt"
for module in number_digits number_parse weak; do
  if grep -q "prebuilt hooks: std/lang/hooks/$module: object found (shipped)" "$S/stats_real.txt"; then
    echo "bash match: $module"
  else
    echo "bash MISS: $module"
  fi
done
# A stats file without the shipped lines must not match.
grep -v number_parse "$S/stats_real.txt" > "$S/stats_broken.txt"
grep -q "prebuilt hooks: std/lang/hooks/number_parse: object found (shipped)" "$S/stats_broken.txt" &&
  echo "bash WRONG: matched without the line" || echo "bash ok: no match without the line"
# The PowerShell regex, as Python's re reads it (the same syntax for this pattern).
python3 -I - "$S/stats_real.txt" <<'PY'
import re, sys
text = open(sys.argv[1]).read()
for m in ["number_digits", "number_parse", "weak"]:
    pat = r"prebuilt hooks: std/lang/hooks/" + m + r": object found \(shipped\)"
    print("regex", "match" if re.search(pat, text) else "MISS", m)
PY
