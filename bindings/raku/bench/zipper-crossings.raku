use Libdictenstein;

my $count = (@*ARGS[0] // 512).Int;
die 'count must be between 1 and 100000' unless 1 <= $count <= 100000;
my $page-size = (@*ARGS[1] // 64).Int;
die 'page size must be between 1 and 4096' unless 1 <= $page-size <= 4096;
my $limits = BatchLimits.new(max-entries => $page-size,
    max-units => $page-size * 16, max-values => $page-size * 2);

my $left = dynamic-dawg;
my $right = dynamic-dawg;
my @keys = (^$count).map({ sprintf('key%06d', $_) });
$left.insert-batch((^$count).map({ Pair.new(@keys[$_], $_) }));
$right.insert-batch((^$count).map({ Pair.new(@keys[$_], $_ + 1) }));

my $started = now;
my $scalar-sum = 0;
$scalar-sum += $left.AT-KEY($_) for @keys;
my $scalar-ms = ((now - $started) * 1000).Num;

$started = now;
my @batch = $left.lookup-batch(@keys);
my $batch-ms = ((now - $started) * 1000).Num;
die 'batch lookup changed the result' unless
    @batch.elems == $count && @batch.map(*.value).sum == $scalar-sum;

sub time-view($view, $limits, Bool :$borrowed = False) {
    my $started = now;
    my $pages = 0;
    my $entries = 0;
    loop {
        my $page = $view.next-batch($limits);
        last unless $page.defined;
        $pages++;
        if $borrowed {
            $entries += $page.entries.elems;
            $page.release;
        } else {
            $entries += $page.elems;
        }
    }
    $view.close;
    ($entries, $pages + 1, ((now - $started) * 1000).Num)
}

my ($union-count, $union-calls, $union-ms) =
    time-view($left.union-view($right), $limits, borrowed => True);
my ($prefix-count, $prefix-calls, $prefix-ms) =
    time-view($left.prefix-view('key'), $limits, borrowed => True);
my ($diff-count, $diff-calls, $diff-ms) =
    time-view($left.value-difference-view($right), $limits);
die 'zipper count mismatch' unless
    $union-count == $count && $prefix-count == $count && $diff-count == $count;

say 'operation,entries,native_page_calls,elapsed_ms';
say "scalar_lookup,$count,$count,$scalar-ms";
say "batch_lookup,$count,1,$batch-ms";
say "union_view,$union-count,$union-calls,$union-ms";
say "prefix_view,$prefix-count,$prefix-calls,$prefix-ms";
say "value_diff_view,$diff-count,$diff-calls,$diff-ms";
$left.close;
$right.close;
