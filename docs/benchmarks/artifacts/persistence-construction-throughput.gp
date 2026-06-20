# Self-contained gnuplot script. Rendered in-place by scripts/render-diagrams.sh
# (cwd = this file's directory) to persistence-construction-throughput.svg.
# Output basename MUST equal this script's basename.
set terminal svg size 760,460 font 'DejaVu Sans,11' background rgb 'white'
set output 'persistence-construction-throughput.svg'

set title "PersistentARTrie construction throughput scales with size\n{/*0.8 Source: persistence-enhancements-ledger.md — Experiment 0 Baseline (2026-01-15)}"
set xlabel "Dictionary size (terms, log scale)"
set ylabel "Construction throughput (Melem/s, higher is better)"

set logscale x 10
set xrange [70:7000]
set yrange [0:22]
set grid ytics lc rgb '#cccccc' lw 1
set key top left box opaque
set border 3
set xtics nomirror (100, 500, 1000, 5000)
set ytics nomirror

# House palette: green = in-memory DoubleArrayTrie, blue = ARTrie, amber = DAWG.
set style line 1 lc rgb '#1565C0' lw 2.5 pt 7 ps 1.2   # PersistentARTrie (blue)
set style line 2 lc rgb '#F9A825' lw 2.5 pt 5 ps 1.2   # DynamicDawg (amber)
set style line 3 lc rgb '#2E7D32' lw 2.5 pt 9 ps 1.4   # DoubleArrayTrie (green)

plot 'persistence-construction-throughput.dat' using 1:2 with linespoints ls 1 title 'PersistentARTrie', \
     '' using 1:3 with linespoints ls 2 title 'DynamicDawg', \
     '' using 1:4 with linespoints ls 3 title 'DoubleArrayTrie'
