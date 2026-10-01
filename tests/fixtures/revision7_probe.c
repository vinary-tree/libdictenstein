/* Deliberately exports only the revision-7 handshake. A revision-8 facade
 * must reject before resolving any new symbol against this probe library. */
#include <stdint.h>

uint32_t ldict_abi_version(void) { return 1; }
uint32_t ldict_api_revision(void) { return 7; }
