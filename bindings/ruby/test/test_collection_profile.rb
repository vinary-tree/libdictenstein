require "json"
require "minitest/autorun"
require "open3"
require "rbconfig"

class CollectionProfileTest < Minitest::Test
  SCRIPT = File.expand_path("../bin/libdictenstein-collection-profile", __dir__)

  def test_small_machine_readable_arms
    expected = {
      "materialized" => [16, 632],
      "stream" => [16, 632],
      "stream-cancel" => [5, 184]
    }
    expected.each do |arm, (count, checksum)|
      stdout, stderr, status = Open3.capture3(
        RbConfig.ruby,
        SCRIPT,
        "--arm", arm,
        "--entries", "16",
        "--passes", "1",
        "--warmup-passes", "1",
        "--batch-size", "4",
        "--early-cancel", "5"
      )
      assert status.success?, "#{arm}: #{stderr}"
      result = JSON.parse(stdout)
      assert_equal "libdictenstein.host-collection-traversal.v1", result.fetch("schema")
      assert_equal "ruby", result.fetch("runtime")
      assert_equal arm, result.fetch("arm")
      assert_equal count, result.fetch("consumed_entries_per_pass")
      assert_equal checksum, result.fetch("checksum")
      assert_operator result.fetch("elapsed_ns"), :>=, 0
    end
  end
end
