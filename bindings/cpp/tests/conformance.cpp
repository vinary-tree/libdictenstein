// Uniform facade conformance suite for the C++ (header-only) binding.
//
// Instantiates the family C1-C10 contract for C++ against a live libdictenstein
// shared library. Unlike cross_project_snapshot.cpp this suite needs only
// libdictenstein and the canonical fixture, never a liblevenshtein transducer,
// so it pins the *producer* ABI in isolation.
//
//   C1  identity/version           check_c1_*
//   C2  lifecycle/ownership        check_c2_*   (RAII move + null-free no-op)
//   C3  error-mapping matrix       check_c3_*   (reachable arms + thread-local msg)
//   C4  canonical fixture replay   check_c4_*   (cross-language oracle, parsed)
//   C5  CRUD/value/batch/substring check_c5_*   (+ capability-derived rejects)
//   C6  text domains / values      check_c6_*   (é/🦀/combining/NUL/invalid/u64)
//   C7  batch edges                check_c7_*   (0/1/255/256/257/large)
//   C8  property vs oracle         check_c8_*   (CRUD script + substring naive)
//   C9  leak discipline            check_c9_*   (>=10k cycles, RSS bounded)
//   C10 concurrency                check_c10_*  (single-writer / many-reader)
//
// Build (from the repository root), e.g.:
//   c++ -std=c++20 -O2 bindings/cpp/tests/conformance.cpp
//       -I include -I ../vinary-tree-interop/include
//       -L target/release -llibdictenstein -o /tmp/cpp_conformance
//   LD_LIBRARY_PATH=target/release /tmp/cpp_conformance bindings/canonical_fixture.json

#include "libdictenstein.hpp"

#include <atomic>
#include <cctype>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <limits>
#include <map>
#include <optional>
#include <random>
#include <set>
#include <span>
#include <sstream>
#include <string>
#include <string_view>
#include <thread>
#include <type_traits>
#include <unistd.h>
#include <utility>
#include <vector>

namespace ld = vinary_tree::libdictenstein;

// ---------------------------------------------------------------------------
// minimal test harness
// ---------------------------------------------------------------------------

namespace {

int g_failures = 0;

void report(bool ok, const char* expression, int line) {
    if (!ok) {
        std::cerr << "FAIL: " << expression << " (line " << line << ")\n";
        ++g_failures;
    }
}

#define CHECK(cond) report(static_cast<bool>(cond), #cond, __LINE__)

using EntrySpan = std::span<const std::pair<std::string_view, std::optional<std::uint64_t>>>;

// ---------------------------------------------------------------------------
// Fixture + schema-specific JSON parser
//
// canonical_fixture.json has a fixed, shallow shape: a top-level object whose
// values are strings, a number, and arrays of flat objects. Rather than a
// generic recursive JSON tree (which needs std::vector<incomplete-type> and
// trips over is_trivially_destructible on libstdc++), we parse exactly that
// schema with concrete return types.
// ---------------------------------------------------------------------------

struct Fixture {
    std::vector<std::pair<std::string, std::optional<std::uint64_t>>> entries;
    std::size_t size = 0;
    std::vector<std::pair<std::string, bool>> contains;
    struct Get { std::string term; bool found; std::optional<std::uint64_t> value; };
    std::vector<Get> get;
    std::vector<std::pair<std::string, std::size_t>> substring_frequency;
    std::vector<std::pair<std::string, bool>> substring_contains;
};

class Parser {
public:
    explicit Parser(std::string text) : text_(std::move(text)) {}

    Fixture parse() {
        Fixture fixture;
        ws();
        expect('{');
        if (try_consume('}')) return fixture;
        for (;;) {
            ws();
            const std::string key = parse_string();
            ws();
            expect(':');
            if (key == "entries") parse_entries(fixture);
            else if (key == "size") fixture.size = static_cast<std::size_t>(parse_number());
            else if (key == "contains") parse_term_bool_array(fixture.contains, "term");
            else if (key == "get") parse_get(fixture);
            else if (key == "substring_frequency") parse_pattern_number_array(fixture.substring_frequency);
            else if (key == "substring_contains") parse_term_bool_array(fixture.substring_contains, "pattern");
            else skip_value();
            if (try_consume(',')) continue;
            expect('}');
            break;
        }
        return fixture;
    }

private:
    std::string text_;
    std::size_t pos_ = 0;

