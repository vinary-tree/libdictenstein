/* An intentionally header-independent revision-7 call set. It must continue
 * to link and operate against the additive revision-8 shared library. */
#include <assert.h>
#include <stddef.h>
#include <stdint.h>

typedef struct LdictDictionary LdictDictionary;
extern uint32_t ldict_abi_version(void);
extern uint32_t ldict_api_revision(void);
extern int32_t ldict_dynamic_dawg_new(uint32_t, LdictDictionary**);
extern void ldict_dictionary_free(LdictDictionary*);
extern int32_t ldict_dictionary_insert_text_value(
    LdictDictionary*, const uint8_t*, size_t, uint64_t, uint8_t, uint8_t*);
extern int32_t ldict_dictionary_get_text_value(
    const LdictDictionary*, const uint8_t*, size_t, uint8_t*, uint64_t*,
    uint8_t*);

int main(void) {
    assert(ldict_abi_version() == 1);
    assert(ldict_api_revision() >= 7);
    LdictDictionary* dictionary = NULL;
    assert(ldict_dynamic_dawg_new(1, &dictionary) == 0);
    const uint8_t key[] = {0, 0xff};
    uint8_t inserted = 0;
    assert(ldict_dictionary_insert_text_value(
               dictionary, key, sizeof(key), 0, 1, &inserted) == 0);
    assert(inserted == 1);
    uint8_t found = 0;
    uint8_t has_value = 0;
    uint64_t value = UINT64_MAX;
    assert(ldict_dictionary_get_text_value(
               dictionary, key, sizeof(key), &found, &value, &has_value) == 0);
    assert(found == 1 && has_value == 1 && value == 0);
    ldict_dictionary_free(dictionary);
    return 0;
}
