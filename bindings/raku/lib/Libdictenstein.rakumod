unit module Libdictenstein;

use NativeCall;

module InteropAccess {
    use Vinary::Tree::Interop;

    our constant UnitDomainType = UnitDomain;
    our constant ByteDomain = BYTE;
    our constant UnicodeDomain = UNICODE-SCALAR;
    our constant U64Domain = U64;
    our constant RawResourceType = RawResource;
    our constant DictionaryType = Dictionary;
    our constant EntriesType = DictionaryEntries;

    our sub borrow($raw) {
        borrow-resource($raw)
    }

    our sub open-dictionary($resource) {
        dictionary($resource, take => True)
    }

    our sub open-entries($dictionary) {
        entries($dictionary)
    }
}

our constant UnitDomain is export = InteropAccess::UnitDomainType;
our constant BYTE is export = InteropAccess::ByteDomain;
our constant UNICODE-SCALAR is export = InteropAccess::UnicodeDomain;
our constant U64 is export = InteropAccess::U64Domain;
my constant RawResource = InteropAccess::RawResourceType;

our constant ABI-VERSION is export = 1;
our constant API-REVISION is export = 6;

our enum DictionaryKind is export (
    DYNAMIC-DAWG => 1,
    DOUBLE-ARRAY-TRIE => 2,
    SCDAWG => 3,
    PERSISTENT-ARTRIE => 4,
    PERSISTENT-VOCABULARY => 5,
);

our enum AlgebraOperation is export (
    ALGEBRA-UNION => 1,
    ALGEBRA-INTERSECTION => 2,
    ALGEBRA-DIFFERENCE => 3,
    ALGEBRA-SYMMETRIC-DIFFERENCE => 4,
);

our enum ValueMerge is export (
    VALUE-MERGE-FIRST => 1,
    VALUE-MERGE-LAST => 2,
    VALUE-MERGE-LATTICE-JOIN => 3,
    VALUE-MERGE-LATTICE-MEET => 4,
);

our constant CAP-READ is export = 1 +< 0;
our constant CAP-INSERT is export = 1 +< 1;
our constant CAP-REMOVE is export = 1 +< 2;
our constant CAP-CLEAR is export = 1 +< 3;
our constant CAP-COMPACT is export = 1 +< 4;
our constant CAP-SUBSTRING is export = 1 +< 5;
our constant CAP-CHECKPOINT is export = 1 +< 6;

class X::Libdictenstein is Exception {
    has Int:D $.status is required;
    has Str:D $.operation is required;
    has Str:D $.detail = '';

    method message(--> Str:D) {
        my $base = "libdictenstein operation '$!operation' failed with status $!status";
        $!detail.chars ?? "$base: $!detail" !! $base
    }
}

class OptionalValue is repr('CStruct') is export {
    has uint64 $.value;
    has uint8 $.has-value;
    has uint8 $.reserved0;
    has uint8 $.reserved1;
    has uint8 $.reserved2;
    has uint8 $.reserved3;
    has uint8 $.reserved4;
    has uint8 $.reserved5;
    has uint8 $.reserved6;
}

role EntryDescriptor {
    has Pointer $.data;
    has size_t $.len;
    has uint64 $.mapped-value;
    has uint8 $.has-value;
    has uint8 $.reserved0;
    has uint8 $.reserved1;
    has uint8 $.reserved2;
    has uint8 $.reserved3;
    has uint8 $.reserved4;
    has uint8 $.reserved5;
    has uint8 $.reserved6;

    submethod BUILD(
        Pointer:D :$data!, Int:D :$len!, Int:D :$mapped-value!,
        Int:D :$has-value!, Int:D :$reserved0 = 0, Int:D :$reserved1 = 0,
        Int:D :$reserved2 = 0, Int:D :$reserved3 = 0,
        Int:D :$reserved4 = 0, Int:D :$reserved5 = 0,
        Int:D :$reserved6 = 0,
    ) {
        $!data := $data;
        $!len = $len;
        $!mapped-value = $mapped-value;
        $!has-value = $has-value;
        $!reserved0 = $reserved0;
        $!reserved1 = $reserved1;
        $!reserved2 = $reserved2;
        $!reserved3 = $reserved3;
        $!reserved4 = $reserved4;
        $!reserved5 = $reserved5;
        $!reserved6 = $reserved6;
    }
}

