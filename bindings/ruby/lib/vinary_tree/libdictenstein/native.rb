require "fiddle/import"
require "rbconfig"

module VinaryTree
  module Libdictenstein
    module Native
      extend Fiddle::Importer

      def self.candidates
        explicit = ENV["LIBDICTENSTEIN_LIBRARY"]
        os, filename = case RbConfig::CONFIG["host_os"]
                       when /darwin/ then ["darwin", "liblibdictenstein.dylib"]
                       when /mswin|mingw/ then ["windows", "libdictenstein.dll"]
                       else ["linux", "liblibdictenstein.so"]
                       end
        cpu = case RbConfig::CONFIG["host_cpu"]
              when /x86_64|amd64/ then "x64"
              when /aarch64|arm64/ then "arm64"
              else RbConfig::CONFIG["host_cpu"]
              end
        platform = "#{os}-#{cpu}"
        [explicit, File.expand_path("native/#{platform}/#{filename}", __dir__), filename].compact
      end
      loaded = candidates.find do |candidate|
        begin dlload candidate; true
        rescue Fiddle::DLError then false
        end
      end
      raise Fiddle::DLError, "unable to load libdictenstein (tried #{candidates.join(', ')})" unless loaded

      Resource = struct ["void* context", "void* vtable"]
      TextEntry = struct [
        "void* data", "size_t len", "uint64_t value", "uint8_t has_value",
        "uint8_t reserved[7]"
      ]

      extern "const char* ldict_last_error_message(void)"
      extern "uint32_t ldict_dynamic_dawg_new(uint32_t, void*)"
      extern "uint32_t ldict_double_array_trie_new(uint32_t, void*, size_t, void*)"
      extern "uint32_t ldict_scdawg_new(uint32_t, void*)"
      extern "uint32_t ldict_persistent_artrie_create(uint32_t, const void*, size_t, void*)"
      extern "uint32_t ldict_persistent_artrie_open(uint32_t, const void*, size_t, void*)"
      extern "uint32_t ldict_persistent_vocab_create(const void*, size_t, void*)"
      extern "uint32_t ldict_persistent_vocab_open(const void*, size_t, void*)"
      extern "void ldict_dictionary_free(void*)"
      extern "uint32_t ldict_dictionary_resource(void*, void*)"
      extern "uint32_t ldict_dictionary_kind(void*, void*)"
      extern "uint32_t ldict_dictionary_capabilities(void*, void*)"
      extern "uint32_t ldict_dictionary_len(void*, void*)"
      extern "uint32_t ldict_dictionary_insert_text_value(void*, const void*, size_t, uint64_t, uint8_t, void*)"
      extern "uint32_t ldict_dictionary_remove_text(void*, const void*, size_t, void*)"
      extern "uint32_t ldict_dictionary_contains_text(void*, const void*, size_t, void*)"
      extern "uint32_t ldict_dictionary_get_text_value(void*, const void*, size_t, void*, void*, void*)"
      extern "uint32_t ldict_dictionary_insert_u64_value(void*, const void*, size_t, uint64_t, uint8_t, void*)"
      extern "uint32_t ldict_dictionary_remove_u64(void*, const void*, size_t, void*)"
      extern "uint32_t ldict_dictionary_contains_u64(void*, const void*, size_t, void*)"
      extern "uint32_t ldict_dictionary_get_u64_value(void*, const void*, size_t, void*, void*, void*)"
      extern "uint32_t ldict_dictionary_insert_text_batch(void*, void*, size_t, void*)"
      extern "uint32_t ldict_dictionary_clear(void*)"
      extern "uint32_t ldict_dictionary_compact(void*, void*)"
      extern "uint32_t ldict_dictionary_checkpoint(void*)"
      extern "uint32_t ldict_scdawg_contains_substring(void*, const void*, size_t, void*)"
      extern "uint32_t ldict_scdawg_substring_frequency(void*, const void*, size_t, void*)"
      extern "uint32_t ldict_vocab_get_term(void*, uint64_t, void*, size_t, void*, void*)"

      module_function
      def pointer_output = Fiddle::Pointer.malloc(Fiddle::SIZEOF_VOIDP, Fiddle::RUBY_FREE)
      def read_pointer(output) = output[0, Fiddle::SIZEOF_VOIDP].unpack1("J")
      def size_output = Fiddle::Pointer.malloc(Fiddle::SIZEOF_SIZE_T, Fiddle::RUBY_FREE)
      def read_size(output) = output[0, Fiddle::SIZEOF_SIZE_T].unpack1("J")
      def u64_output = Fiddle::Pointer.malloc(8, Fiddle::RUBY_FREE)
      def read_u64(output) = output[0, 8].unpack1("Q")
      def byte_output = Fiddle::Pointer.malloc(1, Fiddle::RUBY_FREE)
    end
  end
end