    void ws() {
        while (pos_ < text_.size()) {
            const char c = text_[pos_];
            if (c == ' ' || c == '\n' || c == '\r' || c == '\t') ++pos_;
            else break;
        }
    }
    char cur() {
        ws();
        return pos_ < text_.size() ? text_[pos_] : '\0';
    }
    void expect(char c) {
        ws();
        if (pos_ >= text_.size() || text_[pos_] != c)
            throw std::runtime_error(std::string("expected '") + c + "' in fixture JSON");
        ++pos_;
    }
    bool try_consume(char c) {
        if (cur() == c) { ++pos_; return true; }
        return false;
    }

    std::string parse_string() {
        ws();
        expect('"');
        std::string out;
        while (pos_ < text_.size() && text_[pos_] != '"') {
            const char c = text_[pos_++];
            if (c != '\\') { out.push_back(c); continue; }
            const char e = text_[pos_++];
            switch (e) {
                case 'n': out.push_back('\n'); break;
                case 't': out.push_back('\t'); break;
                case 'r': out.push_back('\r'); break;
                case 'u': {
                    const unsigned code = std::stoul(text_.substr(pos_, 4), nullptr, 16);
                    pos_ += 4;
                    if (code < 0x80) {
                        out.push_back(static_cast<char>(code));
                    } else if (code < 0x800) {
                        out.push_back(static_cast<char>(0xC0 | (code >> 6)));
                        out.push_back(static_cast<char>(0x80 | (code & 0x3F)));
                    } else {
                        out.push_back(static_cast<char>(0xE0 | (code >> 12)));
                        out.push_back(static_cast<char>(0x80 | ((code >> 6) & 0x3F)));
                        out.push_back(static_cast<char>(0x80 | (code & 0x3F)));
                    }
                    break;
                }
                default: out.push_back(e); break; // \" \\ \/
            }
        }
        ++pos_; // closing quote
        return out;
    }
    double parse_number() {
        ws();
        const std::size_t start = pos_;
        while (pos_ < text_.size()) {
            const char c = text_[pos_];
            if (std::isdigit(static_cast<unsigned char>(c)) || c == '-' || c == '+' ||
                c == '.' || c == 'e' || c == 'E')
                ++pos_;
            else
                break;
        }
        return std::stod(text_.substr(start, pos_ - start));
    }
    bool parse_bool() {
        ws();
        if (text_[pos_] == 't') { pos_ += 4; return true; }
        pos_ += 5;
        return false;
    }
    // A JSON value that is either null or an integer -> optional<u64>.
    std::optional<std::uint64_t> parse_optional_number() {
        ws();
        if (text_[pos_] == 'n') { pos_ += 4; return std::nullopt; }
        return static_cast<std::uint64_t>(parse_number());
    }
    void skip_value() {
        const char c = cur();
        if (c == '"') { (void)parse_string(); }
        else if (c == '{') { skip_container('{', '}'); }
        else if (c == '[') { skip_container('[', ']'); }
        else if (c == 'n') { pos_ += 4; }
        else if (c == 't' || c == 'f') { (void)parse_bool(); }
        else { (void)parse_number(); }
    }
    void skip_container(char open, char close) {
        expect(open);
        int depth = 1;
        while (depth > 0 && pos_ < text_.size()) {
            const char c = text_[pos_];
            if (c == '"') { (void)parse_string(); continue; }
            if (c == open) ++depth;
            else if (c == close) --depth;
            ++pos_;
        }
    }

    // Read one flat object, dispatching each member by key to a callback.
    template <typename OnMember>
    void parse_object(OnMember on_member) {
        expect('{');
        if (try_consume('}')) return;
        for (;;) {
            ws();
            const std::string key = parse_string();
            ws();
            expect(':');
            on_member(key);
            if (try_consume(',')) continue;
            expect('}');
            break;
        }
    }
    template <typename OnElement>
    void parse_array(OnElement on_element) {
        expect('[');
        if (try_consume(']')) return;
        for (;;) {
            on_element();
            if (try_consume(',')) continue;
            expect(']');
            break;
        }
    }