class TextEntry is repr('CStruct') does EntryDescriptor is export { }
class U64Entry is repr('CStruct') does EntryDescriptor is export { }

sub native-library(--> Str:D) {
    return %*ENV<LIBDICTENSTEIN_LIBRARY>
        if %*ENV<LIBDICTENSTEIN_LIBRARY>:exists;
    $*DISTRO.is-win ?? 'libdictenstein.dll' !!
        $*KERNEL.name eq 'darwin' ?? 'liblibdictenstein.dylib' !!
        'liblibdictenstein.so'
}

sub ldict-abi-version(--> uint32)
    is native(&native-library) is symbol('ldict_abi_version') { * }
sub ldict-api-revision(--> uint32)
    is native(&native-library) is symbol('ldict_api_revision') { * }
sub ldict-last-error-message(--> Str)
    is native(&native-library) is symbol('ldict_last_error_message') { * }
sub ldict-dynamic-dawg-new(uint32, Pointer is rw --> int32)
    is native(&native-library) is symbol('ldict_dynamic_dawg_new') { * }
sub ldict-double-array-trie-new(uint32, Pointer, size_t,
    Pointer is rw --> int32)
    is native(&native-library) is symbol('ldict_double_array_trie_new') { * }
sub ldict-scdawg-new(uint32, Pointer is rw --> int32)
    is native(&native-library) is symbol('ldict_scdawg_new') { * }
sub ldict-persistent-artrie-create(uint32, Pointer, size_t, Pointer is rw --> int32)
    is native(&native-library) is symbol('ldict_persistent_artrie_create') { * }
sub ldict-persistent-artrie-open(uint32, Pointer, size_t, Pointer is rw --> int32)
    is native(&native-library) is symbol('ldict_persistent_artrie_open') { * }
sub ldict-persistent-vocab-create(Pointer, size_t, Pointer is rw --> int32)
    is native(&native-library) is symbol('ldict_persistent_vocab_create') { * }
sub ldict-persistent-vocab-open(Pointer, size_t, Pointer is rw --> int32)
    is native(&native-library) is symbol('ldict_persistent_vocab_open') { * }
sub ldict-dictionary-free(Pointer)
    is native(&native-library) is symbol('ldict_dictionary_free') { * }
sub ldict-dictionary-kind(Pointer, uint32 is rw --> int32)
    is native(&native-library) is symbol('ldict_dictionary_kind') { * }
sub ldict-dictionary-capabilities(Pointer, uint64 is rw --> int32)
    is native(&native-library) is symbol('ldict_dictionary_capabilities') { * }
sub ldict-dictionary-resource(Pointer, RawResource --> int32)
    is native(&native-library) is symbol('ldict_dictionary_resource') { * }
sub ldict-dictionary-algebra(Pointer, Pointer, uint32, uint32, Pointer is rw --> int32)
    is native(&native-library) is symbol('ldict_dictionary_algebra') { * }
sub ldict-dictionary-len(Pointer, size_t is rw --> int32)
    is native(&native-library) is symbol('ldict_dictionary_len') { * }
sub ldict-dictionary-clear(Pointer --> int32)
    is native(&native-library) is symbol('ldict_dictionary_clear') { * }
sub ldict-dictionary-compact(Pointer, size_t is rw --> int32)
    is native(&native-library) is symbol('ldict_dictionary_compact') { * }
sub ldict-dictionary-checkpoint(Pointer --> int32)
    is native(&native-library) is symbol('ldict_dictionary_checkpoint') { * }
sub ldict-vocab-get-term(Pointer, uint64, Pointer, size_t, size_t is rw,
    uint8 is rw --> int32)
    is native(&native-library) is symbol('ldict_vocab_get_term') { * }
sub ldict-dictionary-insert-text(Pointer, Pointer, size_t, OptionalValue,
    uint8 is rw --> int32)
    is native(&native-library) is symbol('ldict_dictionary_insert_text') { * }
sub ldict-dictionary-insert-text-value(Pointer, Pointer, size_t, uint64, uint8,
    uint8 is rw --> int32)
    is native(&native-library) is symbol('ldict_dictionary_insert_text_value') { * }
