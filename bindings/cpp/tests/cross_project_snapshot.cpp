#include "libdictenstein.hpp"
#include "liblevenshtein.hpp"

#include <cassert>
#include <optional>
#include <set>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

int main() {
    namespace ld = vinary_tree::libdictenstein;
    namespace ll = vinary_tree::liblevenshtein;

    ld::dynamic_dawg dictionary;
    const std::pair<std::string_view, std::optional<std::uint64_t>> initial[] = {
        {"cat", 1}, {"cot", 2}, {"cut", 3}, {"scat", std::nullopt},
    };
    assert(dictionary.insert_all(initial) == 4);
    const auto resource = dictionary.resource();
    ll::transducer transducer(resource);
    auto cursor = transducer.query("cat", 2);

    std::set<std::string> old_results;
    {
        auto first = cursor.next_batch(1);
        assert(first.matches().size() == 1);
        old_results.emplace(ll::batch::utf8(first.matches().front()));
    }

    assert(dictionary.remove("cot"));
    assert(!dictionary.insert("cut", 30));
    assert(dictionary.insert("cit", 5));
    static_cast<void>(dictionary.compact());

    for (;;) {
        auto next = cursor.next_batch(1);
        if (next.matches().empty()) break;
        old_results.emplace(ll::batch::utf8(next.matches().front()));
    }
    assert(old_results == (std::set<std::string>{"cat", "cot", "cut", "scat"}));

    std::set<std::string> fresh_results;
    auto fresh = transducer.query("cat", 2);
    for (;;) {
        auto next = fresh.next_batch(2);
        if (next.matches().empty()) break;
        for (const auto& item : next.matches())
            fresh_results.emplace(ll::batch::utf8(item));
    }
    assert(fresh_results == (std::set<std::string>{"cat", "cut", "cit", "scat"}));

    const std::pair<std::string_view, std::optional<std::uint64_t>> static_entries[] = {
        {"alpha", 7}, {"beta", std::nullopt},
    };
    ld::double_array_trie dat(static_entries);
    assert(dat.contains("alpha"));
    assert(dat.get("alpha").value == 7);

    ld::scdawg suffix;
    assert(suffix.insert("banana", 1));
    assert(suffix.contains_substring("ana"));
    assert(suffix.substring_frequency("ana") == 2);
}
