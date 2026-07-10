# Self-contained gnuplot script. Rendered in-place by scripts/render-diagrams.sh
# to iouring-vs-mmap-latency.svg. Output basename MUST equal script basename.
set terminal svg size 760,470 font 'DejaVu Sans,11' background rgb 'white'
set output 'iouring-vs-mmap-latency.svg'

set title "Single-block latency on real NVMe (log scale): mmap wins reads 17x\n{/*0.8 Source: io\\_uring\\_migration/benchmark\\_results.md Phase 6 (2026-07-10); cmp\\_summary\\_report percentiles, 256 KB blocks}"
set ylabel "Latency (microseconds, log scale, lower is better)"
set xlabel "Block-level random operation / percentile"

set style data histograms
set style histogram clustered gap 1.4
set style fill solid 0.92 border rgb '#444444'
set boxwidth 0.9
set logscale y
set yrange [5:900]
set ytics (5, 10, 20, 50, 100, 200, 500)
set grid ytics lc rgb '#cccccc' lw 1
set key top left box opaque
set border 3
set xtics nomirror
set ytics nomirror

# amber = mmap (page cache), blue = io_uring (submission rings).
set style line 1 lc rgb '#F9A825'   # mmap (amber)
set style line 2 lc rgb '#1565C0'   # io_uring (blue)

# Value labels above each bar (offset pair = the two clustered columns).
# y positions are ~1.18x the bar value on the log axis so each label clears its bar.
set label 1 "10.9" at -0.215,12.9 center font 'DejaVu Sans,9'
set label 2 "187"  at  0.215,221  center font 'DejaVu Sans,9'
set label 3 "16.3" at  0.785,19.2 center font 'DejaVu Sans,9'
set label 4 "222"  at  1.215,262  center font 'DejaVu Sans,9'
set label 5 "117"  at  1.785,138  center font 'DejaVu Sans,9'
set label 6 "93"   at  2.215,110  center font 'DejaVu Sans,9'
set label 7 "127"  at  2.785,150  center font 'DejaVu Sans,9'
set label 8 "538"  at  3.215,635  center font 'DejaVu Sans,9'

plot 'iouring-vs-mmap-latency.dat' using 2:xtic(1) ls 1 title 'mmap (MmapDiskManager)', \
     '' using 3 ls 2 title 'io\_uring + O\_DIRECT (IoUringDiskManager)'
