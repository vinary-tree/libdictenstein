require "thread"
require_relative "libdictenstein/version"
require_relative "libdictenstein/native"

module VinaryTree
  module Libdictenstein
    BYTE = 1
    UNICODE_SCALAR = 2
    U64 = 3

    Lookup = Data.define(:found?, :value)

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

    class Dictionary
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
      def initialize(domain: UNICODE_SCALAR)
        output = Native.pointer_output; Libdictenstein.check(Native.ldict_dynamic_dawg_new(domain, output)); super(Native.read_pointer(output))
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