    void parse_entries(Fixture& fixture) {
        parse_array([&] {
            std::string term;
            std::optional<std::uint64_t> value;
            parse_object([&](const std::string& key) {
                if (key == "term") term = parse_string();
                else if (key == "value") value = parse_optional_number();
                else skip_value();
            });
            fixture.entries.emplace_back(std::move(term), value);
        });
    }
    void parse_term_bool_array(std::vector<std::pair<std::string, bool>>& out,
                               const char* string_key) {
        parse_array([&] {
            std::string term;
            bool expected = false;
            parse_object([&](const std::string& key) {
                if (key == string_key) term = parse_string();
                else if (key == "expected") expected = parse_bool();
                else skip_value();
            });
            out.emplace_back(std::move(term), expected);
        });
    }
    void parse_pattern_number_array(std::vector<std::pair<std::string, std::size_t>>& out) {
        parse_array([&] {
            std::string pattern;
            std::size_t expected = 0;
            parse_object([&](const std::string& key) {
                if (key == "pattern") pattern = parse_string();
                else if (key == "expected") expected = static_cast<std::size_t>(parse_number());
                else skip_value();
            });
            out.emplace_back(std::move(pattern), expected);
        });
    }
    void parse_get(Fixture& fixture) {
        parse_array([&] {
            Fixture::Get record;
            parse_object([&](const std::string& key) {
                if (key == "term") record.term = parse_string();
                else if (key == "found") record.found = parse_bool();
                else if (key == "value") record.value = parse_optional_number();
                else skip_value();
            });
            fixture.get.push_back(std::move(record));
        });
    }
};

Fixture load_fixture(const std::string& path) {
    std::ifstream file(path, std::ios::binary);
    if (!file) throw std::runtime_error("cannot open fixture: " + path);
    std::stringstream buffer;
    buffer << file.rdbuf();
    return Parser(buffer.str()).parse();
}

std::vector<std::pair<std::string_view, std::optional<std::uint64_t>>>
entry_views(const Fixture& fixture) {
    std::vector<std::pair<std::string_view, std::optional<std::uint64_t>>> views;
    views.reserve(fixture.entries.size());
    for (const auto& [term, value] : fixture.entries) views.emplace_back(term, value);
    return views;
}

std::size_t rss_kib() {
    std::ifstream status("/proc/self/status");
    if (!status) return 0;
    std::string line;
    while (std::getline(status, line)) {
        if (line.rfind("VmRSS:", 0) == 0) {
            std::istringstream stream(line.substr(6));
            std::size_t value = 0;
            stream >> value;
            return value;
        }
    }
    return 0;
}

// ---------------------------------------------------------------------------
// C1 identity/version
// ---------------------------------------------------------------------------

void check_c1_identity() {
    CHECK(ld::abi_version() == 1);
    CHECK(ld::api_revision() == LDICT_API_REVISION);
}

void check_c1_kind_and_capabilities() {
    const std::uint64_t read = 1ull << 0, insert = 1ull << 1, remove = 1ull << 2,
                        clear = 1ull << 3, compact = 1ull << 4, substring = 1ull << 5,
                        checkpoint = 1ull << 6;
    ld::dynamic_dawg dawg;
    CHECK(dawg.kind() == ld::backend_kind::dynamic_dawg);
    const auto caps = dawg.capabilities();
    CHECK((caps & insert) && (caps & remove) && (caps & clear) && (caps & compact));
    CHECK(!(caps & substring) && !(caps & checkpoint));

    const std::pair<std::string_view, std::optional<std::uint64_t>> one[] = {{"x", std::nullopt}};
    ld::double_array_trie dat(EntrySpan{one});
    CHECK(dat.kind() == ld::backend_kind::double_array_trie);
    CHECK(dat.capabilities() & read);
    CHECK(!(dat.capabilities() & insert));

    ld::scdawg suffix;
    CHECK(suffix.kind() == ld::backend_kind::scdawg);
    CHECK(suffix.capabilities() & substring);
}

// ---------------------------------------------------------------------------
// C2 lifecycle/ownership (RAII: move leaves a null handle; free(null) is a no-op)
// ---------------------------------------------------------------------------

void check_c2_move_and_null_free() {
    ld::dynamic_dawg source;
    CHECK(source.insert("a"));
    ld::dynamic_dawg moved = std::move(source);
    // Both destruct at scope end: moved frees the real handle, source frees null.
    CHECK(moved.size() == 1);
    ldict_dictionary_free(nullptr); // documented no-op
}

void check_c2_free_order_independence() {
    // Heap-allocate so destruction order is under our control.
    std::vector<ld::dynamic_dawg*> dicts;
    for (int i = 0; i < 4; ++i) {
        auto* dawg = new ld::dynamic_dawg();
        CHECK(dawg->insert("term" + std::to_string(i), static_cast<std::uint64_t>(i)));
        dicts.push_back(dawg);
    }
    for (int index : {2, 0, 3, 1}) delete dicts[index];
}

// ---------------------------------------------------------------------------
// C3 error-mapping matrix + thread-local message
//
// Reachable through the idiomatic typed API: INVALID_UTF8 (3),
// DOMAIN_MISMATCH (9), IO_ERROR (7). N/A:
//   - NULL_POINTER (4):    a null handle throws std::invalid_argument at
//                          construction; ldict_dictionary_free(nullptr) is a
//                          documented no-op (exercised in C2).
//   - UNSUPPORTED (6):     no typed method exposes an unadvertised operation;
//                          capability bits are asserted absent instead (C5).
//   - LIMIT_EXCEEDED (10): persistent_vocabulary::term auto-sizes its buffer.
// ---------------------------------------------------------------------------

void check_c3_invalid_utf8() {
    ld::dynamic_dawg dawg(ld::unit_domain::unicode_scalar);
    try {
        (void)dawg.insert(std::string_view("\xFF", 1));
        CHECK(false); // should have thrown
    } catch (const ld::error& e) {
        CHECK(e.status() == LDICT_STATUS_INVALID_UTF8);
        CHECK(std::string(e.what()).size() > 0);
    }
}

void check_c3_domain_mismatch() {
    ld::dynamic_dawg dawg(ld::unit_domain::unicode_scalar);
    const std::uint64_t tokens[] = {1, 2};
    try {
        (void)dawg.insert(std::span<const std::uint64_t>(tokens));
        CHECK(false);
    } catch (const ld::error& e) {
        CHECK(e.status() == LDICT_STATUS_DOMAIN_MISMATCH);
    }
}

void check_c3_io_error() {
    const auto path = std::filesystem::temp_directory_path() / "ldict-cpp-does-not-exist.part";
    try {
        ld::persistent_artrie art(path, ld::persistent_artrie::open_mode::open);
        CHECK(false);
    } catch (const ld::error& e) {
        CHECK(e.status() == LDICT_STATUS_IO_ERROR);
        CHECK(std::string(e.what()).size() > 0);
    }
}

// ---------------------------------------------------------------------------
// C4 canonical fixture replay (cross-language oracle)
// ---------------------------------------------------------------------------

template <typename Reader>
void assert_fixture_reads(const Fixture& fixture, const Reader& reader) {
    CHECK(reader.size() == fixture.size);
    for (const auto& [term, expected] : fixture.contains)
        CHECK(reader.contains(term) == expected);
    for (const auto& item : fixture.get) {
        const auto lookup = reader.get(item.term);
        CHECK(lookup.found == item.found);
        CHECK(lookup.value == item.value);
    }
}

void check_c4_dynamic_dawg(const Fixture& fixture) {
    ld::dynamic_dawg dawg;
    const auto views = entry_views(fixture);
    CHECK(dawg.insert_all(EntrySpan{views}) == fixture.size);
    assert_fixture_reads(fixture, dawg);
}

void check_c4_double_array_trie(const Fixture& fixture) {
    const auto views = entry_views(fixture);
    ld::double_array_trie dat(EntrySpan{views});
    assert_fixture_reads(fixture, dat);
}

void check_c4_persistent_artrie(const Fixture& fixture) {
    const auto path = std::filesystem::temp_directory_path() /
                      ("ldict-cpp-c4-" + std::to_string(::getpid()) + ".part");
    auto wal_path = path;
    wal_path.replace_extension(".wal");
    std::filesystem::remove(path);
    std::filesystem::remove(wal_path);
    {
        ld::persistent_artrie art(path, ld::persistent_artrie::open_mode::create);
        for (const auto& [term, value] : fixture.entries) (void)art.insert(term, value);
        assert_fixture_reads(fixture, art);
    }
    std::filesystem::remove(path);
    std::filesystem::remove(wal_path);
    std::filesystem::remove(std::filesystem::path(path).concat(".wlock"));
}

void check_c4_scdawg(const Fixture& fixture) {
    ld::scdawg suffix;
    for (const auto& [term, value] : fixture.entries) (void)suffix.insert(term, value);
    for (const auto& [pattern, expected] : fixture.substring_frequency)
        CHECK(suffix.substring_frequency(pattern) == expected);
    for (const auto& [pattern, expected] : fixture.substring_contains)
        CHECK(suffix.contains_substring(pattern) == expected);
}

// ---------------------------------------------------------------------------
// C5 CRUD + value + batch + substring; capability-derived rejects
// ---------------------------------------------------------------------------

void check_c5_crud_round_trip() {
    ld::dynamic_dawg dawg;
    CHECK(dawg.insert("cat", 1));
    CHECK(!dawg.insert("cat", 1)); // idempotent
    CHECK(dawg.get("cat").value == std::optional<std::uint64_t>(1));
    CHECK(dawg.remove("cat"));
    CHECK(!dawg.remove("cat"));
    CHECK(!dawg.contains("cat"));
}

void check_c5_compact_preserves_terms() {
    ld::dynamic_dawg dawg;
    std::vector<std::pair<std::string, std::optional<std::uint64_t>>> owned;
    for (int i = 0; i < 50; ++i) owned.emplace_back("t" + std::to_string(i), static_cast<std::uint64_t>(i));
    std::vector<std::pair<std::string_view, std::optional<std::uint64_t>>> views;
    for (auto& [term, value] : owned) views.emplace_back(term, value);
    CHECK(dawg.insert_all(EntrySpan{views}) == 50);
    for (int i = 0; i < 50; i += 2) CHECK(dawg.remove("t" + std::to_string(i)));
    (void)dawg.compact();
    CHECK(dawg.size() == 25);
    CHECK(dawg.get("t1").value == std::optional<std::uint64_t>(1));
    CHECK(!dawg.contains("t0"));
}

void check_c5_substring_updates_with_inserts() {
    ld::scdawg suffix;
    CHECK(suffix.insert("cat", 1));
    CHECK(suffix.insert("cot", 2));
    CHECK(suffix.substring_frequency("t") == 2);
    CHECK(suffix.insert("cut", std::nullopt));
    CHECK(suffix.substring_frequency("t") == 3);
}

void check_c5_capability_derived_rejects() {
    const std::pair<std::string_view, std::optional<std::uint64_t>> one[] = {{"x", std::nullopt}};
    ld::double_array_trie dat(EntrySpan{one});
    const std::uint64_t insert = 1ull << 1, remove = 1ull << 2, clear = 1ull << 3, compact = 1ull << 4;
    CHECK(!(dat.capabilities() & (insert | remove | clear | compact)));
    ld::dynamic_dawg dawg(ld::unit_domain::unicode_scalar);
    const std::uint64_t tokens[] = {1};
    try {
        (void)dawg.insert(std::span<const std::uint64_t>(tokens));
        CHECK(false);
    } catch (const ld::error& e) {
        CHECK(e.status() == LDICT_STATUS_DOMAIN_MISMATCH);
    }
}

// ---------------------------------------------------------------------------
// C6 text domains and values
// ---------------------------------------------------------------------------

void check_c6_precomposed_and_multibyte() {
    ld::dynamic_dawg dawg;
    CHECK(dawg.insert("café", 7)); // precomposed U+00E9
    CHECK(dawg.insert("🦀", 255));  // 4-byte scalar
    CHECK(dawg.contains("café"));
    CHECK(dawg.get("🦀").value == std::optional<std::uint64_t>(255));
}

void check_c6_combining_distinct() {
    // Hex escapes keep the two byte sequences unambiguously distinct.
    const std::string precomposed = "caf\xC3\xA9";   // café, precomposed U+00E9
    const std::string combining = "cafe\xCC\x81";    // cafe + U+0301 combining acute
    ld::dynamic_dawg dawg;
    CHECK(dawg.insert(precomposed, 1));
    CHECK(dawg.insert(combining, 2));
    CHECK(dawg.size() == 2);
    CHECK(dawg.get(precomposed).value == std::optional<std::uint64_t>(1));
    CHECK(dawg.get(combining).value == std::optional<std::uint64_t>(2));
}

void check_c6_byte_domain() {
    ld::dynamic_dawg dawg(ld::unit_domain::byte);
    const std::string embedded_nul("a\0b", 3);
    const std::string invalid_utf8("\xFF\xFE", 2);
    CHECK(dawg.insert(std::string_view(embedded_nul), 1));
    CHECK(dawg.insert(std::string_view(invalid_utf8), 2));
    CHECK(dawg.contains(std::string_view(embedded_nul)));
    CHECK(dawg.get(std::string_view(invalid_utf8)).value == std::optional<std::uint64_t>(2));
}

void check_c6_u64_values() {
    ld::dynamic_dawg dawg(ld::unit_domain::u64);
    const std::uint64_t a[] = {1, 2, 3};
    const std::uint64_t b[] = {9};
    const std::uint64_t max = std::numeric_limits<std::uint64_t>::max();
    CHECK(dawg.insert(std::span<const std::uint64_t>(a), std::uint64_t{0}));
    CHECK(dawg.insert(std::span<const std::uint64_t>(b), max));
    CHECK(dawg.get(std::span<const std::uint64_t>(a)).value == std::optional<std::uint64_t>(0));
    CHECK(dawg.get(std::span<const std::uint64_t>(b)).value == std::optional<std::uint64_t>(max));
}

// ---------------------------------------------------------------------------
// C7 batch / paging edges
// ---------------------------------------------------------------------------

void check_c7_batch_sizes() {
    for (std::size_t size : {std::size_t{0}, std::size_t{1}, std::size_t{255},
                            std::size_t{256}, std::size_t{257}, std::size_t{1000}}) {
        ld::dynamic_dawg dawg;
        std::vector<std::pair<std::string, std::optional<std::uint64_t>>> owned;
        owned.reserve(size);
        for (std::size_t i = 0; i < size; ++i)
            owned.emplace_back("t" + std::to_string(i), static_cast<std::uint64_t>(i));
        std::vector<std::pair<std::string_view, std::optional<std::uint64_t>>> views;
        views.reserve(size);
        for (auto& [term, value] : owned) views.emplace_back(term, value);
        CHECK(dawg.insert_all(EntrySpan{views}) == size);
        CHECK(dawg.size() == size);
        if (size > 0) {
            CHECK(dawg.get("t0").value == std::optional<std::uint64_t>(0));
            CHECK(dawg.get("t" + std::to_string(size - 1)).value ==
                  std::optional<std::uint64_t>(size - 1));
        }
    }
}

void check_c7_snapshot_entry_range() {
    static_assert(std::ranges::input_range<ld::entries_view>);
    static_assert(std::ranges::view<ld::entries_view>);
    static_assert(!std::is_copy_constructible_v<ld::entries_view>);
    static_assert(std::is_nothrow_move_constructible_v<ld::entries_view>);

    ld::dynamic_dawg unicode;
    CHECK(unicode.insert("", std::nullopt));
    CHECK(unicode.insert("a", std::uint64_t{0}));
    CHECK(unicode.insert("é", std::numeric_limits<std::uint64_t>::max()));
    auto frozen = unicode.entries({1, 8, 1});
    CHECK(frozen.domain() == ld::unit_domain::unicode_scalar);
    CHECK(frozen.exact_size() == std::optional<std::size_t>(3));
    CHECK(unicode.insert("later", 7));

    std::vector<std::vector<std::uint32_t>> scalar_keys;
    std::vector<std::optional<std::uint64_t>> scalar_values;
    for (const ld::entry_view entry : frozen) {
        const auto units = entry.unicode_scalars();
        scalar_keys.emplace_back(units.begin(), units.end());
        scalar_values.push_back(entry.value());
    }
    CHECK((scalar_keys == std::vector<std::vector<std::uint32_t>>{{}, {'a'}, {0xE9}}));
    CHECK((scalar_values == std::vector<std::optional<std::uint64_t>>{
        std::nullopt, std::uint64_t{0}, std::numeric_limits<std::uint64_t>::max()}));

    // Breaking out of a range-for destroys the temporary view, which must
    // release its live lease before closing the opaque cursor.
    for (const ld::entry_view entry : unicode.entries({1, 8, 1})) {
        CHECK(!entry.unicode_scalars().empty() || entry.value() == std::nullopt);
        break;
    }
    std::size_t fresh_count = 0;
    for ([[maybe_unused]] const ld::entry_view entry : unicode.entries({2, 16, 2}))
        ++fresh_count;
    CHECK(fresh_count == 4);

    ld::dynamic_dawg bytes(ld::unit_domain::byte);
    const std::string raw("\0\xFF", 2);
    CHECK(bytes.insert(std::string_view(raw), std::nullopt));
    CHECK(bytes.insert("a", std::uint64_t{0}));
    std::vector<std::vector<std::uint8_t>> byte_keys;
    for (const ld::entry_view entry : bytes.entries({2, 8, 2})) {
        const auto units = entry.bytes();
        byte_keys.emplace_back(units.begin(), units.end());
    }
    CHECK((byte_keys == std::vector<std::vector<std::uint8_t>>{{0, 0xFF}, {'a'}}));

    ld::dynamic_dawg tokens(ld::unit_domain::u64);
    const std::uint64_t one[] = {1};
    const std::uint64_t one_nine[] = {1, 9};
    CHECK(tokens.insert(std::span<const std::uint64_t>(one), std::nullopt));
    CHECK(tokens.insert(std::span<const std::uint64_t>(one_nine), std::uint64_t{0}));
    std::vector<std::vector<std::uint64_t>> token_keys;
    std::vector<std::optional<std::uint64_t>> token_values;
    for (const ld::entry_view entry : tokens.entries({1, 2, 1})) {
        const auto units = entry.u64_units();
        token_keys.emplace_back(units.begin(), units.end());
        token_values.push_back(entry.value());
    }
    CHECK((token_keys == std::vector<std::vector<std::uint64_t>>{{1}, {1, 9}}));
    CHECK((token_values == std::vector<std::optional<std::uint64_t>>{
        std::nullopt, std::uint64_t{0}}));
}

// ---------------------------------------------------------------------------
// C8 property-based testing vs an in-language oracle
// ---------------------------------------------------------------------------

void check_c8_crud_script_vs_map() {
    std::mt19937_64 rng(0xC0FFEEull);
    std::vector<std::string> keys;
    for (int i = 0; i < 40; ++i) keys.push_back("k" + std::to_string(i));
    std::map<std::string, std::optional<std::uint64_t>> oracle;
    ld::dynamic_dawg dawg;
    std::uniform_real_distribution<double> unit(0.0, 1.0);
    std::uniform_int_distribution<std::size_t> pick(0, keys.size() - 1);
    for (int step = 0; step < 3000; ++step) {
        const std::string& key = keys[pick(rng)];
        const bool present = oracle.count(key) != 0;
        const double op = unit(rng);
        if (op < 0.5) {
            std::optional<std::uint64_t> value;
            if (rng() % 2 == 0) value = rng() >> 1;
            CHECK(dawg.insert(key, value) == !present);
            oracle[key] = value;
        } else if (op < 0.75) {
            CHECK(dawg.remove(key) == present);
            oracle.erase(key);
        } else if (op < 0.95) {
            CHECK(dawg.contains(key) == present);
            if (present) CHECK(dawg.get(key).value == oracle[key]);
        } else {
            (void)dawg.compact();
        }
        CHECK(dawg.size() == oracle.size());
    }
}

void check_c8_substring_vs_naive() {
    std::mt19937_64 rng(0x5CDAull);
    const std::string alphabet = "abcx";
    std::uniform_int_distribution<std::size_t> letter(0, alphabet.size() - 1);
    auto generate = [&](int maxLen) {
        int n = 1 + static_cast<int>(rng() % maxLen);
        std::string out;
        for (int i = 0; i < n; ++i) out.push_back(alphabet[letter(rng)]);
        return out;
    };
    std::set<std::string> terms;
    while (terms.size() < 60) terms.insert(generate(6));
    ld::scdawg suffix;
    for (const auto& term : terms) (void)suffix.insert(term);
    auto naive = [&](const std::string& pattern) {
        std::size_t total = 0;
        for (const auto& term : terms)
            for (std::size_t start = 0; start + pattern.size() <= term.size(); ++start)
                if (term.compare(start, pattern.size(), pattern) == 0) ++total;
        return total;
    };
    for (int i = 0; i < 200; ++i) {
        const std::string pattern = generate(3);
        const std::size_t expected = naive(pattern);
        CHECK(suffix.substring_frequency(pattern) == expected);
        CHECK(suffix.contains_substring(pattern) == (expected > 0));
    }
}

void check_c8_native_dictionary_algebra() {
    ld::dynamic_dawg left;
    ld::dynamic_dawg right;
    (void)left.insert("a", 1);
    (void)left.insert("shared", 7);
    (void)left.insert("valueless", std::nullopt);
    (void)right.insert("b", 2);
    (void)right.insert("shared", 11);
    (void)right.insert("valueless", 5);

    auto joined = left.set_union(right, ld::value_merge::lattice_join);
    CHECK(joined.size() == 4);
    CHECK(joined.get("shared").value == std::optional<std::uint64_t>(11));
    CHECK(joined.get("valueless").value == std::optional<std::uint64_t>(5));

    auto common = left.intersection(right);
    CHECK(common.size() == 2);
    CHECK(common.get("shared").value == std::optional<std::uint64_t>(7));
    CHECK(common.get("valueless").value == std::nullopt);

    auto only_left = left.difference(right);
    CHECK(only_left.size() == 1);
    CHECK(only_left.contains("a"));

    auto exclusive = left.symmetric_difference(right);
    CHECK(exclusive.size() == 2);
    CHECK(exclusive.contains("a"));
    CHECK(exclusive.contains("b"));

    (void)left.insert("later", 99);
    CHECK(!joined.contains("later"));
    CHECK(joined.insert("mutable-result", 23));
}

// ---------------------------------------------------------------------------
// C9 leak discipline
// ---------------------------------------------------------------------------

void check_c9_leak() {
    const int cycles = 12000;
    const std::pair<std::string_view, std::optional<std::uint64_t>> batch[] = {
        {"cat", 1}, {"cot", 2}, {"cut", std::nullopt}};
    for (int warmup = 0; warmup < 2000; ++warmup) {
        ld::dynamic_dawg dawg;
        (void)dawg.insert("cat", 1);
    }
    const std::size_t before = rss_kib();
    for (int i = 0; i < cycles; ++i) {
        ld::dynamic_dawg dawg;
        (void)dawg.insert_all(EntrySpan{batch});
        CHECK(dawg.contains("cot"));
    }
    const std::size_t after = rss_kib();
    if (before != 0 && after > before)
        CHECK(after - before < 32u * 1024u);
}

// ---------------------------------------------------------------------------
// C10 concurrency (single-writer / many-reader on a lock-free core)
// ---------------------------------------------------------------------------

void check_c10_independent_per_thread() {
    std::atomic<int> errors{0};
    std::vector<std::thread> workers;
    for (int seed = 0; seed < 8; ++seed) {
        workers.emplace_back([seed, &errors] {
            ld::dynamic_dawg dawg;
            for (int i = 0; i < 2000; ++i)
                if (!dawg.insert("t" + std::to_string(seed) + "_" + std::to_string(i),
                                 static_cast<std::uint64_t>(i)))
                    ++errors;
            if (dawg.size() != 2000) ++errors;
            if (dawg.get("t" + std::to_string(seed) + "_1500").value !=
                std::optional<std::uint64_t>(1500))
                ++errors;
        });
    }
    for (auto& worker : workers) worker.join();
    CHECK(errors.load() == 0);
}

void check_c10_readers_during_writer() {
    ld::dynamic_dawg dawg;
    std::vector<std::pair<std::string, std::optional<std::uint64_t>>> owned;
    for (int i = 0; i < 500; ++i) owned.emplace_back("seed" + std::to_string(i), static_cast<std::uint64_t>(i));
    std::vector<std::pair<std::string_view, std::optional<std::uint64_t>>> views;
    for (auto& [term, value] : owned) views.emplace_back(term, value);
    CHECK(dawg.insert_all(EntrySpan{views}) == 500);

    std::atomic<bool> stop{false};
    std::atomic<int> errors{0};
    std::vector<std::thread> readers;
    for (int r = 0; r < 4; ++r) {
        readers.emplace_back([&] {
            while (!stop.load(std::memory_order_relaxed)) {
                if (!dawg.contains("seed0")) ++errors;
                (void)dawg.get("seed250");
            }
        });
    }
    for (int i = 500; i < 3000; ++i)
        (void)dawg.insert("w" + std::to_string(i), static_cast<std::uint64_t>(i));
    stop.store(true);
    for (auto& reader : readers) reader.join();
    CHECK(errors.load() == 0);
    CHECK(dawg.get("w2999").value == std::optional<std::uint64_t>(2999));
}

} // namespace

