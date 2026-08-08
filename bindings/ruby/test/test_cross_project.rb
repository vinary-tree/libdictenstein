require "minitest/autorun"
require "tmpdir"
require "set"
require "vinary_tree/libdictenstein"
require "vinary_tree/liblevenshtein"

class CrossProjectTest < Minitest::Test
  LD = VinaryTree::Libdictenstein
  LL = VinaryTree::Liblevenshtein

  def collect(query) = query.map(&:term).to_set

  def test_long_lived_query_start_snapshot_and_property_traces
    dictionary = LD::DynamicDawg.new
    dictionary.put_all([["cat", 1], ["cot", 2], ["cut", 3], ["scat", nil]])
    transducer = LL::Transducer.new(dictionary)
    enumerator = transducer.query("cat", 2).each
    old = Set[enumerator.next.term]
    dictionary.remove("cot"); dictionary.put("cut", 30); dictionary.put("cit", 5); dictionary.compact
    begin
      old << enumerator.next.term while true
    rescue StopIteration
      nil
    end
    assert_equal Set["cat", "cot", "cut", "scat"], old
    64.times do |seed|
      subject = LD::DynamicDawg.new
      32.times { |index| subject.put(format("t%02d", index), index) }
      machine = LL::Transducer.new(subject)
      expected = collect(machine.query("t00", 3))
      stream = machine.query("t00", 3).each
      actual = Set[stream.next.term]
      subject.remove(format("t%02d", seed % 32)); subject.put(format("x%02d", seed), seed)
      begin
        actual << stream.next.term while true
      rescue StopIteration
        nil
      end
      assert_equal expected, actual, "seed #{seed}"
      machine.close; subject.close
    end
  ensure
    transducer&.close; dictionary&.close
  end

  def test_other_backends
    dat = LD::DoubleArrayTrie.new([["alpha", 7], ["beta", nil]])
    assert_equal 7, dat.get("alpha").value
    suffix = LD::Scdawg.new; suffix.put("banana", 1)
    assert suffix.include_substring?("ana"); assert_equal 2, suffix.substring_frequency("ana")
    Dir.mktmpdir do |directory|
      path = File.join(directory, "dictionary")
      persistent = LD::PersistentArtrie.create(path); persistent.put("durable", 9); persistent.checkpoint; persistent.close
      reopened = LD::PersistentArtrie.open(path); assert_equal 9, reopened.get("durable").value; reopened.close
    end
  ensure
    dat&.close; suffix&.close
  end
end
