# Self-contained gnuplot script. Rendered in-place by scripts/render-diagrams.sh
# to loading-open-time-before-after.svg. Output basename MUST equal script basename.
set terminal svg size 740,470 font 'DejaVu Sans,11' background rgb 'white'
set output 'loading-open-time-before-after.svg'

set title "Lazy loading collapses open time across every dataset size (88-98% lower)\n{/*0.8 Source: loading-optimization-ledger.md — Experiment 1 Lazy Loading (2026-01-10)}"
set ylabel "Open time (ms, log scale, lower is better)"
set xlabel "Dataset size (terms)"

set style data histograms
set style histogram clustered gap 1.4
set style fill solid 0.92 border rgb '#444444'
set boxwidth 0.9
set logscale y 10
set yrange [0.3:20000]
set grid ytics lc rgb '#cccccc' lw 1
set key top left box opaque
set border 3
set xtics nomirror
set ytics nomirror

# grey = eager baseline, green = lazy (the accepted optimization).
set style line 1 lc rgb '#607D8B'   # Eager baseline (grey)
set style line 2 lc rgb '#2E7D32'   # Lazy (green)

# Percent-reduction labels above each cluster.
set label 1 "-88%" at 0,2.2  center font 'DejaVu Sans,10' tc rgb '#2E7D32'
set label 2 "-95%" at 1,1700 center font 'DejaVu Sans,10' tc rgb '#2E7D32'
set label 3 "-98%" at 2,16000 center font 'DejaVu Sans,10' tc rgb '#2E7D32'

plot 'loading-open-time-before-after.dat' using 3:xtic(2) ls 1 title 'Eager (baseline)', \
     '' using 4 ls 2 title 'Lazy (accepted)'
