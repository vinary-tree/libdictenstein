# Self-contained gnuplot script. Rendered in-place by scripts/render-diagrams.sh
# to pernode-recovery-speedup.svg. Output basename MUST equal script basename.
set terminal svg size 800,480 font 'DejaVu Sans,11' background rgb 'white'
set output 'pernode-recovery-speedup.svg'

set title "Per-node redo logging gives O(dirty) recovery: 10-103x faster reopen\n{/*0.8 Source: persistence-enhancements-ledger.md — Experiment 5 Per-Node Logging (2026-01-15)}"
set ylabel "Recovery time (microseconds, log scale, lower is better)"
set xlabel "Workload (total ops / dirty-node ratio)"

set style data histograms
set style histogram clustered gap 1
set style fill solid 0.92 border rgb '#444444'
set boxwidth 0.9
set logscale y 10
set yrange [1:4000]
set grid ytics lc rgb '#cccccc' lw 1
set key top right box opaque
set border 3
set xtics nomirror
set ytics nomirror

# grey = baseline (Global WAL), blue = ARTrie improved path (Per-Node).
set style line 1 lc rgb '#607D8B'   # Global WAL (grey baseline)
set style line 2 lc rgb '#1565C0'   # Per-Node (blue)

# Speedup labels (the ledger's recorded factor) centred above each cluster.
set label 1 "103x" at 0,210   center font 'DejaVu Sans,10' tc rgb '#C62828'
set label 2 "20x"  at 1,210   center font 'DejaVu Sans,10' tc rgb '#C62828'
set label 3 "10x"  at 2,210   center font 'DejaVu Sans,10' tc rgb '#C62828'
set label 4 "100x" at 3,2150  center font 'DejaVu Sans,10' tc rgb '#C62828'
set label 5 "21x"  at 4,2150  center font 'DejaVu Sans,10' tc rgb '#C62828'

plot 'pernode-recovery-speedup.dat' using 3:xtic(2) ls 1 title 'Global WAL (O(total ops))', \
     '' using 4 ls 2 title 'Per-Node log (O(dirty nodes))'