sub ldict-dictionary-remove-text(Pointer, Pointer, size_t, uint8 is rw --> int32)
    is native(&native-library) is symbol('ldict_dictionary_remove_text') { * }
sub ldict-dictionary-contains-text(Pointer, Pointer, size_t, uint8 is rw --> int32)
    is native(&native-library) is symbol('ldict_dictionary_contains_text') { * }
sub ldict-dictionary-get-text(Pointer, Pointer, size_t, uint8 is rw,
    OptionalValue --> int32)
    is native(&native-library) is symbol('ldict_dictionary_get_text') { * }
sub ldict-dictionary-insert-u64(Pointer, Pointer, size_t, OptionalValue,
    uint8 is rw --> int32)
    is native(&native-library) is symbol('ldict_dictionary_insert_u64') { * }
sub ldict-dictionary-insert-u64-value(Pointer, Pointer, size_t, uint64, uint8,
    uint8 is rw --> int32)
    is native(&native-library) is symbol('ldict_dictionary_insert_u64_value') { * }
sub ldict-dictionary-remove-u64(Pointer, Pointer, size_t, uint8 is rw --> int32)
    is native(&native-library) is symbol('ldict_dictionary_remove_u64') { * }
sub ldict-dictionary-contains-u64(Pointer, Pointer, size_t, uint8 is rw --> int32)
    is native(&native-library) is symbol('ldict_dictionary_contains_u64') { * }
sub ldict-dictionary-get-u64(Pointer, Pointer, size_t, uint8 is rw,
    OptionalValue --> int32)
    is native(&native-library) is symbol('ldict_dictionary_get_u64') { * }
sub ldict-dictionary-insert-text-batch(Pointer, Pointer, size_t,
    size_t is rw --> int32)
    is native(&native-library) is symbol('ldict_dictionary_insert_text_batch') { * }
sub ldict-dictionary-insert-u64-batch(Pointer, Pointer, size_t,
    size_t is rw --> int32)
    is native(&native-library) is symbol('ldict_dictionary_insert_u64_batch') { * }
sub ldict-scdawg-contains-substring(Pointer, Pointer, size_t, uint8 is rw --> int32)
    is native(&native-library) is symbol('ldict_scdawg_contains_substring') { * }
sub ldict-scdawg-substring-frequency(Pointer, Pointer, size_t, size_t is rw --> int32)
    is native(&native-library) is symbol('ldict_scdawg_substring_frequency') { * }
sub memcpy(Pointer, Pointer, size_t --> Pointer) is native { * }

sub abi-version(--> UInt:D) is export { ldict-abi-version().UInt }
sub api-revision(--> UInt:D) is export { ldict-api-revision().UInt }

sub check-status(Int:D $status, Str:D $operation --> Nil) {
    return if $status == 0;
    X::Libdictenstein.new(
        :$status,
        :$operation,
        detail => (try ldict-last-error-message) // '',
    ).throw;
}

sub optional-value(Mu $value --> OptionalValue:D) {
    if $value.defined {
        die 'dictionary value is outside uint64'
            unless $value ~~ Int && 0 <= $value <= 2**64 - 1;
        OptionalValue.new(
            value => $value,
            has-value => 1,
            reserved0 => 0, reserved1 => 0, reserved2 => 0, reserved3 => 0,
            reserved4 => 0, reserved5 => 0, reserved6 => 0,
        )
    } else {
        OptionalValue.new(
            value => 0,
            has-value => 0,
            reserved0 => 0, reserved1 => 0, reserved2 => 0, reserved3 => 0,
            reserved4 => 0, reserved5 => 0, reserved6 => 0,
        )
    }
}

sub text-buffer(Mu:D $key, UnitDomain:D $domain --> Blob:D) {
    return Buf.new($key.list) if $domain == BYTE && $key ~~ Blob;
    return $key.encode('utf8') if $key ~~ Str &&
        ($domain == BYTE || $domain == UNICODE-SCALAR);
    die "key is incompatible with $domain";
}

