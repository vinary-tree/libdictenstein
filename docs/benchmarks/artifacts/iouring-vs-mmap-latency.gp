# Self-contained gnuplot script. Rendered in-place by scripts/render-diagrams.sh
# to iouring-vs-mmap-latency.svg. Output basename MUST equal script basename.
set terminal svg size 760,470 font 'DejaVu Sans,11' background rgb 'white'
set output 'iouring-vs-mmap-latency.svg'

set title "Single-block I/O: mmap wins on per-operation latency (1.7-2.3x)\n{/*0.8 Source: io_uring_migration/benchmark_results.md Phase 3 (2026-02-20); summary-report averages, 256 KB blocks}"
set ylabel "Latency (microseconds, lower is better)"
set xlabel "Block-level random operation / percentile"

set style data histograms
set style histogram clustered gap 1.4
set style fill solid 0.92 border rgb '#444444'
set boxwidth 0.9
set yrange [0:250]
set grid ytics lc rgb '#cccccc' lw 1
set key top left box opaque
set border 3
set xtics nomirror
set ytics nomirror

# amber = mmap (page cache), blue = io_uring (submission rings).
set style line 1 lc rgb '#F9A825'   # mmap (amber)
set style line 2 lc rgb '#1565C0'   # io_uring (blue)

# Value labels above each bar (offset pair = the two clustered columns).
set label 1 "32"  at -0.215,40  center font 'DejaVu Sans,9'
set label 2 "61"  at  0.215,71  center font 'DejaVu Sans,9'
set label 3 "90"  at  0.785,100 center font 'DejaVu Sans,9'
set label 4 "210" at  1.215,220 center font 'DejaVu Sans,9'
set label 5 "44"  at  1.785,54  center font 'DejaVu Sans,9'
set label 6 "74"  at  2.215,84  center font 'DejaVu Sans,9'
set label 7 "120" at  2.785,130 center font 'DejaVu Sans,9'
set label 8 "225" at  3.215,235 center font 'DejaVu Sans,9'

plot 'iouring-vs-mmap-latency.dat' using 2:xtic(1) ls 1 title 'mmap (MmapDiskManager)', \
     '' using 3 ls 2 title 'io_uring + O\_DIRECT (IoUringDiskManager)'
