# frozen_string_literal: true

# Uniform facade conformance suite for the Ruby binding.
#
# Instantiates the family C1-C10 contract for Ruby against a live
# libdictenstein shared library. Unlike test_cross_project.rb this suite needs
# only libdictenstein, never a liblevenshtein transducer, so it pins the
# *producer* ABI in isolation.
#
#   C1  identity/version           test_c1_*
#   C2  lifecycle/ownership        test_c2_*   (idempotent + free-order free)
#   C3  error-mapping matrix       test_c3_*   (reachable arms + thread-local msg)
#   C4  canonical fixture replay   test_c4_*   (cross-language oracle)
#   C5  CRUD/value/batch/substring test_c5_*   (+ capability-derived rejects)
#   C6  text domains / values      test_c6_*   (é/🦀/combining/NUL/invalid/u64)
#   C7  batch edges                test_c7_*   (0/1/255/256/257/large)
#   C8  property vs oracle         test_c8_*   (CRUD script + substring naive)
#   C9  leak discipline            test_c9_*   (>=10k cycles, RSS bounded)
#   C10 concurrency                test_c10_*  (parallel snapshot/mutate)
#
# Run (with the shared library discoverable), e.g.:
#   LIBDICTENSTEIN_LIBRARY=../../target/release/liblibdictenstein.so \
#     ruby -Ilib -Itest test/test_conformance.rb

require "minitest/autorun"
require "json"
require "tmpdir"
require "set"
require "vinary_tree/libdictenstein"