sub u64-buffer(Mu:D $key, UnitDomain:D $domain --> CArray[uint64]) {
    die 'key is incompatible with U64' unless $domain == U64 && $key ~~ Positional;
    my $buffer = CArray[uint64].allocate($key.elems);
    for $key.list.kv -> $index, $unit {
        die 'key unit is outside uint64' unless $unit ~~ Int && 0 <= $unit <= 2**64 - 1;
        $buffer[$index] = $unit;
    }
    $buffer
}

sub raw-pointer(Blob:D $buffer --> Pointer:D) {
    nativecast(Pointer, $buffer)
}

sub descriptor-arena(Mu $descriptor-type, @descriptors --> Blob:D) {
    my $width = nativesizeof($descriptor-type);
    my $arena = buf8.allocate(@descriptors.elems * $width);
    my $base = raw-pointer($arena);
    for @descriptors.kv -> $index, $descriptor {
        memcpy(
            Pointer.new($base + $index * $width),
            nativecast(Pointer, $descriptor),
            $width,
        );
    }
    $arena
}

sub pairs-from(Mu:D $entries --> Array:D) {
    $entries.list.map({ $_ ~~ Pair ?? $_ !! ($_ => Nil) }).Array
}

class DictionaryIterator does Iterator {
    has InteropAccess::DictionaryType:D $!view is required;
    has InteropAccess::EntriesType:D $!cursor is required;
    has Iterator:D $!iterator is required;
    has UnitDomain:D $.unit-domain is required;
    has Bool $!closed = False;

    submethod BUILD(:$view!, :$cursor!, :$iterator!, :$unit-domain!) {
        $!view := $view;
        $!cursor := $cursor;
        $!iterator := $iterator;
        $!unit-domain := $unit-domain;
    }

    method !close(--> Nil) {
        return if $!closed;
        $!closed = True;
        try $!cursor.close;
        try $!view.close;
    }

    method close(--> Nil) { self!close }

    method pull-one() {
        my \entry = $!iterator.pull-one;
        if entry =:= IterationEnd {
            self!close;
            return IterationEnd;
        }
        my $key = do given $!unit-domain {
            when BYTE { Buf.new(entry.units) }
            when UNICODE-SCALAR { entry.units.map(*.chr).join }
            when U64 { entry.units.Array }
        };
        $key => (entry.value.defined ?? entry.value !! Nil)
    }

    submethod DESTROY { self!close }
}

class Dictionary does Associative does Iterable is export {
    has Pointer $!handle is required;
    has UnitDomain:D $.unit-domain is required;
    has Bool $!closed = False;

    submethod BUILD(Pointer:D :$handle!, UnitDomain:D :$unit-domain!) {
        $!handle = $handle;
        $!unit-domain = $unit-domain;
    }

    method !handle(--> Pointer:D) {
        X::Libdictenstein.new(
            status => 8,
            operation => 'dictionary',
            detail => 'dictionary is closed',
        ).throw if $!closed;
        $!handle
    }

    method close(--> Nil) {
        return if $!closed;
        my $handle = $!handle;
        $!closed = True;
        $!handle = Pointer;
        ldict-dictionary-free($handle) if $handle;
    }

    method opened(--> Bool:D) { !$!closed }
    submethod DESTROY { self.close }

    method kind(--> DictionaryKind:D) {
        my uint32 $output = 0;
        check-status(ldict-dictionary-kind(self!handle, $output), 'dictionary-kind');
        DictionaryKind($output)
    }

    method capabilities(--> UInt:D) {
        my uint64 $output = 0;
        check-status(ldict-dictionary-capabilities(self!handle, $output),
            'dictionary-capabilities');
        $output.UInt
    }

    method elems(--> Int:D) {
        my size_t $output = 0;
        check-status(ldict-dictionary-len(self!handle, $output), 'dictionary-len');
        $output.Int
    }

    method !text-call(Str:D $stem, Mu:D $key --> Bool:D) {
        my $buffer = text-buffer($key, $!unit-domain);
        my uint8 $output = 0;
        my $status = $stem eq 'contains'
            ?? ldict-dictionary-contains-text(self!handle, raw-pointer($buffer),
                $buffer.elems, $output)
            !! ldict-dictionary-remove-text(self!handle, raw-pointer($buffer),
                $buffer.elems, $output);
        check-status($status, "dictionary-{$stem}-text");
        so $output
    }

