# Self-contained gnuplot script. Rendered in-place by scripts/render-diagrams.sh
# to loading-strategy-comparison.svg. Output basename MUST equal script basename.
set terminal svg size 720,470 font 'DejaVu Sans,11' background rgb 'white'
set output 'loading-strategy-comparison.svg'

set title "Lazy loading opens a 1M-term trie 49x faster than eager\n{/*0.8 Source: loading-optimization-ledger.md Summary Table, 1M terms (2026-01-10)}"
set ylabel "Open time (ms, log scale, lower is better)"
set xlabel "Loading strategy (1,000,000 terms)"

set style fill solid 0.92 border rgb '#444444'
set boxwidth 0.6
set logscale y 10
set yrange [100:14000]
set grid ytics lc rgb '#cccccc' lw 1
unset key
set border 3
set xtics nomirror
set ytics nomirror

# 1 red = REJECTED, 2 green = ACCEPTED (the chosen default), 3 grey = baseline.
# lc rgb variable reads a packed 0xRRGGBB integer (NOT a hex string).
mycolor(i) = (i == 1) ? 0xC62828 : (i == 2) ? 0x2E7D32 : 0x607D8B

plot 'loading-strategy-comparison.dat' using 1:3:(mycolor($4)):xtic(2) with boxes lc rgb variable notitle, \
     '' using 1:($3*1.18):(sprintf("%.1f ms", $3)) with labels font 'DejaVu Sans,10' notitle, \
     '' using 1:($3*0.55):5 with labels font 'DejaVu Sans,9' tc rgb 'white' notitle
