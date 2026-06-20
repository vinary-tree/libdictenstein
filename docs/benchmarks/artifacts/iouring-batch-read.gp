# Self-contained gnuplot script. Rendered in-place by scripts/render-diagrams.sh
# to iouring-batch-read.svg. Output basename MUST equal script basename.
set terminal svg size 680,460 font 'DejaVu Sans,11' background rgb 'white'
set output 'iouring-batch-read.svg'

set title "Batch read (64 blocks): io_uring SQE batching wins (1.41x vs mmap)\n{/*0.8 Source: io_uring_migration/benchmark_results.md Phase 3 Batch Read (2026-02-20)}"
set ylabel "Throughput (Kelem/s, higher is better)"
set xlabel "I/O strategy (64 x 256 KB blocks)"

set style fill solid 0.92 border rgb '#444444'
set boxwidth 0.6
set yrange [0:34]
set grid ytics lc rgb '#cccccc' lw 1
unset key
set border 3
set xtics nomirror
set ytics nomirror

# Map color_id -> house palette: 1 amber (mmap), 2 blue (io_uring win), 3 grey (io_uring seq, the slow control).
# lc rgb variable reads a packed 0xRRGGBB integer (NOT a hex string).
mycolor(i) = (i == 1) ? 0xF9A825 : (i == 2) ? 0x1565C0 : 0x607D8B

# Bars colored per row; value labels above each.
plot 'iouring-batch-read.dat' using 1:3:(mycolor($4)):xtic(2) with boxes lc rgb variable notitle, \
     '' using 1:($3+1.3):(sprintf("%.1f", $3)) with labels font 'DejaVu Sans,10' notitle
