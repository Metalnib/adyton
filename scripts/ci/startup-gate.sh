#!/bin/sh
# Cold-start budget gate (specification §8): best-of-20 `--version`, in ms.
# Best-of measurement filters CI scheduler noise; the budget is generous vs the
# ~2 ms measured baseline.
set -eu
bin=$1
budget_ms=$2
python3 - "$bin" "$budget_ms" <<'EOF'
import subprocess, sys, time

binary, budget = sys.argv[1], float(sys.argv[2])
best = min(
    (lambda t0: (subprocess.run([binary, "--version"], stdout=subprocess.DEVNULL, check=True),
                 (time.perf_counter() - t0) * 1000)[1])(time.perf_counter())
    for _ in range(20)
)
print(f"startup best-of-20: {best:.1f} ms (budget {budget} ms)")
sys.exit(0 if best <= budget else 1)
EOF