int main(int argc, char** argv) {
    std::string fixture_path = "bindings/canonical_fixture.json";
    if (argc > 1) {
        fixture_path = argv[1];
    } else {
        for (const char* candidate :
             {"bindings/canonical_fixture.json", "../../canonical_fixture.json",
              "../canonical_fixture.json"}) {
            if (std::filesystem::exists(candidate)) { fixture_path = candidate; break; }
        }
    }

    check_c1_identity();
    check_c1_kind_and_capabilities();
    check_c2_move_and_null_free();
    check_c2_free_order_independence();
    check_c3_invalid_utf8();
    check_c3_domain_mismatch();
    check_c3_io_error();

    const Fixture fixture = load_fixture(fixture_path);
    check_c4_dynamic_dawg(fixture);
    check_c4_double_array_trie(fixture);
    check_c4_persistent_artrie(fixture);
    check_c4_scdawg(fixture);

    check_c5_crud_round_trip();
    check_c5_compact_preserves_terms();
    check_c5_substring_updates_with_inserts();
    check_c5_capability_derived_rejects();

    check_c6_precomposed_and_multibyte();
    check_c6_combining_distinct();
    check_c6_byte_domain();
    check_c6_u64_values();

    check_c7_batch_sizes();
    check_c7_snapshot_entry_range();
    check_c8_crud_script_vs_map();
    check_c8_substring_vs_naive();
    check_c8_native_dictionary_algebra();
    check_c9_leak();
    check_c10_independent_per_thread();
    check_c10_readers_during_writer();

    if (g_failures == 0) {
        std::cout << "cpp conformance: all checks passed\n";
        return 0;
    }
    std::cerr << "cpp conformance: " << g_failures << " check(s) failed\n";
    return 1;
}
