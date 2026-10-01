/* Compile-time public C layout controls for the revision-8 source page. */
#include "libdictenstein.h"
#include <stddef.h>

_Static_assert(sizeof(LdictOptionalU64) == 16, "optional value size");
_Static_assert(sizeof(LdictSuffixSourceRecord) ==
                   8 + sizeof(void*) + sizeof(size_t) + sizeof(LdictOptionalU64),
               "suffix record size");
_Static_assert(offsetof(LdictSuffixSourceRecord, source_id) == 0,
               "source ID offset");
_Static_assert(offsetof(LdictSuffixSourceRecord, data) == 8,
               "source data offset");
_Static_assert(offsetof(LdictSuffixSourceRecord, len) == 8 + sizeof(void*),
               "source length offset");
_Static_assert(offsetof(LdictSuffixSourceRecord, value) ==
                   8 + sizeof(void*) + sizeof(size_t),
               "embedded optional value offset");

int main(void) { return 0; }
