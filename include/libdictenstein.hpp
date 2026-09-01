#ifndef LIBDICTENSTEIN_HPP
#define LIBDICTENSTEIN_HPP

#include "libdictenstein.h"

#include <cstddef>
#include <cstdint>
#include <filesystem>
#include <iterator>
#include <optional>
#include <ranges>
#include <span>
#include <stdexcept>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

namespace vinary_tree::libdictenstein {

class error final : public std::runtime_error {
public:
    explicit error(LdictStatus status)
        : std::runtime_error(ldict_last_error_message()), status_(status) {}
    [[nodiscard]] LdictStatus status() const noexcept { return status_; }
private:
    LdictStatus status_;
};

inline void check(LdictStatus status) {
    if (status != LDICT_STATUS_OK) throw error(status);
}

// Native ABI version (LDICT_ABI_VERSION); always 1 for this family. Consumers
// gate on it before trusting any other symbol.
[[nodiscard]] inline std::uint32_t abi_version() noexcept { return ldict_abi_version(); }

// Compatible-additions revision within the ABI version (LDICT_API_REVISION).
[[nodiscard]] inline std::uint32_t api_revision() noexcept { return ldict_api_revision(); }

enum class unit_domain : std::uint32_t {
    byte = VT_UNIT_DOMAIN_BYTE,
    unicode_scalar = VT_UNIT_DOMAIN_UNICODE_SCALAR,
    u64 = VT_UNIT_DOMAIN_U64,
};

enum class backend_kind : std::uint32_t {
    dynamic_dawg = LDICT_KIND_DYNAMIC_DAWG,
    double_array_trie = LDICT_KIND_DOUBLE_ARRAY_TRIE,
    scdawg = LDICT_KIND_SCDAWG,
    persistent_artrie = LDICT_KIND_PERSISTENT_ARTRIE,
    persistent_vocabulary = LDICT_KIND_PERSISTENT_VOCAB_ARTRIE,
};

enum class algebra_operation : std::uint32_t {
    set_union = LDICT_ALGEBRA_UNION,
    intersection = LDICT_ALGEBRA_INTERSECTION,
    difference = LDICT_ALGEBRA_DIFFERENCE,
    symmetric_difference = LDICT_ALGEBRA_SYMMETRIC_DIFFERENCE,
};

enum class value_merge : std::uint32_t {
    first = LDICT_VALUE_MERGE_FIRST,
    last = LDICT_VALUE_MERGE_LAST,
    lattice_join = LDICT_VALUE_MERGE_LATTICE_JOIN,
    lattice_meet = LDICT_VALUE_MERGE_LATTICE_MEET,
};

struct lookup final {
    bool found = false;
    std::optional<std::uint64_t> value;
};

struct entry_batch_limits final {
    std::size_t max_entries = 256;
    std::size_t max_units = 4096;
    std::size_t max_values = 256;
};

class entries_view;
class dynamic_dawg;

class dictionary {
public:
    dictionary(const dictionary&) = delete;
    dictionary& operator=(const dictionary&) = delete;
    dictionary(dictionary&& other) noexcept : value_(std::exchange(other.value_, nullptr)) {}
    dictionary& operator=(dictionary&& other) noexcept {
        if (this != &other) {
            ldict_dictionary_free(value_);
            value_ = std::exchange(other.value_, nullptr);
        }
        return *this;
    }
    virtual ~dictionary() { ldict_dictionary_free(value_); }

