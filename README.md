# CPU bench branch

Orphan branch on the fork that only carries a CPU benchmark run: `.github/workflows/cpu-bench.yml`,
`bench/bench.py` (pairs runner), `bench/report.py` (aggregate) and `bench/bench.json` (inputs).
Pushing it runs the workflow: both trees are fetched by SHA, built with
`cargo +<toolchain> build --release --bins`, then each workload runs 1 warmup + 7 alternating
pairs measuring child user CPU, requiring identical instruction counts and console hashes.
Results: job summary and the `result-*` / `summary` artifacts. Not a code branch; never merge.
