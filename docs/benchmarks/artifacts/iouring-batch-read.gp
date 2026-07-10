# Self-contained gnuplot script. Rendered in-place by scripts/render-diagrams.sh
# to iouring-batch-read.svg. Output basename MUST equal script basename.
set terminal svg size 680,460 font 'DejaVu Sans,11' background rgb 'white'
set output 'iouring-batch-read.svg'

set title "Batch read (64 blocks, real NVMe): mmap's cache-served batch wins 6.7x\n{/*0.8 Source: io\\_uring\\_migration/benchmark\\_results.md Phase 6 Batch Read (2026-07-10)}"
set ylabel "Throughput (Kelem/s, higher is better)"
set xlabel "I/O strategy (64 x 256 KB blocks)"

set style fill solid 0.92 border rgb '#444444'
set boxwidth 0.6
set yrange [0:100]
set grid ytics lc rgb '#cccccc' lw 1
unset key
set border 3
set xtics nomirror
set ytics nomirror

# Map color_id -> house palette per strategy: 1 amber (mmap, the winner here),
# 2 blue (io_uring batch SQE), 3 grey (io_uring sequential, the slow control).
# lc rgb variable reads a packed 0xRRGGBB integer (NOT a hex string).
mycolor(i) = (i == 1) ? 0xF9A825 : (i == 2) ? 0x1565C0 : 0x607D8B

# Bars colored per row; value labels above each.
plot 'iouring-batch-read.dat' using 1:3:(mycolor($4)):xtic(2) with boxes lc rgb variable notitle, \
     '' using 1:($3+4):(sprintf("%.1f", $3)) with labels font 'DejaVu Sans,10' notitle