    [[nodiscard]] backend_kind kind() const {
        std::uint32_t result = 0;
        check(ldict_dictionary_kind(value_, &result));
        return static_cast<backend_kind>(result);
    }
    [[nodiscard]] std::uint64_t capabilities() const {
        std::uint64_t result = 0;
        check(ldict_dictionary_capabilities(value_, &result));
        return result;
    }
    [[nodiscard]] std::size_t size() const {
        std::size_t result = 0;
        check(ldict_dictionary_len(value_, &result));
        return result;
    }
    [[nodiscard]] VtResource resource() const {
        VtResource result{};
        check(ldict_dictionary_resource(value_, &result));
        return result;
    }
    [[nodiscard]] bool contains(std::string_view term) const {
        std::uint8_t result = 0;
        check(ldict_dictionary_contains_text(value_, bytes(term), term.size(), &result));
        return result != 0;
    }
    [[nodiscard]] lookup get(std::string_view term) const {
        std::uint8_t found = 0;
        LdictOptionalU64 result{};
        check(ldict_dictionary_get_text(value_, bytes(term), term.size(), &found, &result));
        return {found != 0, result.has_value ? std::optional(result.value) : std::nullopt};
    }
    [[nodiscard]] bool contains(std::span<const std::uint64_t> term) const {
        std::uint8_t result = 0;
        check(ldict_dictionary_contains_u64(value_, term.data(), term.size(), &result));
        return result != 0;
    }
    [[nodiscard]] lookup get(std::span<const std::uint64_t> term) const {
        std::uint8_t found = 0;
        LdictOptionalU64 result{};
        check(ldict_dictionary_get_u64(value_, term.data(), term.size(), &found, &result));
        return {found != 0, result.has_value ? std::optional(result.value) : std::nullopt};
    }
    [[nodiscard]] entries_view entries(entry_batch_limits limits = {}) const;
    [[nodiscard]] dynamic_dawg algebra(
        const dictionary& right,
        algebra_operation operation = algebra_operation::set_union,
        value_merge merge = value_merge::last) const;
    [[nodiscard]] dynamic_dawg set_union(
        const dictionary& right, value_merge merge = value_merge::last) const;
    [[nodiscard]] dynamic_dawg intersection(
        const dictionary& right, value_merge merge = value_merge::lattice_meet) const;
    [[nodiscard]] dynamic_dawg difference(const dictionary& right) const;
    [[nodiscard]] dynamic_dawg symmetric_difference(const dictionary& right) const;

protected:
    explicit dictionary(LdictDictionary* value) : value_(value) {
        if (value == nullptr) throw std::invalid_argument("dictionary handle is null");
    }
    static const std::uint8_t* bytes(std::string_view term) noexcept {
        return reinterpret_cast<const std::uint8_t*>(term.data());
    }
    static LdictOptionalU64 abi_value(std::optional<std::uint64_t> value) noexcept {
        return {value.value_or(0), static_cast<std::uint8_t>(value.has_value()), {}};
    }
    [[nodiscard]] bool insert_text(std::string_view term, std::optional<std::uint64_t> value) {
        std::uint8_t inserted = 0;
        check(ldict_dictionary_insert_text(value_, bytes(term), term.size(), abi_value(value), &inserted));
        return inserted != 0;
    }
    [[nodiscard]] bool remove_text(std::string_view term) {
        std::uint8_t removed = 0;
        check(ldict_dictionary_remove_text(value_, bytes(term), term.size(), &removed));
        return removed != 0;
    }
    [[nodiscard]] bool insert_u64(std::span<const std::uint64_t> term,
                                  std::optional<std::uint64_t> value) {
        std::uint8_t inserted = 0;
        check(ldict_dictionary_insert_u64(value_, term.data(), term.size(), abi_value(value), &inserted));
        return inserted != 0;
    }
    [[nodiscard]] bool remove_u64(std::span<const std::uint64_t> term) {
        std::uint8_t removed = 0;
        check(ldict_dictionary_remove_u64(value_, term.data(), term.size(), &removed));
        return removed != 0;
    }
    [[nodiscard]] LdictDictionary* native_handle() const noexcept { return value_; }
private:
    LdictDictionary* value_;
};

// Borrowed reference into one leased native batch. It is valid only until its
// owning entries_view iterator advances beyond that batch or the view closes.
class entry_view final {
public:
    [[nodiscard]] unit_domain domain() const noexcept { return domain_; }

    [[nodiscard]] std::span<const std::uint8_t> bytes() const {
        require_domain(unit_domain::byte);
        return units<std::uint8_t>();
    }

    [[nodiscard]] std::span<const std::uint32_t> unicode_scalars() const {
        require_domain(unit_domain::unicode_scalar);
        return units<std::uint32_t>();
    }

    [[nodiscard]] std::span<const std::uint64_t> u64_units() const {
        require_domain(unit_domain::u64);
        return units<std::uint64_t>();
    }

    [[nodiscard]] std::optional<std::uint64_t> value() const {
        if (descriptor_.value_len == 0) return std::nullopt;
        if (descriptor_.value_len != 1 || values_ == nullptr)
            throw std::logic_error("invalid optional-u64 entry descriptor");
        return values_[descriptor_.value_offset];
    }

private:
    friend class entries_view;

    entry_view(LdictEntry descriptor, const void* units, const std::uint64_t* values,
               unit_domain domain) noexcept
        : descriptor_(descriptor), units_(units), values_(values), domain_(domain) {}

    void require_domain(unit_domain expected) const {
        if (domain_ != expected) throw std::logic_error("entry unit-domain mismatch");
    }

