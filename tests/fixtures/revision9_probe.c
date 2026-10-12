/* Revision-9 loader probe deliberately lacks the additive revision-10 scalar
 * suffix insertion symbol. Its version handshake is the only callable API. */
#include <stdint.h>

uint32_t ldict_abi_version(void) { return 1; }
uint32_t ldict_api_revision(void) { return 9; }
const char* ldict_last_error_message(void) { return "revision-9 loader probe"; }
