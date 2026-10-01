# frozen_string_literal: true

# The probe exports every revision-7 symbol but deliberately no revision-8
# symbol. Loading the facade must not eagerly bind the optional constructor.
require "minitest/autorun"
require "vinary_tree/libdictenstein"

class Revision7GateTest < Minitest::Test
  LD = VinaryTree::Libdictenstein

  def test_optional_backends_are_rejected_before_missing_symbol_lookup
    assert_equal 1, LD.abi_version
    assert_equal 7, LD.api_revision
    assert_equal 6, assert_raises(LD::Error) { LD::PathMap.new }.status
    assert_equal 6, assert_raises(LD::Error) { LD::SuffixIndex.new }.status
  end
end