    template <typename Unit>
    [[nodiscard]] std::span<const Unit> units() const noexcept {
        if (descriptor_.unit_len == 0) return {};
        const auto* base = static_cast<const Unit*>(units_);
        return {base + descriptor_.unit_offset, descriptor_.unit_len};
    }

    LdictEntry descriptor_{};
    const void* units_ = nullptr;
    const std::uint64_t* values_ = nullptr;
    unit_domain domain_ = unit_domain::byte;
};

// Move-only, single-pass snapshot range. It owns an opaque native cursor and
// at most one borrowed batch lease; destruction cancels, releases, and closes
// in that order, including when a range-for loop exits early.
class entries_view final : public std::ranges::view_interface<entries_view> {
public:
    class iterator final {
    public:
        using iterator_concept = std::input_iterator_tag;
        using iterator_category = std::input_iterator_tag;
        using value_type = entry_view;
        using difference_type = std::ptrdiff_t;
        using reference = entry_view;

        iterator() noexcept = default;

        [[nodiscard]] entry_view operator*() const { return owner_->current(); }

        iterator& operator++() {
            owner_->advance();
            if (owner_->ended_) owner_ = nullptr;
            return *this;
        }

        void operator++(int) { ++*this; }

        friend bool operator==(const iterator& value, std::default_sentinel_t) noexcept {
            return value.owner_ == nullptr;
        }

        friend bool operator==(std::default_sentinel_t sentinel, const iterator& value) noexcept {
            return value == sentinel;
        }

    private:
        friend class entries_view;
        explicit iterator(entries_view* owner) noexcept : owner_(owner) {}
        entries_view* owner_ = nullptr;
    };

    entries_view() noexcept = default;
    entries_view(const entries_view&) = delete;
    entries_view& operator=(const entries_view&) = delete;

    entries_view(entries_view&& other) noexcept { move_from(other); }

    entries_view& operator=(entries_view&& other) noexcept {
        if (this != &other) {
            cleanup_noexcept();
            move_from(other);
        }
        return *this;
    }

    ~entries_view() { cleanup_noexcept(); }

    [[nodiscard]] iterator begin() {
        if (cursor_ == nullptr || ended_) return iterator{};
        if (!started_) {
            started_ = true;
            acquire_next();
        }
        return ended_ ? iterator{} : iterator{this};
    }

    [[nodiscard]] std::default_sentinel_t end() const noexcept { return {}; }

    [[nodiscard]] unit_domain domain() const noexcept {
        return static_cast<unit_domain>(info_.unit_domain);
    }

    [[nodiscard]] std::optional<std::size_t> exact_size() const noexcept {
        if ((info_.flags & LDICT_ENTRIES_INFO_FLAG_EXACT_LEN) == 0) return std::nullopt;
        return info_.exact_len;
    }

    [[nodiscard]] const LdictEntriesInfo& info() const noexcept { return info_; }

    void cancel() {
        if (cursor_ == nullptr || ended_) return;
        check(ldict_entry_cursor_cancel(cursor_));
        release_lease();
        ended_ = true;
    }

    void close() {
        if (cursor_ == nullptr) return;
        check(ldict_entry_cursor_cancel(cursor_));
        release_lease();
        check(ldict_entry_cursor_free(cursor_));
        cursor_ = nullptr;
        ended_ = true;
    }

private:
    friend class dictionary;

    entries_view(LdictDictionary* dictionary, entry_batch_limits limits)
        : limits_{limits.max_entries, limits.max_units, limits.max_values, 0} {
        if (limits.max_entries == 0)
            throw std::invalid_argument("entry batch max_entries must be nonzero");
        check(ldict_dictionary_entries_open(dictionary, &cursor_, &info_));
    }

    [[nodiscard]] entry_view current() const {
        return entry_view(batch_.entries[index_], batch_.units, batch_.values, domain());
    }

    void acquire_next() {
        const LdictStatus status = ldict_entry_cursor_next(cursor_, &limits_, &batch_);
        if (status == LDICT_STATUS_END) {
            ended_ = true;
            batch_ = {};
            index_ = 0;
            return;
        }
        check(status);
        leased_ = true;
        index_ = 0;
    }

    void advance() {
        if (ended_) return;
        ++index_;
        if (index_ < batch_.entry_count) return;
        release_lease();
        acquire_next();
    }

    void release_lease() {
        if (!leased_) return;
        check(ldict_entry_cursor_release(cursor_, batch_.generation));
        leased_ = false;
        batch_ = {};
        index_ = 0;
    }

