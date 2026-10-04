#!/usr/bin/env bash
# Exact, bidirectional inventory of byte-DAWG proof obligations.
set -euo pipefail
repo_root="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)"
cd "$repo_root"
sha256sum -c formal-verification/rocq/byte-dawg-drop-qualification.sources
perl - formal-verification/rocq/byte-dawg-drop-qualification-ledger.tsv \
  formal-verification/rocq/Spec/LockFreeDawgDropSpec.v \
  formal-verification/tla+/LockFreeDawgDropRace.tla \
  formal-verification/tla+/LockFreeDawgDropRace_Safe.cfg \
  formal-verification/tla+/LockFreeDawgDropRace_SafeShared.cfg \
  formal-verification/tla+/LockFreeDawgDropRace_TryUnwrapUnsafe.cfg <<'PERL'
use strict;
use warnings;
my ($ledger_path, $rocq_path, $tla_path, @configs) = @ARGV;
sub fail { die "byte-DAWG ledger: $_[0]\n" }
sub lines {
  my ($path) = @_;
  open my $fh, '<:encoding(UTF-8)', $path or fail("open $path: $!");
  my @rows = <$fh>; chomp @rows; return @rows;
}
sub equal_sets {
  my ($label, $actual, $expected) = @_;
  for (sort keys %$expected) { exists $actual->{$_} or fail("$label omits $_") }
  for (sort keys %$actual) { exists $expected->{$_} or fail("$label invents $_") }
}
my @ledger = lines($ledger_path);
shift(@ledger) eq join("\t", qw(kind symbol claim premises source_seam witness_id control_id boundary))
  or fail('bad ledger header');
my (%rocq_rows, %tla_rows, %witnesses, %controls);
for my $line (@ledger) {
  my @f = split /\t/, $line, -1;
  @f == 8 or fail("wrong field count: $line");
  for (@f) { length($_) && !/\r/ or fail("empty/CR field: $line") }
  my ($kind, $symbol, $claim, $premises, $seam, $witness, $control, $boundary) = @f;
  $kind eq 'Rocq' || $kind eq 'TLA' or fail("unknown kind $kind");
  $claim =~ /[A-Za-z]/ && $boundary =~ /[A-Za-z]/ or fail("unexplained $symbol");
  $premises =~ /^[A-Z][A-Z0-9_]*(?:;[A-Z][A-Z0-9_]*)*$/ or fail("premises: $symbol");
  $seam =~ /^(?:LockFreeDawgNode|GraphVersion|arc_from_cursor|root_arc)/
    or fail("source seam: $symbol");
  $witness =~ /^DAWG_PROP_[A-Z0-9_]+$/ or fail("witness: $symbol");
  $control =~ /^DAWG_MUT_[A-Z0-9_]+$/ or fail("control: $symbol");
  my $set = $kind eq 'Rocq' ? \%rocq_rows : \%tla_rows;
  !$set->{$symbol}++ or fail("duplicate $kind $symbol");
  $witnesses{$witness}++; $controls{$control}++;
}
my %rocq_source;
for (lines($rocq_path)) {
  next unless /^(?:Theorem|Corollary)\s+(RSDICT_DAWG_\d{3}_[A-Za-z0-9_]+)\s*:/;
  !$rocq_source{$1}++ or fail("duplicate Rocq theorem $1");
}
equal_sets('Rocq theorems', \%rocq_rows, \%rocq_source);
for my $number (0..14) {
  my $prefix = sprintf('RSDICT_DAWG_%03d_', $number);
  scalar(grep { index($_, $prefix) == 0 } keys %rocq_rows) == 1
    or fail("Rocq theorem number $number is missing/duplicated");
}
# Partition every top-level TLA declaration. An added helper must be
# classified explicitly; an added predicate must receive a ledger row.
my %helper = map { $_ => 1 } qw(
  Nodes Threads NoThread ChainEdges SharedEdges EdgeTokens HolderTokens Tokens
  Source Target Outgoing Max vars Strong Top Pending Failed ActivateEdges
  AssignEdges AssignFrame Init ReleaseHolder ConsumeEdge DiscardFailed
  BeginExtracted FinishExtracted FinishDrop Next Spec FairSpec
);
my (%operators, %tla_source);
for (lines($tla_path)) {
  next unless /^([A-Za-z][A-Za-z0-9_]*)(?:\([^)]*\))?\s*==/;
  my $op = $1;
  !$operators{$op}++ or fail("duplicate TLA operator $op");
  $tla_source{$op} = 1 unless $helper{$op};
}
for (keys %helper) { $operators{$_} or fail("stale helper classification $_") }
equal_sets('TLA predicates', \%tla_rows, \%tla_source);
my %configured;
for my $config (@configs) {
  my $section = '';
  my %seen;
  for (lines($config)) {
    if (/^(INVARIANTS|PROPERTIES)$/) { $section = $1; next }
    if (/^[A-Z_]+(?:\s|$)/) { $section = ''; next }
    next unless $section ne '' && /^\s+([A-Za-z][A-Za-z0-9_]*)\s*$/;
    my $name = $1;
    $tla_rows{$name} or fail("unledgered config predicate $name in $config");
    !$seen{$name}++ or fail("duplicate $name in $config");
    $configured{$name}++;
    if ($name eq 'RecursiveDropBound') {
      $config =~ /TryUnwrapUnsafe/ && $section eq 'INVARIANTS'
        or fail('negative control misconfigured');
    }
    if ($name eq 'NativeBound') {
      $config !~ /TryUnwrapUnsafe/ or fail('safe bound in unsafe config');
    }
    if ($name eq 'EventuallyDestroyed') {
      $section eq 'PROPERTIES' or fail('temporal law not checked as property');
    }
  }
  keys(%seen) or fail("no checked predicates in $config");
}
for (keys %tla_rows) {
  next if $_ eq 'AllDestroyed';
  $configured{$_} or fail("unconfigured predicate $_");
}
!$configured{AllDestroyed} or fail('helper AllDestroyed treated as invariant');
$configured{RecursiveDropBound} == 1 or fail('negative control not unique');
$configured{NativeBound} == 2 or fail('both safe shapes must check the bound');
printf "PASS byte-DAWG ledger: %d Rocq theorems, %d TLA predicates, %d witnesses, %d controls\n",
  scalar(keys %rocq_rows), scalar(keys %tla_rows), scalar(keys %witnesses), scalar(keys %controls);
PERL
