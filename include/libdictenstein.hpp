#ifndef LIBDICTENSTEIN_HPP
#define LIBDICTENSTEIN_HPP

#include "libdictenstein.h"

#include <cstddef>
#include <cstdint>
#include <filesystem>
#include <optional>
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

struct lookup final {
    bool found = false;
    std::optional<std::uint64_t> value;
};

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
    static LdictDictionary* create(unit_domain domain) {
        LdictDictionary* result = nullptr;
        check(ldict_dynamic_dawg_new(static_cast<std::uint32_t>(domain), &result));
        return result;
    }
};

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