    void cleanup_noexcept() noexcept {
        if (cursor_ == nullptr) return;
        static_cast<void>(ldict_entry_cursor_cancel(cursor_));
        if (leased_)
            static_cast<void>(ldict_entry_cursor_release(cursor_, batch_.generation));
        static_cast<void>(ldict_entry_cursor_free(cursor_));
        cursor_ = nullptr;
        leased_ = false;
        ended_ = true;
        batch_ = {};
    }

    void move_from(entries_view& other) noexcept {
        cursor_ = std::exchange(other.cursor_, nullptr);
        info_ = other.info_;
        limits_ = other.limits_;
        batch_ = other.batch_;
        index_ = other.index_;
        started_ = other.started_;
        ended_ = other.ended_;
        leased_ = other.leased_;
        other.batch_ = {};
        other.index_ = 0;
        other.started_ = false;
        other.ended_ = true;
        other.leased_ = false;
    }

    LdictEntryCursor* cursor_ = nullptr;
    LdictEntriesInfo info_{};
    LdictEntryBatchLimits limits_{};
    LdictEntryBatch batch_{};
    std::size_t index_ = 0;
    bool started_ = false;
    bool ended_ = false;
    bool leased_ = false;
};

inline entries_view dictionary::entries(entry_batch_limits limits) const {
    return entries_view(value_, limits);
}

class dynamic_dawg final : public dictionary {
public:
    explicit dynamic_dawg(unit_domain domain = unit_domain::unicode_scalar) : dictionary(create(domain)) {}
    using dictionary::contains;
    using dictionary::get;
    [[nodiscard]] bool insert(std::string_view term, std::optional<std::uint64_t> value = std::nullopt) {
        return insert_text(term, value);
    }
    [[nodiscard]] bool insert(std::span<const std::uint64_t> term,
                              std::optional<std::uint64_t> value = std::nullopt) {
        return insert_u64(term, value);
    }
    [[nodiscard]] bool remove(std::string_view term) { return remove_text(term); }
    [[nodiscard]] bool remove(std::span<const std::uint64_t> term) { return remove_u64(term); }
    void clear() { check(ldict_dictionary_clear(native_handle())); }
    [[nodiscard]] std::size_t compact() {
        std::size_t result = 0;
        check(ldict_dictionary_compact(native_handle(), &result));
        return result;
    }
    std::size_t insert_all(std::span<const std::pair<std::string_view, std::optional<std::uint64_t>>> entries) {
        std::vector<LdictTextEntry> descriptors;
        descriptors.reserve(entries.size());
        for (const auto& [term, value] : entries)
            descriptors.push_back({bytes(term), term.size(), abi_value(value)});
        std::size_t inserted = 0;
        check(ldict_dictionary_insert_text_batch(native_handle(), descriptors.data(), descriptors.size(), &inserted));
        return inserted;
    }
private:
    friend class dictionary;
    explicit dynamic_dawg(LdictDictionary* value) : dictionary(value) {}