class ConformanceTest < Minitest::Test
  LD = VinaryTree::Libdictenstein

  FIXTURE = JSON.parse(
    File.read(File.expand_path("../../canonical_fixture.json", __dir__))
  ).freeze

  # Capability bits (LDICT_CAP_*).
  READ, INSERT, REMOVE, CLEAR, COMPACT, SUBSTRING, CHECKPOINT =
    (0..6).map { |bit| 1 << bit }

  def entries = FIXTURE["entries"].map { |item| [item["term"], item["value"]] }

  # nil-safe value assertion (assert_equal nil is removed in Minitest 6).
  def assert_value(expected, actual, message = nil)
    if expected.nil?
      assert_nil actual, message
    else
      assert_equal expected, actual, message
    end
  end

  # --------------------------------------------------------------------------
  # C1 identity/version
  # --------------------------------------------------------------------------

  def test_c1_identity_constants
    assert_equal 1, LD.abi_version
    assert_equal 4, LD.api_revision
  end

  def test_c1_kind_and_capabilities
    dawg = LD::DynamicDawg.new
    assert_equal 1, dawg.kind
    caps = dawg.capabilities
    [INSERT, REMOVE, CLEAR, COMPACT].each { |bit| assert caps.anybits?(bit) }
    refute caps.anybits?(SUBSTRING)
    refute caps.anybits?(CHECKPOINT)
    dat = LD::DoubleArrayTrie.new([["x", nil]])
    assert_equal 2, dat.kind
    assert dat.capabilities.anybits?(READ)
    scdawg = LD::Scdawg.new
    assert_equal 3, scdawg.kind
    assert scdawg.capabilities.anybits?(SUBSTRING)
  ensure
    dawg&.close
    dat&.close
    scdawg&.close
  end

  # --------------------------------------------------------------------------
  # C2 lifecycle/ownership
  # --------------------------------------------------------------------------

  def test_c2_double_close_is_idempotent
    dawg = LD::DynamicDawg.new
    dawg.put("a")
    dawg.close
    dawg.close # no double free, no crash
  end

  def test_c2_free_order_independence
    dawgs = Array.new(4) { LD::DynamicDawg.new }
    dawgs.each_with_index { |dawg, index| dawg.put("term#{index}", index) }
    # Free in an order unrelated to construction order.
    [2, 0, 3, 1].each { |index| dawgs[index].close }
  end

  # --------------------------------------------------------------------------
  # C3 error-mapping matrix + thread-local message
  #
  # Reachable through the idiomatic typed API: INVALID_UTF8 (3),
  # DOMAIN_MISMATCH (9), IO_ERROR (7). N/A:
  #   - NULL_POINTER (4): the facade raises IOError for a closed handle before
  #     crossing the ABI (see ConcurrentHandle#with_pointer).
  #   - UNSUPPORTED (6): no typed method exposes an unadvertised operation;
  #     capability bits are asserted absent instead (C5).
  #   - LIMIT_EXCEEDED (10): PersistentVocabulary#term auto-sizes its buffer.
  # --------------------------------------------------------------------------

  def test_c3_invalid_utf8
    dawg = LD::DynamicDawg.new(domain: LD::UNICODE_SCALAR)
    error = assert_raises(LD::Error) { dawg.put("\xFF".b) }
    assert_equal 3, error.status
    refute_empty error.message
  ensure
    dawg&.close
  end

  def test_c3_domain_mismatch
    dawg = LD::DynamicDawg.new(domain: LD::UNICODE_SCALAR)
    error = assert_raises(LD::Error) { dawg.put_u64([1, 2]) }
    assert_equal 9, error.status
  ensure
    dawg&.close
  end

  def test_c3_io_error_on_missing_persistent
    Dir.mktmpdir do |directory|
      error = assert_raises(LD::Error) do
        LD::PersistentArtrie.open(File.join(directory, "does-not-exist.part"))
      end
      assert_equal 7, error.status
      refute_empty error.message
    end
  end

  # --------------------------------------------------------------------------
  # C4 canonical fixture replay (cross-language oracle)
  # --------------------------------------------------------------------------

  def assert_fixture_reads(dictionary)
    assert_equal FIXTURE["size"], dictionary.size
    FIXTURE["contains"].each do |item|
      assert_equal item["expected"], dictionary.include?(item["term"]), item["term"]
    end
    FIXTURE["get"].each do |item|
      lookup = dictionary.get(item["term"])
      assert_equal item["found"], lookup.found?, item["term"]
      assert_value item["value"], lookup.value, item["term"]
    end
  end

  def test_c4_dynamic_dawg_matches_oracle
    dawg = LD::DynamicDawg.new
    assert_equal FIXTURE["size"], dawg.put_all(entries)
    assert_fixture_reads(dawg)
  ensure
    dawg&.close
  end

  def test_c4_double_array_trie_matches_oracle
    dat = LD::DoubleArrayTrie.new(entries)
    assert_fixture_reads(dat)
  ensure
    dat&.close
  end

  def test_c4_persistent_artrie_matches_oracle
    Dir.mktmpdir do |directory|
      art = LD::PersistentArtrie.create(File.join(directory, "terms.part"))
      entries.each { |term, value| art.put(term, value) }
      assert_fixture_reads(art)
      art.close
    end
  end

  def test_c4_scdawg_matches_substring_oracle
    scdawg = LD::Scdawg.new
    entries.each { |term, value| scdawg.put(term, value) }
    FIXTURE["substring_frequency"].each do |item|
      assert_equal item["expected"], scdawg.substring_frequency(item["pattern"]), item["pattern"]
    end
    FIXTURE["substring_contains"].each do |item|
      assert_equal item["expected"], scdawg.include_substring?(item["pattern"]), item["pattern"]
    end
  ensure
    scdawg&.close
  end

  # --------------------------------------------------------------------------
  # C5 CRUD + value + batch + substring; capability-derived rejects
  # --------------------------------------------------------------------------

  def test_c5_crud_round_trip
    dawg = LD::DynamicDawg.new
    assert dawg.put("cat", 1)
    refute dawg.put("cat", 1) # idempotent
    assert_equal 1, dawg.get("cat").value
    assert dawg.remove("cat")
    refute dawg.remove("cat")
    refute dawg.include?("cat")
  ensure
    dawg&.close
  end

  def test_c5_compact_preserves_terms
    dawg = LD::DynamicDawg.new
    dawg.put_all((0...50).map { |i| ["t#{i}", i] })
    (0...50).step(2) { |i| assert dawg.remove("t#{i}") }
    dawg.compact
    assert_equal 25, dawg.size
    assert_equal 1, dawg.get("t1").value
    refute dawg.include?("t0")
  ensure
    dawg&.close
  end

  def test_c5_substring_updates_with_inserts
    scdawg = LD::Scdawg.new
    scdawg.put("cat", 1)
    scdawg.put("cot", 2)
    assert_equal 2, scdawg.substring_frequency("t")
    assert scdawg.put("cut", nil)
    assert_equal 3, scdawg.substring_frequency("t")
  ensure
    scdawg&.close
  end

  def test_c5_capability_derived_rejects
    dat = LD::DoubleArrayTrie.new([["x", nil]])
    caps = dat.capabilities
    [INSERT, REMOVE, CLEAR, COMPACT].each { |bit| refute caps.anybits?(bit) }
    dawg = LD::DynamicDawg.new(domain: LD::UNICODE_SCALAR)
    assert_equal 9, assert_raises(LD::Error) { dawg.put_u64([1]) }.status
  ensure
    dat&.close
    dawg&.close
  end

  # --------------------------------------------------------------------------
  # C6 text domains and values
  # --------------------------------------------------------------------------

  def test_c6_precomposed_and_multibyte
    dawg = LD::DynamicDawg.new
    assert dawg.put("café", 7) # precomposed U+00E9
    assert dawg.put("🦀", 255)  # 4-byte scalar
    assert dawg.include?("café")
    assert_equal 255, dawg.get("🦀").value
  ensure
    dawg&.close
  end

  def test_c6_combining_sequence_is_distinct_from_precomposed
    precomposed = "café"  # café with a precomposed U+00E9
    combining = "café"   # cafe + U+0301 combining acute (distinct scalars)
    dawg = LD::DynamicDawg.new
    assert dawg.put(precomposed, 1)
    assert dawg.put(combining, 2)
    assert_equal 2, dawg.size
    assert_equal 1, dawg.get(precomposed).value
    assert_equal 2, dawg.get(combining).value
  ensure
    dawg&.close
  end

  def test_c6_byte_domain_accepts_nul_and_invalid_utf8
    dawg = LD::DynamicDawg.new(domain: LD::BYTE)
    embedded_nul = "a\x00b".b
    invalid_utf8 = "\xFF\xFE".b
    assert dawg.put(embedded_nul, 1)
    assert dawg.put(invalid_utf8, 2)
    assert dawg.include?(embedded_nul)
    assert_equal 2, dawg.get(invalid_utf8).value
  ensure
    dawg&.close
  end

  def test_c6_u64_domain_values_zero_and_max
    dawg = LD::DynamicDawg.new(domain: LD::U64)
    max = (1 << 64) - 1
    assert dawg.put_u64([1, 2, 3], 0)
    assert dawg.put_u64([9], max)
    assert_equal 0, dawg.get_u64([1, 2, 3]).value
    assert_equal max, dawg.get_u64([9]).value
  ensure
    dawg&.close
  end

  # --------------------------------------------------------------------------
  # C7 batch/paging edges
  # --------------------------------------------------------------------------

  def test_c7_batch_sizes
    [0, 1, 255, 256, 257, 1000].each do |size|
      dawg = LD::DynamicDawg.new
      inserted = dawg.put_all((0...size).map { |i| ["t#{i}", i] })
      assert_equal size, inserted
      assert_equal size, dawg.size
      if size.positive?
        assert_equal 0, dawg.get("t0").value
        assert_equal size - 1, dawg.get("t#{size - 1}").value
      end
      dawg.close
    end
  end

  # --------------------------------------------------------------------------
  # C8 property-based testing vs an in-language oracle
  # --------------------------------------------------------------------------

  def test_c8_crud_script_matches_hash_oracle
    rng = Random.new(0xC0FFEE)
    keys = Array.new(40) { |i| "k#{i}" }
    oracle = {}
    dawg = LD::DynamicDawg.new
    3000.times do
      key = keys.sample(random: rng)
      present = oracle.key?(key)
      case rng.rand
      when 0...0.5
        value = rng.rand(2).zero? ? nil : rng.rand(1 << 63)
        assert_equal !present, dawg.put(key, value)
        oracle[key] = value
      when 0.5...0.75
        assert_equal present, dawg.remove(key)
        oracle.delete(key)
      when 0.75...0.95
        assert_equal present, dawg.include?(key)
        assert_value oracle[key], dawg.get(key).value if present
      else
        dawg.compact
      end
      assert_equal oracle.size, dawg.size
    end
  ensure
    dawg&.close
  end

  def test_c8_substring_matches_naive_oracle
    rng = Random.new(0x5CDA)
    alphabet = "abcx".chars
    generate = ->(max) { Array.new(rng.rand(max) + 1) { alphabet.sample(random: rng) }.join }
    terms = Set.new
    terms << generate.call(6) while terms.size < 60
    terms = terms.to_a
    naive = lambda do |pattern|
      terms.sum do |term|
        (0..term.length - pattern.length).count { |start| term[start, pattern.length] == pattern }
      end
    end
    scdawg = LD::Scdawg.new
    terms.each { |term| scdawg.put(term, nil) }
    200.times do
      pattern = generate.call(3)
      expected = naive.call(pattern)
      assert_equal expected, scdawg.substring_frequency(pattern), pattern
      assert_equal expected.positive?, scdawg.include_substring?(pattern), pattern
    end
  ensure
    scdawg&.close
  end

  # --------------------------------------------------------------------------
  # C9 leak discipline
  # --------------------------------------------------------------------------

  def rss_kib
    return 0 unless File.exist?("/proc/self/status")

    File.foreach("/proc/self/status") do |line|
      return Integer(line.split[1]) if line.start_with?("VmRSS:")
    end
    0
  end

  def test_c9_create_use_free_cycles_do_not_leak
    cycles = 12_000
    2000.times do # allocator steady state
      dawg = LD::DynamicDawg.new
      dawg.put("cat", 1)
      dawg.close
    end
    GC.start
    before = rss_kib
    cycles.times do
      dawg = LD::DynamicDawg.new
      dawg.put_all([["cat", 1], ["cot", 2], ["cut", nil]])
      assert dawg.include?("cot")
      dawg.close
    end
    GC.start
    after = rss_kib
    if before.positive? && after > before
      assert_operator after - before, :<, 48 * 1024,
                       "RSS grew #{after - before} KiB over #{cycles} cycles"
    end
  end

  # --------------------------------------------------------------------------
  # C10 concurrency
  # --------------------------------------------------------------------------

  def test_c10_independent_dictionaries_per_thread
    errors = []
    threads = (0...8).map do |seed|
      Thread.new do
        dawg = LD::DynamicDawg.new
        2000.times { |i| dawg.put("t#{seed}_#{i}", i) }
        raise "len" unless dawg.size == 2000
        raise "get" unless dawg.get("t#{seed}_1500").value == 1500

        dawg.close
      rescue StandardError => error
        errors << error
      end
    end
    threads.each(&:join)
    assert_empty errors
  end

  def test_c10_concurrent_readers_during_writer
    errors = []
    dawg = LD::DynamicDawg.new
    dawg.put_all((0...500).map { |i| ["seed#{i}", i] })
    stop = false
    readers = Array.new(4) do
      Thread.new do
        until stop
          raise "lost seed0" unless dawg.include?("seed0")

          dawg.get("seed250")
        end
      rescue StandardError => error
        errors << error
      end
    end
    (500...3000).each { |i| dawg.put("w#{i}", i) }
    stop = true
    readers.each(&:join)
    assert_empty errors
    assert_equal 2999, dawg.get("w2999").value
  ensure
    dawg&.close
  end
end
