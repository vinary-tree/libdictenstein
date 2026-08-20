#include "libdictenstein.hpp"

#include <algorithm>
#include <array>
#include <charconv>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <exception>
#include <iostream>
#include <limits>
#include <optional>
#include <span>
#include <stdexcept>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

namespace ld = vinary_tree::libdictenstein;

namespace {

constexpr std::size_t default_entries = 65'536;
constexpr std::size_t default_batch_size = 256;
constexpr std::size_t default_early_cancel = 64;
constexpr std::size_t key_units = 38;

enum class profile_arm { materialized, stream, stream_cancel };

struct profile_config final {
    profile_arm arm{};
    std::string_view arm_name;
    std::size_t entries = default_entries;
    std::size_t passes = 1;
    std::size_t warmup_passes = 1;
    std::size_t batch_size = default_batch_size;
    std::size_t early_cancel = default_early_cancel;
};

struct corpus_entry final {
    std::string key;
    std::uint64_t value = 0;
};

struct owned_entry final {
    std::vector<std::uint8_t> key;
    std::optional<std::uint64_t> value;
};

struct drain_result final {
    std::uint64_t checksum = 0;
    std::size_t count = 0;
};

[[nodiscard]] std::size_t parse_size(std::string_view value,
                                     std::string_view option,
                                     bool allow_zero = false) {
    std::size_t parsed = 0;
    const auto result =
        std::from_chars(value.data(), value.data() + value.size(), parsed);
    if (result.ec != std::errc{} || result.ptr != value.data() + value.size() ||
        (!allow_zero && parsed == 0))
        throw std::invalid_argument(std::string(option) +
                                    (allow_zero ? " must be nonnegative"
                                                : " must be positive"));
    return parsed;
}

[[nodiscard]] profile_config parse_arguments(int argc, char** argv) {
    profile_config config;
    bool has_arm = false;
    for (int index = 1; index < argc; index += 2) {
        if (index + 1 >= argc)
            throw std::invalid_argument("every option requires a value");
        const std::string_view option(argv[index]);
        const std::string_view value(argv[index + 1]);
        if (option == "--arm") {
            has_arm = true;
            config.arm_name = value;
            if (value == "materialized")
                config.arm = profile_arm::materialized;
            else if (value == "stream")
                config.arm = profile_arm::stream;
            else if (value == "stream-cancel")
                config.arm = profile_arm::stream_cancel;
            else
                throw std::invalid_argument(
                    "--arm must be materialized, stream, or stream-cancel");
        } else if (option == "--entries") {
            config.entries = parse_size(value, option);
        } else if (option == "--passes") {
            config.passes = parse_size(value, option);
        } else if (option == "--warmup-passes") {
            config.warmup_passes = parse_size(value, option, true);
        } else if (option == "--batch-size") {
            config.batch_size = parse_size(value, option);
        } else if (option == "--early-cancel") {
            config.early_cancel = parse_size(value, option);
        } else {
            throw std::invalid_argument("unknown argument: " +
                                        std::string(option));
        }
    }
    if (!has_arm) throw std::invalid_argument("--arm is required");
    if (config.batch_size >
        std::numeric_limits<std::size_t>::max() / key_units)
        throw std::invalid_argument("--batch-size is too large");
    return config;
}

[[nodiscard]] std::vector<corpus_entry> make_corpus(std::size_t size) {
    std::vector<corpus_entry> corpus;
    corpus.reserve(size);
    for (std::size_t index = 0; index < size; ++index) {
        std::array<char, key_units + 1> key{};
        const int written = std::snprintf(
            key.data(), key.size(), "collection/%04zx/%08zx/shared-suffix",
            index & 0x0fff, index);
        if (written != static_cast<int>(key_units))
            throw std::runtime_error("generated key length changed");
        corpus.push_back({std::string(key.data(), key_units),
                          static_cast<std::uint64_t>(index)});
    }
    return corpus;
}

[[nodiscard]] std::uint64_t expected_checksum(
    const std::vector<corpus_entry>& corpus, std::size_t limit) {
    std::vector<const corpus_entry*> ordered;
    ordered.reserve(corpus.size());
    for (const auto& entry : corpus) ordered.push_back(&entry);
    std::ranges::sort(ordered, {}, [](const corpus_entry* entry) {
        return std::string_view(entry->key);
    });
    limit = std::min(limit, ordered.size());
    std::uint64_t checksum = 0;
    for (const corpus_entry* entry :
         std::span<const corpus_entry* const>(ordered).first(limit))
        checksum += static_cast<std::uint64_t>(entry->key.size()) ^ entry->value;
    return checksum;
}

[[nodiscard]] ld::dynamic_dawg build_dictionary(
    const std::vector<corpus_entry>& corpus) {
    ld::dynamic_dawg dictionary(ld::unit_domain::byte);
    std::vector<std::pair<std::string_view, std::optional<std::uint64_t>>>
        entries;
    entries.reserve(corpus.size());
    for (const auto& entry : corpus)
        entries.emplace_back(entry.key, entry.value);
    if (dictionary.insert_all(entries) != entries.size())
        throw std::runtime_error("generated corpus did not insert completely");
    return dictionary;
}

[[nodiscard]] ld::entry_batch_limits limits_for(std::size_t batch_size) {
    return {batch_size, batch_size * key_units, batch_size};
}

[[nodiscard]] std::uint64_t entry_checksum(const ld::entry_view& entry) {
    if (entry.domain() != ld::unit_domain::byte)
        throw std::runtime_error("benchmark expected a byte-domain entry");
    return static_cast<std::uint64_t>(entry.bytes().size()) ^
           entry.value().value_or(0);
}

[[nodiscard]] drain_result drain_materialized(const ld::dictionary& dictionary,
                                              std::size_t batch_size) {
    auto view = dictionary.entries(limits_for(batch_size));
    std::vector<owned_entry> entries;
    if (const auto exact = view.exact_size()) entries.reserve(*exact);
    for (const ld::entry_view entry : view) {
        const auto key = entry.bytes();
        entries.push_back(
            {std::vector<std::uint8_t>(key.begin(), key.end()), entry.value()});
    }
    view.close();
    drain_result result{0, entries.size()};
    for (const auto& entry : entries)
        result.checksum += static_cast<std::uint64_t>(entry.key.size()) ^
                           entry.value.value_or(0);
    return result;
}

[[nodiscard]] drain_result drain_stream(const ld::dictionary& dictionary,
                                        std::size_t batch_size,
                                        std::size_t limit, bool cancel) {
    auto view = dictionary.entries(limits_for(batch_size));
    drain_result result;
    for (const ld::entry_view entry : view) {
        if (result.count == limit)
            throw std::runtime_error("stream cardinality exceeds corpus");
        result.checksum += entry_checksum(entry);
        ++result.count;
        if (cancel && result.count == limit) break;
    }
    if (cancel) view.cancel();
    view.close();
    if (result.count != limit)
        throw std::runtime_error("stream cardinality differs from corpus");
    return result;
}

[[nodiscard]] drain_result drain(const ld::dictionary& dictionary,
                                 const profile_config& config) {
    switch (config.arm) {
        case profile_arm::materialized:
            return drain_materialized(dictionary, config.batch_size);
        case profile_arm::stream:
            return drain_stream(dictionary, config.batch_size, config.entries,
                                false);
        case profile_arm::stream_cancel:
            return drain_stream(dictionary, config.batch_size,
                                std::min(config.entries, config.early_cancel),
                                true);
    }
    throw std::logic_error("unreachable arm");
}

void run(int argc, char** argv) {
    const profile_config config = parse_arguments(argc, argv);
    const auto corpus = make_corpus(config.entries);
    const auto dictionary = build_dictionary(corpus);
    const std::size_t consumed =
        config.arm == profile_arm::stream_cancel
            ? std::min(config.entries, config.early_cancel)
            : config.entries;
    const std::uint64_t expected = expected_checksum(corpus, consumed);

    for (std::size_t pass = 0; pass < config.warmup_passes; ++pass) {
        const auto result = drain(dictionary, config);
        if (result.count != consumed || result.checksum != expected)
            throw std::runtime_error("warmup checksum or cardinality mismatch");
    }

    const auto started = std::chrono::steady_clock::now();
    std::uint64_t checksum = 0;
    for (std::size_t pass = 0; pass < config.passes; ++pass) {
        const auto result = drain(dictionary, config);
        if (result.count != consumed || result.checksum != expected)
            throw std::runtime_error("timed checksum or cardinality mismatch");
        checksum += result.checksum;
    }
    const auto measured_ns =
        std::chrono::duration_cast<std::chrono::nanoseconds>(
            std::chrono::steady_clock::now() - started)
            .count();
    const auto elapsed_ns = measured_ns > 0 ? measured_ns : 1;
    if (checksum != expected * static_cast<std::uint64_t>(config.passes))
        throw std::runtime_error("aggregate checksum mismatch");

    std::cout
        << "{\"schema\":\"libdictenstein.host-collection-traversal.v1\","
        << "\"runtime\":\"cpp\",\"arm\":\"" << config.arm_name
        << "\",\"dictionary_entries\":" << config.entries
        << ",\"consumed_entries_per_pass\":" << consumed
        << ",\"passes\":" << config.passes
        << ",\"warmup_passes\":" << config.warmup_passes
        << ",\"batch_size\":" << config.batch_size
        << ",\"early_cancel\":";
    if (config.arm == profile_arm::stream_cancel)
        std::cout << config.early_cancel;
    else
        std::cout << "null";
    std::cout << ",\"elapsed_ns\":" << elapsed_ns
              << ",\"checksum\":" << checksum << "}\n";
}

}  // namespace

int main(int argc, char** argv) {
    try {
        run(argc, argv);
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 2;
    }
}
