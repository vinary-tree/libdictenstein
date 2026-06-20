# Self-contained gnuplot script. Rendered in-place by scripts/render-diagrams.sh
# to lockfree-flip-throughput.svg. Output basename MUST equal script basename.
set terminal svg size 880,500 font 'DejaVu Sans,11' background rgb 'white'
set output 'lockfree-flip-throughput.svg'

set title "Lock-free overlay checkpoint beats Arc<RwLock> owned tree on write throughput (3-6.5x)\n{/*0.8 Source: lockfree-flip-benchmark-ledger.md, K=30/arm, Immediate durability (2026-06-02, HEAD 549957a)}"
set ylabel "Write throughput (ops/sec, higher is better)"
set xlabel "Durability/eviction regime and workload variant"

set style data histograms
set style histogram clustered gap 1.4
set style fill solid 0.92 border rgb '#444444'
set boxwidth 0.9
set yrange [0:21000]
set grid ytics lc rgb '#cccccc' lw 1
set key top right box opaque
set border 3
set xtics nomirror rotate by -18 right font 'DejaVu Sans,9'
set ytics nomirror

# red = CONTROL (the path the flip replaces / worst), blue = TREATMENT (lock-free).
set style line 1 lc rgb '#C62828'   # CONTROL (red)
set style line 2 lc rgb '#1565C0'   # TREATMENT (blue)

# Percentage-gain labels above each TREATMENT bar (its cluster offset is +0.21).
set label 1 "+312%" at 0.21,19600 center font 'DejaVu Sans,8' tc rgb '#1565C0'
set label 2 "+167%" at 1.21,16300 center font 'DejaVu Sans,8' tc rgb '#1565C0'
set label 3 "+534%" at 2.21,18050 center font 'DejaVu Sans,8' tc rgb '#1565C0'
set label 4 "+300%" at 3.21,16000 center font 'DejaVu Sans,8' tc rgb '#1565C0'
set label 5 "+653%" at 4.21,15780 center font 'DejaVu Sans,8' tc rgb '#1565C0'
set label 6 "+507%" at 5.21,15620 center font 'DejaVu Sans,8' tc rgb '#1565C0'

plot 'lockfree-flip-throughput.dat' using 2:xtic(1) ls 1 title 'CONTROL (Arc<RwLock> owned tree)', \
     '' using 3 ls 2 title 'TREATMENT (lock-free overlay)'