    method !u64-call(Str:D $stem, Mu:D $key --> Bool:D) {
        my $buffer = u64-buffer($key, $!unit-domain);
        my uint8 $output = 0;
        my $status = $stem eq 'contains'
            ?? ldict-dictionary-contains-u64(self!handle, nativecast(Pointer, $buffer),
                $key.elems, $output)
            !! ldict-dictionary-remove-u64(self!handle, nativecast(Pointer, $buffer),
                $key.elems, $output);
        check-status($status, "dictionary-{$stem}-u64");
        so $output
    }

    method EXISTS-KEY(Mu:D $key --> Bool:D) {
        $!unit-domain == U64 ?? self!u64-call('contains', $key) !!
            self!text-call('contains', $key)
    }

    method AT-KEY(Mu:D $key --> Mu) {
        my uint8 $found = 0;
        my $value = OptionalValue.new;
        my $status;
        if $!unit-domain == U64 {
            my $buffer = u64-buffer($key, $!unit-domain);
            $status = ldict-dictionary-get-u64(self!handle,
                nativecast(Pointer, $buffer), $key.elems, $found, $value);
        } else {
            my $buffer = text-buffer($key, $!unit-domain);
            $status = ldict-dictionary-get-text(self!handle, raw-pointer($buffer),
                $buffer.elems, $found, $value);
        }
        check-status($status, 'dictionary-get');
        return Nil unless $found;
        $value.has-value ?? $value.value.UInt !! Nil
    }

    method ASSIGN-KEY(Mu:D $key, Mu $value --> Mu) {
        my uint8 $inserted = 0;
        my $optional = optional-value($value);
        my $status;
        if $!unit-domain == U64 {
            my $buffer = u64-buffer($key, $!unit-domain);
            $status = ldict-dictionary-insert-u64-value(self!handle,
                nativecast(Pointer, $buffer), $key.elems, $optional.value,
                $optional.has-value, $inserted);
        } else {
            my $buffer = text-buffer($key, $!unit-domain);
            $status = ldict-dictionary-insert-text-value(self!handle,
                raw-pointer($buffer), $buffer.elems, $optional.value,
                $optional.has-value, $inserted);
        }
        check-status($status, 'dictionary-insert');
        $value
    }

    method DELETE-KEY(Mu:D $key --> Mu) {
        my $present = self.EXISTS-KEY($key);
        my $old = $present ?? self.AT-KEY($key) !! Nil;
        $!unit-domain == U64 ?? self!u64-call('remove', $key) !!
            self!text-call('remove', $key);
        $old
    }

    method clear(--> Dictionary:D) {
        check-status(ldict-dictionary-clear(self!handle), 'dictionary-clear');
        self
    }

    method compact(--> Int:D) {
        my size_t $reclaimed = 0;
        check-status(ldict-dictionary-compact(self!handle, $reclaimed),
            'dictionary-compact');
        $reclaimed.Int
    }

    method checkpoint(--> Dictionary:D) {
        check-status(ldict-dictionary-checkpoint(self!handle), 'dictionary-checkpoint');
        self
    }

    method snapshot(--> InteropAccess::DictionaryType:D) {
        my $raw = RawResource.new;
        check-status(ldict-dictionary-resource(self!handle, $raw), 'dictionary-resource');
        my $owned = InteropAccess::borrow($raw);
        my $live = InteropAccess::open-dictionary($owned);
        LEAVE $live.close;
        $live.snapshot
    }

    method iterator(--> Iterator:D) {
        my $view = self.snapshot;
        my $cursor = InteropAccess::open-entries($view);
        DictionaryIterator.new(
            :$view,
            :$cursor,
            iterator => $cursor.iterator,
            unit-domain => $!unit-domain,
        )
    }

    method Seq(--> Seq:D) { Seq.new(self.iterator) }
    method list(--> List:D) { self.Seq.list }

