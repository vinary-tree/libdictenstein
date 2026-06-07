#!/usr/bin/env raku
# F6 un-pin: remove ONLY the F6-compact OwnedTree pins (those whose preceding
# comment block mentions "pending F6 phase" OR "compaction rebuilds from the owned
# tree"). Leaves untouched the NON-compact pins that share the kill_switch_to_owned
# idiom for other reasons (negative-increment / dirty-state / DurabilityPolicy::None).
my @files = «
    tests/compaction_tests.rs
    tests/persistent_compaction_correspondence.rs
»;

sub is-f6-compact-pin(Str $block --> Bool) {
    $block.contains('pending F6 phase')
        || $block.contains('compaction rebuilds from the owned tree');
}

for @files -> $f {
    my @lines = $f.IO.lines;
    my @out;
    my $replaced = 0;
    for @lines -> $line {
        if $line ~~ /^ (\s*) ['trie'|'dict'] '.kill_switch_to_owned();' \s* $/ {
            my $indent = ~$0;
            # Pop the contiguous comment lines immediately above the kill_switch.
            my @popped;
            while @out && @out[*-1].trim.starts-with('//') {
                @popped.unshift(@out.pop);
            }
            my $block = @popped.join("\n");
            if is-f6-compact-pin($block) {
                # F6-compact pin: drop the comment block + the kill_switch, leaving an
                # accurate note. Preserve any non-pin comment that sits ABOVE the pin
                # block (restore lines up to the first pin-marker line).
                my $pin-idx = @popped.first(
                    { .contains('compact') || .contains('F6') }, :k);
                @out.append(@popped[0 ..^ ($pin-idx // 0)]);
                @out.push("$indent\// F6: overlay compaction implemented — feature-on this trie is");
                @out.push("$indent\// overlay-routed and compact() exercises the overlay snapshot path;");
                @out.push("$indent\// feature-off it stays owned. (Former OwnedTree pin removed.)");
                $replaced++;
            } else {
                # Not an F6-compact pin (negative-increment / dirty-state / policy):
                # restore the comments and keep the kill_switch.
                @out.append(@popped);
                @out.push($line);
            }
        } else {
            @out.push($line);
        }
    }
    $f.IO.spurt(@out.join("\n") ~ "\n");
    say "$f: un-pinned $replaced F6-compact pin(s)";
}
