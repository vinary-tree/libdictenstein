require "thread"
require_relative "libdictenstein/version"
require_relative "libdictenstein/native"

# Namespace for the Vinary Tree language bindings.
module VinaryTree
  # Ruby collections over libdictenstein's versioned native dictionary ABI.
  #
  # A dictionary owns a native handle. Close it explicitly to release native
  # memory promptly; a finalizer is only a fallback. A missing key differs from
  # a present key whose optional value is +nil+:
  #
  #   words = VinaryTree::Libdictenstein::DynamicDawg.new
  #   begin
  #     words.put("colour", 17)
  #     words.put("color", nil)
  #     words.get("color").found? # => true
  #     words.get("missing").found? # => false
  #   ensure
  #     words.close
  #   end
  #
  # Text keys use +BYTE+ or +UNICODE_SCALAR+ units. Token keys use +U64+ and
  # the explicit +_u64+ methods; their arrays contain unsigned 64-bit integers.
  module Libdictenstein
    # Treat each byte as one transition unit, including bytes outside UTF-8.
    BYTE = 1
    # Treat each Unicode scalar value as one transition unit.
    UNICODE_SCALAR = 2
    # Treat each unsigned 64-bit integer as one transition unit.
    U64 = 3

    # Set operation selected by Dictionary#algebra.
    module AlgebraOperation
      # Keep keys present in either input.
      UNION = 1
      # Keep keys present in both inputs.
      INTERSECTION = 2
      # Keep keys present only in the left input.
      DIFFERENCE = 3
      # Keep keys present in exactly one input.
      SYMMETRIC_DIFFERENCE = 4
    end

    # Policy for a key present in both input dictionaries during algebra.
    module ValueMerge
      # Retain the left input's optional value.
      FIRST = 1
      # Retain the right input's optional value.
      LAST = 2
      # Treat +nil+ as bottom; otherwise choose the greater unsigned value.
      LATTICE_JOIN = 3
      # Return +nil+ unless both values exist; otherwise choose the lesser.
      LATTICE_MEET = 4
    end

    # Return the loaded library's native ABI major version.
    # This family currently uses version 1.
    def self.abi_version
      Native.ldict_abi_version
    end

    # Return the loaded library's compatible-additions API revision.
    # PathMap requires revision 8 or later.
    def self.api_revision
      Native.ldict_api_revision
    end

    # One lookup result. +found?+ distinguishes an absent key from a present
    # key whose optional unsigned value is +nil+.
    Lookup = Data.define(:found?, :value)
    # One copied dictionary entry. +key+ is a String for text dictionaries or
    # an Array of unsigned integers for a +U64+ dictionary.
    Entry = Data.define(:key, :value, :domain)
    # Cursor metadata: unit domain, optional exact length, and optional
    # snapshot identity as a producer-ID and revision-ID pair.
    EntryInfo = Data.define(:domain, :exact_length, :snapshot_identity)

    # A nonzero native status with the status code and native error text.
    class Error < StandardError
      # Native status code returned by the failed operation.
      attr_reader :status

      # Construct an error from +status+ and an optional explicit +message+.
      def initialize(status, message = nil)
        @status = status
        super(message || "libdictenstein status #{status}: #{Native.ldict_last_error_message.to_s}")
      end
    end

    # Raise Error for a nonzero native status.
    def self.check(status)
      raise Error, status unless status.zero?
    end

    class ConcurrentHandle # :nodoc:
      def initialize(pointer)
        @pointer = pointer
        @active = 0
        @closing = false
        @mutex = Mutex.new
        @condition = ConditionVariable.new
      end

      def with_pointer
        pointer = @mutex.synchronize do
          raise IOError, "dictionary is closed" if @closing || @pointer.zero?
          @active += 1
          @pointer
        end
        yield pointer
      ensure
        @mutex.synchronize do
          @active -= 1
          @condition.broadcast if @active.zero?
        end if pointer
      end

      def close
        pointer = @mutex.synchronize do
          return 0 if @pointer.zero?
          @closing = true
          @condition.wait(@mutex) until @active.zero?
          result = @pointer
          @pointer = 0
          result
        end
        Native.ldict_dictionary_free(pointer) unless pointer.zero?
        pointer
      end
    end

    class EntryCursorState # :nodoc:
      attr_reader :info

      def initialize(handle, max_entries:, max_units:, max_values:)
        raise ArgumentError, "max_entries must be positive" unless max_entries.positive?
        raise ArgumentError, "entry batch limits must be nonnegative" if max_units.negative? || max_values.negative?

        @mutex = Mutex.new
        @cursor = 0
        @leased = false
        @ended = false
        @index = 0
        @limits_memory = Fiddle::Pointer.malloc(Native::EntryBatchLimits.size, Fiddle::RUBY_FREE)
        @limits = Native::EntryBatchLimits.new(@limits_memory)
        @limits.max_entries = max_entries
        @limits.max_units = max_units
        @limits.max_values = max_values
        @limits.reserved = 0
        @batch_memory = Fiddle::Pointer.malloc(Native::EntryBatch.size, Fiddle::RUBY_FREE)
        @batch = Native::EntryBatch.new(@batch_memory)
        info_memory = Fiddle::Pointer.malloc(Native::EntriesInfo.size, Fiddle::RUBY_FREE)
        native_info = Native::EntriesInfo.new(info_memory)
        cursor_output = Native.pointer_output
        handle.with_pointer do |dictionary|
          Libdictenstein.check(
            Native.ldict_dictionary_entries_open(dictionary, cursor_output, info_memory)
          )
        end
        @cursor = Native.read_pointer(cursor_output)
        flags = native_info.flags
        @info = EntryInfo.new(
          native_info.unit_domain,
          flags.anybits?(1) ? native_info.exact_len : nil,
          flags.anybits?(2) ? [native_info.identity_producer, native_info.identity_revision].freeze : nil
        )
      end

      def next_entry
        @mutex.synchronize do
          return nil if @cursor.zero? || @ended
          begin
            unless @leased
              status = Native.ldict_entry_cursor_next(@cursor, @limits_memory, @batch_memory)
              if status == 1
                @ended = true
                close_locked(cancel: false)
                return nil
              end
              Libdictenstein.check(status)
              @leased = true
              @index = 0
            end
            result = copy_entry(@index)
            @index += 1
            release_locked if @index == @batch.entry_count
            result
          rescue Exception
            close_locked(cancel: true) rescue nil
            raise
          end
        end
      end

      def cancel
        @mutex.synchronize do
          return nil if @cursor.zero? || @ended
          first_error = native_error(Native.ldict_entry_cursor_cancel(@cursor))
          begin
            release_locked
          rescue Exception => error
            first_error ||= error
          end
          @ended = true
          raise first_error if first_error
        end
        nil
      end

      def close
        @mutex.synchronize { close_locked(cancel: true) }
      end

      def closed?
        @mutex.synchronize { @cursor.zero? }
      end

      private

      def address(pointer)
        return 0 if pointer.nil?
        pointer.respond_to?(:to_i) ? pointer.to_i : Integer(pointer)
      end

      def checked_range(offset, length, total, name)
        unless offset >= 0 && length >= 0 && offset <= total && length <= total - offset
          raise RuntimeError, "invalid native #{name} arena range"
        end
        offset...(offset + length)
      end

      def copy_entry(index)
        raise RuntimeError, "invalid native entry descriptor index" unless index.between?(0, @batch.entry_count - 1)
        # `entries` collides with Fiddle::CStruct#entries; indexed field access
        # selects the actual pointer member.
        entries_address = address(@batch["entries"])
        raise RuntimeError, "native entry descriptor array is null" if entries_address.zero?
        descriptor = Native::DictionaryEntry.new(
          Fiddle::Pointer.new(entries_address + index * Native::DictionaryEntry.size)
        )
        range = checked_range(descriptor.unit_offset, descriptor.unit_len, @batch.unit_count, "unit")
        unit_address = address(@batch.units)
        raise RuntimeError, "native unit arena is null" if range.size.positive? && unit_address.zero?
        key = case @info.domain
              when BYTE
                range.size.zero? ? "".b : Fiddle::Pointer.new(unit_address + range.begin)[0, range.size].b
              when UNICODE_SCALAR
                scalars = range.size.zero? ? [] : Fiddle::Pointer.new(unit_address + range.begin * 4)[0, range.size * 4].unpack("L*")
                scalars.pack("U*")
              when U64
                range.size.zero? ? [] : Fiddle::Pointer.new(unit_address + range.begin * 8)[0, range.size * 8].unpack("Q*")
              else
                raise RuntimeError, "unknown native entry unit domain #{@info.domain}"
              end
        value = case descriptor.value_len
                when 0
                  nil
                when 1
                  value_range = checked_range(descriptor.value_offset, 1, @batch.value_count, "value")
                  values_address = address(@batch.values)
                  raise RuntimeError, "native value arena is null" if values_address.zero?
                  Fiddle::Pointer.new(values_address + value_range.begin * 8)[0, 8].unpack1("Q")
                else
                  raise RuntimeError, "invalid native optional-u64 descriptor"
                end
        Entry.new(key, value, @info.domain)
      end

      def release_locked
        return nil unless @leased
        Libdictenstein.check(Native.ldict_entry_cursor_release(@cursor, @batch.generation))
        @leased = false
        @index = 0
        nil
      end

      def native_error(status)
        status.zero? ? nil : Error.new(status)
      end

      def close_locked(cancel:)
        return nil if @cursor.zero?
        first_error = cancel ? native_error(Native.ldict_entry_cursor_cancel(@cursor)) : nil
        begin
          release_locked
        rescue Exception => error
          first_error ||= error
        end
        free_error = native_error(Native.ldict_entry_cursor_free(@cursor))
        if free_error.nil?
          @cursor = 0
          @ended = true
        else
          first_error ||= free_error
        end
        raise first_error if first_error
        nil
      end
    end

    # A bounded, snapshot-consistent stream of copied Entry records.
    #
    # The cursor pins one native dictionary revision. Each entry's key and
    # optional value are copied before the native page lease is released.
    # +each+ closes the cursor even when the block breaks or raises; callers
    # using +next+ directly should close it in +ensure+:
    #
    #   stream = dictionary.entry_stream(max_entries: 64)
    #   begin
    #     while (entry = stream.next)
    #       break if entry.key == "stop"
    #     end
    #   ensure
    #     stream.close
    #   end
    class EntryStream
      include Enumerable

      # The cursor's EntryInfo, including its optional snapshot identity.
      attr_reader :info

      # Open a native cursor with positive +max_entries+ and nonnegative
      # +max_units+ and +max_values+ arena limits. The source dictionary may
      # be mutated afterward without changing this stream's captured revision.
      def initialize(handle, max_entries:, max_units:, max_values:)
        @state = EntryCursorState.new(
          handle,
          max_entries: max_entries,
          max_units: max_units,
          max_values: max_values
        )
        @info = @state.info
        ObjectSpace.define_finalizer(self, self.class.finalizer(@state))
      end

      # Finalizer fallback for an abandoned stream; prefer #close.
      def self.finalizer(state)
        proc do
          state.close
        rescue StandardError
          nil
        end
      end

      # Return the next copied Entry, or +nil+ after exhaustion.
      def next
        @state.next_entry
      end

      # Yield copied Entry records in dictionary order and close on every exit.
      # With no block, return an Enumerator that closes when iterated.
      def each
        return enum_for(__method__) unless block_given?
        begin
          while (entry = self.next)
            yield entry
          end
        ensure
          close
        end
        self
      end

      # Stop an incomplete traversal; return +nil+ on success. Call +close+
      # afterward to free the cursor itself, or use +each+ for automatic close.
      def cancel
        @state.cancel
      end

      # Release the native cursor. Repeated calls are harmless.
      def close
        @state.close
        ObjectSpace.undefine_finalizer(self) if @state.closed?
        nil
      end
    end

    # Shared collection and algebra surface for all native dictionary kinds.
    #
    # Instances own their native handles and include Enumerable. +each+ yields
    # Entry objects, not Ruby key/value pairs. A closed dictionary rejects
    # subsequent native operations; call +close+ in +ensure+ for deterministic
    # release instead of relying on the fallback finalizer.
    class Dictionary
      include Enumerable

      # Internal concurrent native handle; use +with_resource+ for a scoped
      # interoperable resource or +entry_stream+ for a snapshot cursor.
      attr_reader :handle

      # Wrap an owned native pointer. Application code should use a concrete
      # constructor such as DynamicDawg.new rather than this base constructor.
      def initialize(pointer)
        @handle = ConcurrentHandle.new(pointer)
        ObjectSpace.define_finalizer(self, self.class.finalizer(@handle))
      end
      # Finalizer fallback for an abandoned dictionary; prefer #close.
      def self.finalizer(handle)
        proc do
          handle.close
        rescue StandardError
          nil
        end
      end

      # Release the owned native handle after active calls complete. Closing
      # twice is harmless; already-opened retained snapshots remain separate.
      def close
        @handle.close
        ObjectSpace.undefine_finalizer(self)
        nil
      end

      # Yield the borrowed context and vtable addresses of the versioned
      # dictionary resource while this dictionary remains open. A foreign
      # consumer must retain the resource before keeping it beyond the block.
      # Do not dereference either address after the block without a retain.
      def with_resource
        @handle.with_pointer do |pointer|
          resource = Native::Resource.malloc
          Libdictenstein.check(Native.ldict_dictionary_resource(pointer, resource))
          yield resource.context.to_i, resource.vtable.to_i
        end
      end

      # Return the native backend kind code (for example, DynamicDAWG is 1).
      def kind
        scalar(:ldict_dictionary_kind, Native.u64_output, ->(output) { Native.read_u64(output) & 0xffff_ffff })
      end
      # Return the native capability bitset; inspect it before optional calls.
      def capabilities
        scalar(:ldict_dictionary_capabilities, Native.u64_output, Native.method(:read_u64))
      end
      # Return the exact number of terminal dictionary keys.
      def length
        scalar(:ldict_dictionary_len, Native.size_output, Native.method(:read_size))
      end
      # Alias for #length.
      alias size length

      # Capture an immutable native revision and return a bounded EntryStream.
      # Limits are per page, not a cap on the total number of results. Always
      # close a manually pulled stream, including after early termination.
      def entry_stream(max_entries: 256, max_units: 4096, max_values: 256)
        EntryStream.new(
          @handle,
          max_entries: max_entries,
          max_units: max_units,
          max_values: max_values
        )
      end

      # Yield copied Entry records from one immutable revision. With no block,
      # return an Enumerator. Traversal closes its cursor on break or error.
      def each
        stream = entry_stream
        return stream.each unless block_given?
        stream.each { |entry| yield entry }
        self
      end

      # Materialize copied Entry records from a fresh immutable revision.
      def entries
        each.to_a
      end
      # Materialize the keys from a fresh immutable revision.
      def keys
        entries.map(&:key)
      end
      # Materialize the optional values from a fresh immutable revision.
      def values
        entries.map(&:value)
      end

      # Merge two captured dictionary revisions using a native set operation.
      # Both dictionaries must use the same unit domain. Return an independently
      # owned, mutable DynamicDawg; close the result separately from the inputs.
      # +value_merge+ applies only to keys present in both inputs.
      def algebra(right, operation: AlgebraOperation::UNION, value_merge: ValueMerge::LAST)
        raise TypeError, "right must be a dictionary" unless right.is_a?(Dictionary)
        output = Native.pointer_output
        @handle.with_pointer do |left_pointer|
          right.handle.with_pointer do |right_pointer|
            Libdictenstein.check(
              Native.ldict_dictionary_algebra(
                left_pointer, right_pointer, operation, value_merge, output
              )
            )
          end
        end
        DynamicDawg.adopt(Native.read_pointer(output))
      end

      # Return all keys from both dictionaries (equivalent to the +|+ operator).
      # Duplicate keys default to the right input's optional value.
      def union(right, value_merge: ValueMerge::LAST)
        algebra(right, operation: AlgebraOperation::UNION, value_merge: value_merge)
      end

      # Return shared keys (equivalent to the +&+ operator). Values default to
      # optional-value lattice meet: +nil+ unless both sides carry a value.
      def intersection(right, value_merge: ValueMerge::LATTICE_MEET)
        algebra(right, operation: AlgebraOperation::INTERSECTION, value_merge: value_merge)
      end

      # Return keys only in this dictionary (equivalent to the +-+ operator).
      def difference(right)
        algebra(right, operation: AlgebraOperation::DIFFERENCE, value_merge: ValueMerge::FIRST)
      end

      # Return keys in exactly one input (equivalent to the +^+ operator).
      def symmetric_difference(right)
        algebra(
          right,
          operation: AlgebraOperation::SYMMETRIC_DIFFERENCE,
          value_merge: ValueMerge::FIRST
        )
      end

      # Native union with the default right-value policy; close the result.
      def |(right)
        union(right)
      end
      # Native intersection with the default optional-value meet policy.
      def &(right)
        intersection(right)
      end
      # Native left difference; close the result.
      def -(right)
        difference(right)
      end
      # Native symmetric difference; close the result.
      def ^(right)
        symmetric_difference(right)
      end

      # Test membership of a text key; a present key may still have a +nil+
      # value. Use +include_u64?+ for token-domain dictionaries.
      def include?(term)
        output = Native.byte_output
        @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_dictionary_contains_text(pointer, term.b, term.bytesize, output)) }
        output[0].positive?
      end

      # Return Lookup for a text key. Check +found?+ before interpreting
      # +value+, because +nil+ can mean either absence or valueless membership.
      def get(term)
        found, value, present = Native.byte_output, Native.u64_output, Native.byte_output
        @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_dictionary_get_text_value(pointer, term.b, term.bytesize, found, value, present)) }
        Lookup.new(found[0].positive?, present[0].positive? ? Native.read_u64(value) : nil)
      end

      # Test membership of an array of unsigned 64-bit token units.
      def include_u64?(tokens)
        packed = tokens.pack("Q*"); output = Native.byte_output
        @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_dictionary_contains_u64(pointer, packed, tokens.length, output)) }
        output[0].positive?
      end

      # Return Lookup for an unsigned 64-bit token sequence.
      def get_u64(tokens)
        packed = tokens.pack("Q*"); found, value, present = Native.byte_output, Native.u64_output, Native.byte_output
        @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_dictionary_get_u64_value(pointer, packed, tokens.length, found, value, present)) }
        Lookup.new(found[0].positive?, present[0].positive? ? Native.read_u64(value) : nil)
      end

      private

      def scalar(function, output, decode)
        @handle.with_pointer { |pointer| Libdictenstein.check(Native.public_send(function, pointer, output)) }
        decode.call(output)
      end

      def put_text(term, value)
        output = Native.byte_output
        @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_dictionary_insert_text_value(pointer, term.b, term.bytesize, value || 0, value.nil? ? 0 : 1, output)) }
        output[0].positive?
      end

      def remove_text(term)
        output = Native.byte_output
        @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_dictionary_remove_text(pointer, term.b, term.bytesize, output)) }
        output[0].positive?
      end

      def put_tokens(tokens, value)
        packed = tokens.pack("Q*"); output = Native.byte_output
        @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_dictionary_insert_u64_value(pointer, packed, tokens.length, value || 0, value.nil? ? 0 : 1, output)) }
        output[0].positive?
      end

      def remove_tokens(tokens)
        packed = tokens.pack("Q*"); output = Native.byte_output
        @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_dictionary_remove_u64(pointer, packed, tokens.length, output)) }
        output[0].positive?
      end
    end

    # Text-key mutation shared by DynamicDawg and PathMap. Values are optional
    # unsigned 64-bit integers: +nil+ denotes a present, valueless key.
    module MutableTextDictionary
      # Insert or update +term+ and its optional +value+. Return the native
      # insertion flag; a repeat of an unchanged entry returns +false+.
      def put(term, value = nil)
        put_text(term, value)
      end
      # Remove +term+; return +true+ only if a key was removed.
      def remove(term)
        remove_text(term)
      end
      # Remove all entries. Unsupported backend capabilities raise Error.
      def clear
        @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_dictionary_clear(pointer)) }
      end
      # Rebuild native storage and return the number of reclaimed nodes.
      # Unsupported backend capabilities raise Error.
      def compact
        output = Native.size_output; @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_dictionary_compact(pointer, output)) }; Native.read_size(output)
      end
      # Submit an array of two-element [term, value] pairs in one native batch
      # and return the native insertion count. Terms must be String keys in this backend's
      # unit domain; use +nil+ for a present key without a mapped value.
      def put_all(entries)
        terms = entries.map { |term, _value| term.b }
        memory = Fiddle::Pointer.malloc([1, entries.length * Native::TextEntry.size].max, Fiddle::RUBY_FREE)
        entries.each_with_index do |(_term, value), index|
          item = Native::TextEntry.new(memory + index * Native::TextEntry.size)
          item.data = Fiddle::Pointer[terms[index]]
          item.len = terms[index].bytesize
          item.value = value || 0
          item.has_value = value.nil? ? 0 : 1
        end
        output = Native.size_output
        @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_dictionary_insert_text_batch(pointer, memory, entries.length, output)) }
        Native.read_size(output)
      end
    end

    # Mutable minimal directed acyclic word graph for exact keys.
    #
    # Use +put_all+ to amortize foreign calls during bulk construction.
    # +U64+ dictionaries use +put_u64+, +remove_u64+, +get_u64+, and
    # +include_u64?+ rather than the String-key methods.
    class DynamicDawg < Dictionary
      include MutableTextDictionary

      # Adopt an owned native pointer returned by dictionary algebra. The
      # public constructor is +new+; callers should not manufacture pointers.
      def self.adopt(pointer)
        new(pointer: pointer)
      end

      # Create a new dictionary in +BYTE+, +UNICODE_SCALAR+ (default), or
      # +U64+ domain. +pointer+ is for internally adopting a native result.
      def initialize(domain: UNICODE_SCALAR, pointer: nil)
        unless pointer
          output = Native.pointer_output
          Libdictenstein.check(Native.ldict_dynamic_dawg_new(domain, output))
          pointer = Native.read_pointer(output)
        end
        super(pointer)
      end
      # Insert an unsigned 64-bit token sequence and optional mapped value.
      def put_u64(tokens, value = nil)
        put_tokens(tokens, value)
      end
      # Remove an unsigned 64-bit token sequence; report whether it existed.
      def remove_u64(tokens)
        remove_tokens(tokens)
      end
    end

    # Mutable path-compressed trie-map for exact String keys.
    #
    # +BYTE+ mode preserves arbitrary bytes; +UNICODE_SCALAR+ mode requires
    # valid UTF-8. +U64+ is not supported. Native API revision 8 is required.
    #
    #   map = VinaryTree::Libdictenstein::PathMap.new(domain: VinaryTree::Libdictenstein::BYTE)
    #   begin
    #     map.put("\xff\x00".b, nil)
    #     map.include?("\xff\x00".b) # => true
    #   ensure
    #     map.close
    #   end
    class PathMap < Dictionary
      include MutableTextDictionary

      # Create a byte or Unicode-scalar PathMap, or raise Error for an
      # unsupported domain or an older native API revision.
      def initialize(domain: UNICODE_SCALAR)
        raise Error.new(6, "PathMap does not support u64-token keys") if domain == U64
        if Libdictenstein.api_revision < 8 || !Native.respond_to?(:ldict_pathmap_new)
          raise Error.new(6, "PathMap requires a native revision-8 library with ldict_pathmap_new")
        end
        output = Native.pointer_output
        Libdictenstein.check(Native.ldict_pathmap_new(domain, output))
        super(Native.read_pointer(output))
      end
    end

    # Reserved Ruby type for the distinct suffix-source index ABI.
    # The Ruby binding does not yet expose typed source snapshots; creating
    # this class raises Error with native UNSUPPORTED status (6). Do not use
    # it as a substitute for a dictionary or SCDAWG.
    class SuffixIndex
      # Raise Error because typed suffix-source snapshots are unavailable.
      def initialize(domain: UNICODE_SCALAR)
        raise Error.new(6, "typed suffix-source snapshots are not exposed by the Ruby binding")
      end
    end

    # Immutable, read-optimized double-array trie built from text entries.
    # Pass an array of two-element [term, value] pairs at construction;
    # mutation methods are not available. Close the dictionary after use.
    class DoubleArrayTrie < Dictionary
      # Build a text-key trie in +BYTE+ or +UNICODE_SCALAR+ domain.
      def initialize(entries, domain: UNICODE_SCALAR)
        terms = entries.map { |term, _value| term.b }
        memory = Fiddle::Pointer.malloc([1, entries.length * Native::TextEntry.size].max, Fiddle::RUBY_FREE)
        entries.each_with_index do |(_term, value), index|
          item = Native::TextEntry.new(memory + index * Native::TextEntry.size)
          item.data = Fiddle::Pointer[terms[index]]; item.len = terms[index].bytesize; item.value = value || 0; item.has_value = value.nil? ? 0 : 1
        end
        output = Native.pointer_output; Libdictenstein.check(Native.ldict_double_array_trie_new(domain, memory, entries.length, output)); super(Native.read_pointer(output))
      end
    end

    # Mutable substring-capable compact directed acyclic word graph.
    # Exact lookup comes from Dictionary; +include_substring?+ and
    # +substring_frequency+ search within indexed terms.
    class Scdawg < Dictionary
      # Create an empty byte or Unicode-scalar SCDAWG.
      def initialize(domain: UNICODE_SCALAR)
        output = Native.pointer_output; Libdictenstein.check(Native.ldict_scdawg_new(domain, output)); super(Native.read_pointer(output))
      end
      # Insert an exact term and optional unsigned value.
      def put(term, value = nil)
        put_text(term, value)
      end
      # Return whether +term+ occurs as a substring of any indexed term.
      def include_substring?(term)
        output = Native.byte_output; @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_scdawg_contains_substring(pointer, term.b, term.bytesize, output)) }; output[0].positive?
      end
      # Return the number of substring occurrences across indexed terms.
      def substring_frequency(term)
        output = Native.size_output; @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_scdawg_substring_frequency(pointer, term.b, term.bytesize, output)) }; Native.read_size(output)
      end
    end

    # Durable adaptive radix trie with explicit checkpoint and reopen.
    # The backing path is expanded before native access. Close the handle
    # after checkpointing so native resources are released promptly.
    class PersistentArtrie < Dictionary
      # Create a new persistent trie at +path+ in the selected unit domain.
      def self.create(path, domain: UNICODE_SCALAR)
        open_native(path, domain, true)
      end
      # Open an existing persistent trie at +path+.
      def self.open(path, domain: UNICODE_SCALAR)
        open_native(path, domain, false)
      end
      # Select the native create/open entry point for a persistent trie.
      # Application code should call +create+ or +open+ instead.
      def self.open_native(path, domain, create)
        text = File.expand_path(path).b; output = Native.pointer_output
        function = create ? :ldict_persistent_artrie_create : :ldict_persistent_artrie_open
        Libdictenstein.check(Native.public_send(function, domain, text, text.bytesize, output)); new(Native.read_pointer(output))
      end
      # Insert or update a String key and optional mapped value.
      def put(term, value = nil)
        put_text(term, value)
      end
      # Remove a String key and report whether it existed.
      def remove(term)
        remove_text(term)
      end
      # Insert or update an unsigned 64-bit token sequence.
      def put_u64(tokens, value = nil)
        put_tokens(tokens, value)
      end
      # Remove an unsigned 64-bit token sequence if present.
      def remove_u64(tokens)
        remove_tokens(tokens)
      end
      # Flush a durable checkpoint; this is not the same as +close+.
      def checkpoint
        @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_dictionary_checkpoint(pointer)) }
      end
    end

    # Durable term-to-index vocabulary with reverse lookup by index.
    #
    #   vocab = VinaryTree::Libdictenstein::PersistentVocabulary.create("vocab.db")
    #   begin
    #     vocab.put("word", 7)
    #     vocab.checkpoint
    #     vocab.term(7) # => "word"
    #   ensure
    #     vocab.close
    #   end
    class PersistentVocabulary < Dictionary
      # Create a new vocabulary at +path+.
      def self.create(path)
        open_native(path, true)
      end
      # Reopen an existing vocabulary at +path+.
      def self.open(path)
        open_native(path, false)
      end
      # Select the native create/open entry point for a vocabulary.
      # Application code should call +create+ or +open+ instead.
      def self.open_native(path, create)
        text = File.expand_path(path).b; output = Native.pointer_output
        function = create ? :ldict_persistent_vocab_create : :ldict_persistent_vocab_open
        Libdictenstein.check(Native.public_send(function, text, text.bytesize, output)); new(Native.read_pointer(output))
      end
      # Associate a text +term+ with its unsigned 64-bit +index+.
      def put(term, index)
        put_text(term, index)
      end
      # Return the UTF-8 term at +index+, or +nil+ when absent.
      def term(index)
        length, found = Native.size_output, Native.byte_output
        @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_vocab_get_term(pointer, index, 0, 0, length, found)) }
        return nil if found[0].zero?
        output = Fiddle::Pointer.malloc(Native.read_size(length), Fiddle::RUBY_FREE)
        @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_vocab_get_term(pointer, index, output, output.size, length, found)) }
        output[0, Native.read_size(length)].force_encoding(Encoding::UTF_8)
      end
      # Flush a durable vocabulary checkpoint; close separately.
      def checkpoint
        @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_dictionary_checkpoint(pointer)) }
      end
    end
  end
end