    method insert-batch(Mu:D $entries --> Int:D) {
        my @pairs = pairs-from($entries);
        my size_t $inserted = 0;
        if $!unit-domain == U64 {
            my @buffers;
            @buffers.push(u64-buffer(.key, $!unit-domain)) for @pairs;
            my @descriptors;
            for @pairs.kv -> $index, $pair {
                my $optional = optional-value($pair.value);
                @descriptors.push(U64Entry.new(
                    data => nativecast(Pointer, @buffers[$index]),
                    len => $pair.key.elems,
                    mapped-value => $optional.value,
                    has-value => $optional.has-value,
                    reserved0 => 0, reserved1 => 0, reserved2 => 0, reserved3 => 0,
                    reserved4 => 0, reserved5 => 0, reserved6 => 0,
                ));
            }
            my $arena = descriptor-arena(U64Entry, @descriptors);
            check-status(ldict-dictionary-insert-u64-batch(self!handle,
                raw-pointer($arena),
                @pairs.elems, $inserted), 'dictionary-insert-u64-batch');
        } else {
            my @buffers;
            @buffers.push(text-buffer(.key, $!unit-domain)) for @pairs;
            my @descriptors;
            for @pairs.kv -> $index, $pair {
                my $optional = optional-value($pair.value);
                @descriptors.push(TextEntry.new(
                    data => raw-pointer(@buffers[$index]),
                    len => @buffers[$index].elems,
                    mapped-value => $optional.value,
                    has-value => $optional.has-value,
                    reserved0 => 0, reserved1 => 0, reserved2 => 0, reserved3 => 0,
                    reserved4 => 0, reserved5 => 0, reserved6 => 0,
                ));
            }
            my $arena = descriptor-arena(TextEntry, @descriptors);
            check-status(ldict-dictionary-insert-text-batch(self!handle,
                raw-pointer($arena),
                @pairs.elems, $inserted), 'dictionary-insert-text-batch');
        }
        $inserted.Int
    }

    method algebra(Dictionary:D $right, AlgebraOperation:D $operation = ALGEBRA-UNION,
        ValueMerge:D $merge = VALUE-MERGE-LAST --> Dictionary:D) {
        die 'dictionary domains must match' unless $!unit-domain == $right.unit-domain;
        my Pointer $output .= new;
        check-status(ldict-dictionary-algebra(self!handle, $right!handle,
            $operation.Int, $merge.Int, $output), 'dictionary-algebra');
        Dictionary.new(handle => $output, unit-domain => $!unit-domain)
    }

    method union(Dictionary:D $right, ValueMerge:D :$merge = VALUE-MERGE-LAST
        --> Dictionary:D) {
        self.algebra($right, ALGEBRA-UNION, $merge)
    }

    method intersection(Dictionary:D $right,
        ValueMerge:D :$merge = VALUE-MERGE-LATTICE-MEET --> Dictionary:D) {
        self.algebra($right, ALGEBRA-INTERSECTION, $merge)
    }

    method difference(Dictionary:D $right --> Dictionary:D) {
        self.algebra($right, ALGEBRA-DIFFERENCE, VALUE-MERGE-FIRST)
    }

    method symmetric-difference(Dictionary:D $right --> Dictionary:D) {
        self.algebra($right, ALGEBRA-SYMMETRIC-DIFFERENCE, VALUE-MERGE-FIRST)
    }

    method contains-substring(Str:D $pattern --> Bool:D) {
        my $buffer = $pattern.encode('utf8');
        my uint8 $output = 0;
        check-status(ldict-scdawg-contains-substring(self!handle, raw-pointer($buffer),
            $buffer.elems, $output), 'scdawg-contains-substring');
        so $output
    }

    method substring-frequency(Str:D $pattern --> Int:D) {
        my $buffer = $pattern.encode('utf8');
        my size_t $output = 0;
        check-status(ldict-scdawg-substring-frequency(self!handle, raw-pointer($buffer),
            $buffer.elems, $output), 'scdawg-substring-frequency');
        $output.Int
    }

    method vocabulary-term(UInt:D $index --> Str) {
        my size_t $required = 0;
        my uint8 $found = 0;
        check-status(ldict-vocab-get-term(self!handle, $index, Pointer, 0,
            $required, $found), 'vocab-get-term');
        return Str unless $found;
        my $buffer = buf8.allocate($required);
        check-status(ldict-vocab-get-term(self!handle, $index,
            nativecast(Pointer, $buffer), $required, $required, $found), 'vocab-get-term');
        $found ?? $buffer.decode('utf8') !! Str
    }
}

