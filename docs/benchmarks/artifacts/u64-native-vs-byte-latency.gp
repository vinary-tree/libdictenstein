# Self-contained gnuplot script. Rendered in-place by scripts/render-diagrams.sh
# to u64-native-vs-byte-latency.svg. Output basename MUST equal script basename.
set terminal svg size 720,470 font 'DejaVu Sans,11' background rgb 'white'
set output 'u64-native-vs-byte-latency.svg'

set title "Native prefix-4 u64 keys cut read latency vs byte-encoding (shallower paths)\n{/*0.8 Source: persistent-u64-watermark-commitrank-2026-06-13.md (registered exps 53/54), Welch-accepted}"
set ylabel "Latency (nanoseconds, lower is better)"
set xlabel "Read-path metric (PersistentARTrieU64Compact)"

set style data histograms
set style histogram clustered gap 1.6
set style fill solid 0.92 border rgb '#444444'
set boxwidth 0.85
set yrange [0:520]
set grid ytics lc rgb '#cccccc' lw 1
set key top right box opaque
set border 3
set xtics nomirror
set ytics nomirror

# grey = byte-encoded control, blue = native ARTrie treatment.
set style line 1 lc rgb '#607D8B'   # byte-encoded u64 (grey control)
set style line 2 lc rgb '#1565C0'   # native prefix-4 (blue treatment)

# Value labels above each bar.
set label 1 "455.4" at -0.21,475 center font 'DejaVu Sans,9'
set label 2 "357.2" at  0.21,377 center font 'DejaVu Sans,9'
set label 3 "204.3" at  0.79,224 center font 'DejaVu Sans,9'
set label 4 "148.3" at  1.21,168 center font 'DejaVu Sans,9'
# Significance labels under each metric group.
set label 5 "p=2.82e-35" at 0,28 center font 'DejaVu Sans,8' tc rgb '#2E7D32'
set label 6 "p=4.42e-9"  at 1,28 center font 'DejaVu Sans,8' tc rgb '#2E7D32'

plot 'u64-native-vs-byte-latency.dat' using 2:xtic(1) ls 1 title 'byte-encoded u64 (control)', \
     '' using 3 ls 2 title 'native prefix-4 U64Key (treatment)'
