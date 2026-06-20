# Self-contained gnuplot script. Rendered in-place by scripts/render-diagrams.sh
# to disktrie-durable-throughput.svg. Output basename MUST equal script basename.
set terminal svg size 720,460 font 'DejaVu Sans,11' background rgb 'white'
set output 'disktrie-durable-throughput.svg'

set title "Durable build amortizes fixed cost: throughput rises with size\n{/*0.8 Source: theory/disk-tries/07-benchmark-results.md — 2024-12-27 durability bring-up snapshot}"
set xlabel "Dictionary size (terms)"
set ylabel "Throughput (Melem/s, higher is better)"

set xrange [0:1100]
set yrange [0:3.4]
set grid ytics lc rgb '#cccccc' lw 1
set key top left box opaque
set border 3
set xtics nomirror (100, 500, 1000)
set ytics nomirror

# blue = persistent write path (create+insert+sync), grey = recovery (replay baseline).
set style line 1 lc rgb '#1565C0' lw 2.5 pt 7 ps 1.3   # create+insert+sync (blue)
set style line 2 lc rgb '#607D8B' lw 2.5 pt 5 ps 1.2   # recovery (grey)

plot 'disktrie-durable-throughput.dat' using 1:2 with linespoints ls 1 title 'create + insert + sync (full durability)', \
     '' using 1:3 with linespoints ls 2 title 'recovery (checkpoint load + WAL replay)', \
     '' using 1:2:(sprintf("%.2f", $2)) with labels offset 0,1.0 font 'DejaVu Sans,9' tc rgb '#1565C0' notitle, \
     '' using 1:3:(sprintf("%.2f", $3)) with labels offset 0,-1.0 font 'DejaVu Sans,9' tc rgb '#607D8B' notitle
