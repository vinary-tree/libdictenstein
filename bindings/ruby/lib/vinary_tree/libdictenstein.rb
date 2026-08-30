require "thread"
require_relative "libdictenstein/version"
require_relative "libdictenstein/native"

module VinaryTree
  module Libdictenstein
    BYTE = 1
    UNICODE_SCALAR = 2
    U64 = 3

    module AlgebraOperation
      UNION = 1
      INTERSECTION = 2
      DIFFERENCE = 3
      SYMMETRIC_DIFFERENCE = 4
    end

    module ValueMerge
      FIRST = 1
      LAST = 2
      LATTICE_JOIN = 3
      LATTICE_MEET = 4
    end

    # Native ABI version (LDICT_ABI_VERSION); always 1 for this family.
    def self.abi_version = Native.ldict_abi_version

    # Compatible-additions revision within the ABI version (LDICT_API_REVISION).
    def self.api_revision = Native.ldict_api_revision

    Lookup = Data.define(:found?, :value)
    Entry = Data.define(:key, :value, :domain)
    EntryInfo = Data.define(:domain, :exact_length, :snapshot_identity)

    class Error < StandardError
      attr_reader :status
      def initialize(status)
        @status = status
        super("libdictenstein status #{status}: #{Native.ldict_last_error_message.to_s}")
      end
    end
    def self.check(status) = (raise Error, status unless status.zero?)

    class ConcurrentHandle
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

    # Shared low-level adapter over one opaque native entry cursor. Every key
    # is copied before a leased batch is released.
    class EntryCursorState
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

    # Public bounded stream. Enumerable#each uses ensure so break and raised
    # exceptions deterministically close the native cursor.
    class EntryStream
      include Enumerable

      attr_reader :info

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

      def self.finalizer(state) = proc { state.close rescue nil }

      def next = @state.next_entry

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

      def cancel = @state.cancel

      def close
        @state.close
        ObjectSpace.undefine_finalizer(self) if @state.closed?
        nil
      end
    end

    class Dictionary
      include Enumerable

      attr_reader :handle
      def initialize(pointer)
        @handle = ConcurrentHandle.new(pointer)
        ObjectSpace.define_finalizer(self, self.class.finalizer(@handle))
      end
      def self.finalizer(handle) = proc { handle.close rescue nil }

      def close
        @handle.close
        ObjectSpace.undefine_finalizer(self)
        nil
      end

      def with_resource
        @handle.with_pointer do |pointer|
          resource = Native::Resource.malloc
          Libdictenstein.check(Native.ldict_dictionary_resource(pointer, resource))
          yield resource.context.to_i, resource.vtable.to_i
        end
      end

      def kind = scalar(:ldict_dictionary_kind, Native.u64_output, ->(output) { Native.read_u64(output) & 0xffff_ffff })
      def capabilities = scalar(:ldict_dictionary_capabilities, Native.u64_output, Native.method(:read_u64))
      def length = scalar(:ldict_dictionary_len, Native.size_output, Native.method(:read_size))
      alias size length

      def entry_stream(max_entries: 256, max_units: 4096, max_values: 256)
        EntryStream.new(
          @handle,
          max_entries: max_entries,
          max_units: max_units,
          max_values: max_values
        )
      end

      def each
        stream = entry_stream
        return stream.each unless block_given?
        stream.each { |entry| yield entry }
        self
      end

      # Host-owned materialized snapshot idioms.
      def entries = each.to_a
      def keys = entries.map(&:key)
      def values = entries.map(&:value)

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

      def union(right, value_merge: ValueMerge::LAST)
        algebra(right, operation: AlgebraOperation::UNION, value_merge: value_merge)
      end

      def intersection(right, value_merge: ValueMerge::LATTICE_MEET)
        algebra(right, operation: AlgebraOperation::INTERSECTION, value_merge: value_merge)
      end

      def difference(right)
        algebra(right, operation: AlgebraOperation::DIFFERENCE, value_merge: ValueMerge::FIRST)
      end

      def symmetric_difference(right)
        algebra(
          right,
          operation: AlgebraOperation::SYMMETRIC_DIFFERENCE,
          value_merge: ValueMerge::FIRST
        )
      end

      def |(right) = union(right)
      def &(right) = intersection(right)
      def -(right) = difference(right)
      def ^(right) = symmetric_difference(right)

      def include?(term)
        output = Native.byte_output
        @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_dictionary_contains_text(pointer, term.b, term.bytesize, output)) }
        output[0].positive?
      end

      def get(term)
        found, value, present = Native.byte_output, Native.u64_output, Native.byte_output
        @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_dictionary_get_text_value(pointer, term.b, term.bytesize, found, value, present)) }
        Lookup.new(found[0].positive?, present[0].positive? ? Native.read_u64(value) : nil)
      end

      def include_u64?(tokens)
        packed = tokens.pack("Q*"); output = Native.byte_output
        @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_dictionary_contains_u64(pointer, packed, tokens.length, output)) }
        output[0].positive?
      end

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

    class DynamicDawg < Dictionary
      def self.adopt(pointer) = new(pointer: pointer)

      def initialize(domain: UNICODE_SCALAR, pointer: nil)
        unless pointer
          output = Native.pointer_output
          Libdictenstein.check(Native.ldict_dynamic_dawg_new(domain, output))
          pointer = Native.read_pointer(output)
        end
        super(pointer)
      end
      def put(term, value = nil) = put_text(term, value)
      def remove(term) = remove_text(term)
      def put_u64(tokens, value = nil) = put_tokens(tokens, value)
      def remove_u64(tokens) = remove_tokens(tokens)
      def clear = @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_dictionary_clear(pointer)) }
      def compact
        output = Native.size_output; @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_dictionary_compact(pointer, output)) }; Native.read_size(output)
      end
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

    class DoubleArrayTrie < Dictionary
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

    class Scdawg < Dictionary
      def initialize(domain: UNICODE_SCALAR)
        output = Native.pointer_output; Libdictenstein.check(Native.ldict_scdawg_new(domain, output)); super(Native.read_pointer(output))
      end
      def put(term, value = nil) = put_text(term, value)
      def include_substring?(term)
        output = Native.byte_output; @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_scdawg_contains_substring(pointer, term.b, term.bytesize, output)) }; output[0].positive?
      end
      def substring_frequency(term)
        output = Native.size_output; @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_scdawg_substring_frequency(pointer, term.b, term.bytesize, output)) }; Native.read_size(output)
      end
    end

    class PersistentArtrie < Dictionary
      def self.create(path, domain: UNICODE_SCALAR) = open_native(path, domain, true)
      def self.open(path, domain: UNICODE_SCALAR) = open_native(path, domain, false)
      def self.open_native(path, domain, create)
        text = File.expand_path(path).b; output = Native.pointer_output
        function = create ? :ldict_persistent_artrie_create : :ldict_persistent_artrie_open
        Libdictenstein.check(Native.public_send(function, domain, text, text.bytesize, output)); new(Native.read_pointer(output))
      end
      def put(term, value = nil) = put_text(term, value)
      def remove(term) = remove_text(term)
      def put_u64(tokens, value = nil) = put_tokens(tokens, value)
      def remove_u64(tokens) = remove_tokens(tokens)
      def checkpoint = @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_dictionary_checkpoint(pointer)) }
    end

    class PersistentVocabulary < Dictionary
      def self.create(path) = open_native(path, true)
      def self.open(path) = open_native(path, false)
      def self.open_native(path, create)
        text = File.expand_path(path).b; output = Native.pointer_output
        function = create ? :ldict_persistent_vocab_create : :ldict_persistent_vocab_open
        Libdictenstein.check(Native.public_send(function, text, text.bytesize, output)); new(Native.read_pointer(output))
      end
      def put(term, index) = put_text(term, index)
      def term(index)
        length, found = Native.size_output, Native.byte_output
        @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_vocab_get_term(pointer, index, 0, 0, length, found)) }
        return nil if found[0].zero?
        output = Fiddle::Pointer.malloc(Native.read_size(length), Fiddle::RUBY_FREE)
        @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_vocab_get_term(pointer, index, output, output.size, length, found)) }
        output[0, Native.read_size(length)].force_encoding(Encoding::UTF_8)
      end
      def checkpoint = @handle.with_pointer { |pointer| Libdictenstein.check(Native.ldict_dictionary_checkpoint(pointer)) }
    end
  end
end