sub dictionary-from(Pointer:D $handle, UnitDomain:D $domain --> Dictionary:D) {
    X::Libdictenstein.new(
        status => 4,
        operation => 'constructor',
        detail => 'native constructor returned null',
    ).throw unless $handle;
    Dictionary.new(:$handle, unit-domain => $domain)
}

sub dynamic-dawg(UnitDomain:D $domain = UNICODE-SCALAR --> Dictionary:D) is export {
    my Pointer $output .= new;
    check-status(ldict-dynamic-dawg-new($domain.Int, $output), 'dynamic-dawg-new');
    dictionary-from($output, $domain)
}

sub scdawg(UnitDomain:D $domain = UNICODE-SCALAR --> Dictionary:D) is export {
    my Pointer $output .= new;
    check-status(ldict-scdawg-new($domain.Int, $output), 'scdawg-new');
    dictionary-from($output, $domain)
}

sub double-array-trie(Mu:D $entries, UnitDomain:D :$domain = UNICODE-SCALAR
    --> Dictionary:D) is export {
    die 'DoubleArrayTrie does not support U64' if $domain == U64;
    my @pairs = pairs-from($entries);
    my @buffers;
    @buffers.push(text-buffer(.key, $domain)) for @pairs;
    my @descriptors;
    for @pairs.kv -> $index, $pair {
        my $optional = optional-value($pair.value);
        @descriptors.push(TextEntry.new(
            data => raw-pointer(@buffers[$index]),
            len => @buffers[$index].elems,
            mapped-value => $optional.value,
            has-value => $optional.has-value,
            reserved0 => 0, reserved1 => 0, reserved2 => 0, reserved3 => 0,
            reserved4 => 0, reserved5 => 0, reserved6 => 0,
        ));
    }
    my $arena = descriptor-arena(TextEntry, @descriptors);
    my Pointer $output .= new;
    check-status(ldict-double-array-trie-new($domain.Int, raw-pointer($arena),
        @pairs.elems, $output), 'double-array-trie-new');
    dictionary-from($output, $domain)
}

sub persistent-artrie(IO::Path:D $path, UnitDomain:D :$domain = UNICODE-SCALAR,
    Bool:D :$create = True --> Dictionary:D) is export {
    my $buffer = $path.absolute.Str.encode('utf8');
    my Pointer $output .= new;
    my $status = $create
        ?? ldict-persistent-artrie-create($domain.Int, raw-pointer($buffer),
            $buffer.elems, $output)
        !! ldict-persistent-artrie-open($domain.Int, raw-pointer($buffer),
            $buffer.elems, $output);
    check-status($status, $create ?? 'persistent-artrie-create' !! 'persistent-artrie-open');
    dictionary-from($output, $domain)
}

sub persistent-vocabulary(IO::Path:D $path, Bool:D :$create = True
    --> Dictionary:D) is export {
    my $buffer = $path.absolute.Str.encode('utf8');
    my Pointer $output .= new;
    my $status = $create
        ?? ldict-persistent-vocab-create(raw-pointer($buffer), $buffer.elems, $output)
        !! ldict-persistent-vocab-open(raw-pointer($buffer), $buffer.elems, $output);
    check-status($status, $create ?? 'persistent-vocab-create' !! 'persistent-vocab-open');
    dictionary-from($output, UNICODE-SCALAR)
}

multi sub infix:<(|)>(Dictionary:D $left, Dictionary:D $right --> Dictionary:D)
    is export {
    $left.union($right)
}

multi sub infix:<(&)>(Dictionary:D $left, Dictionary:D $right --> Dictionary:D)
    is export {
    $left.intersection($right)
}

=begin pod

=TITLE Libdictenstein

High-performance dictionaries and trie-maps for approximate string matching.

C<Dictionary> implements C<Associative> and C<Iterable>. Constructors return
owned native handles; call C<close> deterministically, with C<DESTROY> as a
fallback. C<insert-batch> crosses the NativeCall boundary once. Algebra methods
capture immutable revisions and materialize their linear merge as an optimized
mutable DynamicDAWG.

=end pod