    static LdictDictionary* create(unit_domain domain) {
        LdictDictionary* result = nullptr;
        check(ldict_dynamic_dawg_new(static_cast<std::uint32_t>(domain), &result));
        return result;
    }
};

inline dynamic_dawg dictionary::algebra(
    const dictionary& right, algebra_operation operation, value_merge merge) const {
    LdictDictionary* result = nullptr;
    check(ldict_dictionary_algebra(
        native_handle(), right.native_handle(), static_cast<std::uint32_t>(operation),
        static_cast<std::uint32_t>(merge), &result));
    return dynamic_dawg(result);
}

inline dynamic_dawg dictionary::set_union(const dictionary& right, value_merge merge) const {
    return algebra(right, algebra_operation::set_union, merge);
}

inline dynamic_dawg dictionary::intersection(const dictionary& right, value_merge merge) const {
    return algebra(right, algebra_operation::intersection, merge);
}

inline dynamic_dawg dictionary::difference(const dictionary& right) const {
    return algebra(right, algebra_operation::difference, value_merge::first);
}

inline dynamic_dawg dictionary::symmetric_difference(const dictionary& right) const {
    return algebra(right, algebra_operation::symmetric_difference, value_merge::first);
}

class double_array_trie final : public dictionary {
public:
    explicit double_array_trie(
        std::span<const std::pair<std::string_view, std::optional<std::uint64_t>>> entries,
        unit_domain domain = unit_domain::unicode_scalar)
        : dictionary(create(entries, domain)) {}
private:
    static LdictDictionary* create(
        std::span<const std::pair<std::string_view, std::optional<std::uint64_t>>> entries,
        unit_domain domain) {
        std::vector<LdictTextEntry> descriptors;
        descriptors.reserve(entries.size());
        for (const auto& [term, value] : entries)
            descriptors.push_back({bytes(term), term.size(), abi_value(value)});
        LdictDictionary* result = nullptr;
        check(ldict_double_array_trie_new(static_cast<std::uint32_t>(domain), descriptors.data(), descriptors.size(), &result));
        return result;
    }
};

class scdawg final : public dictionary {
public:
    explicit scdawg(unit_domain domain = unit_domain::unicode_scalar) : dictionary(create(domain)) {}
    [[nodiscard]] bool insert(std::string_view term, std::optional<std::uint64_t> value = std::nullopt) {
        return insert_text(term, value);
    }
    [[nodiscard]] bool contains_substring(std::string_view text) const {
        std::uint8_t result = 0;
        check(ldict_scdawg_contains_substring(native_handle(), bytes(text), text.size(), &result));
        return result != 0;
    }
    [[nodiscard]] std::size_t substring_frequency(std::string_view text) const {
        std::size_t result = 0;
        check(ldict_scdawg_substring_frequency(native_handle(), bytes(text), text.size(), &result));
        return result;
    }
private:
    static LdictDictionary* create(unit_domain domain) {
        LdictDictionary* result = nullptr;
        check(ldict_scdawg_new(static_cast<std::uint32_t>(domain), &result));
        return result;
    }
};

class persistent_artrie final : public dictionary {
public:
    enum class open_mode { create, open };
    persistent_artrie(const std::filesystem::path& path, open_mode mode,
                      unit_domain domain = unit_domain::unicode_scalar)
        : dictionary(make(path, mode, domain)) {}
    [[nodiscard]] bool insert(std::string_view term, std::optional<std::uint64_t> value = std::nullopt) {
        return insert_text(term, value);
    }
    [[nodiscard]] bool insert(std::span<const std::uint64_t> term,
                              std::optional<std::uint64_t> value = std::nullopt) {
        return insert_u64(term, value);
    }
    [[nodiscard]] bool remove(std::string_view term) { return remove_text(term); }
    [[nodiscard]] bool remove(std::span<const std::uint64_t> term) { return remove_u64(term); }
    void checkpoint() { check(ldict_dictionary_checkpoint(native_handle())); }
private:
    static LdictDictionary* make(const std::filesystem::path& path, open_mode mode, unit_domain domain) {
        const auto encoded = path.u8string();
        LdictDictionary* result = nullptr;
        const auto* data = reinterpret_cast<const std::uint8_t*>(encoded.data());
        check(mode == open_mode::create
            ? ldict_persistent_artrie_create(static_cast<std::uint32_t>(domain), data, encoded.size(), &result)
            : ldict_persistent_artrie_open(static_cast<std::uint32_t>(domain), data, encoded.size(), &result));
        return result;
    }
};

class persistent_vocabulary final : public dictionary {
public:
    enum class open_mode { create, open };
    persistent_vocabulary(const std::filesystem::path& path, open_mode mode)
        : dictionary(make(path, mode)) {}
    [[nodiscard]] bool put(std::string_view term, std::uint64_t index) { return insert_text(term, index); }
    [[nodiscard]] std::optional<std::string> term(std::uint64_t index) const {
        std::size_t len = 0;
        std::uint8_t found = 0;
        check(ldict_vocab_get_term(native_handle(), index, nullptr, 0, &len, &found));
        if (!found) return std::nullopt;
        std::string result(len, '\0');
        check(ldict_vocab_get_term(native_handle(), index,
                                   reinterpret_cast<std::uint8_t*>(result.data()), result.size(), &len, &found));
        return result;
    }
    void checkpoint() { check(ldict_dictionary_checkpoint(native_handle())); }
private:
    static LdictDictionary* make(const std::filesystem::path& path, open_mode mode) {
        const auto encoded = path.u8string();
        LdictDictionary* result = nullptr;
        const auto* data = reinterpret_cast<const std::uint8_t*>(encoded.data());
        check(mode == open_mode::create
            ? ldict_persistent_vocab_create(data, encoded.size(), &result)
            : ldict_persistent_vocab_open(data, encoded.size(), &result));
        return result;
    }
};

} // namespace vinary_tree::libdictenstein

#endif
