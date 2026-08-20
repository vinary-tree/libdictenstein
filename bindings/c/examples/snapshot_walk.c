/* Construct a dictionary through the ldict_* C ABI, export its two-word
 * resource, capture an immutable snapshot, and walk the pinned revision while
 * the live dictionary keeps mutating.
 *
 * Compile check (repository root):
 *
 *   cc -std=c17 -Wall -Wextra -Werror -fsyntax-only -I include \
 *      bindings/c/examples/snapshot_walk.c
 */
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>

#include "libdictenstein.h"

static void fail(const char* where, LdictStatus status) {
    fprintf(stderr, "%s failed: status %d: %s\n", where, (int)status,
            ldict_last_error_message());
    exit(EXIT_FAILURE);
}

static void fail_vt(const char* where, VtStatus status) {
    fprintf(stderr, "%s failed: VtStatus %d\n", where, (int)status);
    exit(EXIT_FAILURE);
}

static uint64_t must_step(const VtDictionaryVTable* walk, void* context,
                          uint64_t node, uint64_t label) {
    uint64_t child = 0;
    uint8_t found = 0;
    VtStatus status = walk->node_transition(context, node, label, &child, &found);
    if (status != VT_STATUS_OK) fail_vt("node_transition", status);
    if (!found) {
        fprintf(stderr, "transition U+%04" PRIX64 " unexpectedly missing\n", label);
        exit(EXIT_FAILURE);
    }
    return child;
}

int main(void) {
    if (ldict_abi_version() != LDICT_ABI_VERSION ||
        ldict_api_revision() < LDICT_API_REVISION) {
        fprintf(stderr, "incompatible libdictenstein ABI/API\n");
        return EXIT_FAILURE;
    }

    LdictDictionary* dictionary = NULL;
    LdictStatus status =
        ldict_dynamic_dawg_new(VT_UNIT_DOMAIN_UNICODE_SCALAR, &dictionary);
    if (status != LDICT_STATUS_OK) fail("ldict_dynamic_dawg_new", status);

    uint8_t inserted = 0;
    status = ldict_dictionary_insert_text_value(
        dictionary, (const uint8_t*)"cat", 3, 41u, 1u, &inserted);
    if (status != LDICT_STATUS_OK) fail("insert \"cat\"", status);

    const LdictTextEntry batch[] = {
        {(const uint8_t*)"car", 3, {0u, 0u, {0}}},
        {(const uint8_t*)"cot", 3, {7u, 1u, {0}}},
    };
    size_t batch_inserted = 0;
    status = ldict_dictionary_insert_text_batch(dictionary, batch, 2,
                                                &batch_inserted);
    if (status != LDICT_STATUS_OK) fail("batch insert", status);
    printf("inserted %u + %zu terms\n", inserted, batch_inserted);

    uint8_t found = 0;
    uint64_t value = 0;
    uint8_t has_value = 0;
    status = ldict_dictionary_get_text_value(
        dictionary, (const uint8_t*)"cat", 3, &found, &value, &has_value);
    if (status != LDICT_STATUS_OK) fail("get \"cat\"", status);
    printf("cat: found=%u has_value=%u value=%" PRIu64 "\n", found, has_value,
           value);

    VtResource resource = {NULL, NULL};
    status = ldict_dictionary_resource(dictionary, &resource);
    if (status != LDICT_STATUS_OK) fail("ldict_dictionary_resource", status);
    resource.vtable->retain(resource.context);

    const void* raw_vtable = NULL;
    VtStatus vt_status = resource.vtable->query_interface(
        resource.context, &VT_DICTIONARY_INTERFACE_ID,
        VT_DICTIONARY_INTERFACE_VERSION, &raw_vtable);
    if (vt_status != VT_STATUS_OK) fail_vt("query_interface(source)", vt_status);
    const VtDictionaryVTable* source_vtable =
        (const VtDictionaryVTable*)raw_vtable;

    VtResource snapshot = {NULL, NULL};
    vt_status = source_vtable->snapshot(resource.context, &snapshot);
    if (vt_status != VT_STATUS_OK) fail_vt("snapshot", vt_status);

    const void* raw_walk = NULL;
    vt_status = snapshot.vtable->query_interface(
        snapshot.context, &VT_DICTIONARY_INTERFACE_ID,
        VT_DICTIONARY_INTERFACE_VERSION, &raw_walk);
    if (vt_status != VT_STATUS_OK) fail_vt("query_interface(snapshot)", vt_status);
    const VtDictionaryVTable* walk = (const VtDictionaryVTable*)raw_walk;
    printf("snapshot flags: immutable=%d suffix=%d\n",
           (walk->flags & VT_DICTIONARY_FLAG_IMMUTABLE) != 0,
           (walk->flags & VT_DICTIONARY_FLAG_SUFFIX_BASED) != 0);

    uint8_t removed = 0;
    status = ldict_dictionary_remove_text(dictionary, (const uint8_t*)"cot", 3,
                                          &removed);
    if (status != LDICT_STATUS_OK) fail("remove \"cot\"", status);

    size_t pinned_len = 0;
    uint8_t len_known = 0;
    vt_status = walk->len(snapshot.context, &pinned_len, &len_known);
    if (vt_status != VT_STATUS_OK) fail_vt("len", vt_status);
    printf("snapshot terms: %zu (known=%u)\n", pinned_len, len_known);

    uint64_t root = 0;
    vt_status = walk->root(snapshot.context, &root);
    if (vt_status != VT_STATUS_OK) fail_vt("root", vt_status);

    VtDictionaryEdge page[VT_RECOMMENDED_EDGE_BATCH];
    size_t start = 0;
    size_t total = 0;
    do {
        size_t written = 0;
        vt_status = walk->node_edges(snapshot.context, root, start, page,
                                     VT_RECOMMENDED_EDGE_BATCH, &written,
                                     &total);
        if (vt_status != VT_STATUS_OK) fail_vt("node_edges", vt_status);
        for (size_t i = 0; i < written; ++i) {
            uint8_t is_final = 0;
            vt_status = walk->node_is_final(snapshot.context, page[i].node,
                                            &is_final);
            if (vt_status != VT_STATUS_OK) fail_vt("node_is_final", vt_status);
            printf("root edge U+%04" PRIX64 " -> node %" PRIu64 " (final=%u)\n",
                   page[i].label, page[i].node, is_final);
        }
        start += written;
    } while (start < total);

    uint64_t node = root;
    for (const char* c = "cot"; *c != '\0'; ++c) {
        node = must_step(walk, snapshot.context, node,
                         (uint64_t)(unsigned char)*c);
    }
    uint8_t pinned_final = 0;
    vt_status = walk->node_is_final(snapshot.context, node, &pinned_final);
    if (vt_status != VT_STATUS_OK) fail_vt("node_is_final(\"cot\")", vt_status);
    VtOptionalU64 pinned_value = {0u, 0u, {0}};
    vt_status = walk->node_value_u64(snapshot.context, node, &pinned_value);
    if (vt_status != VT_STATUS_OK) fail_vt("node_value_u64(\"cot\")", vt_status);

    uint8_t live_contains = 0;
    status = ldict_dictionary_contains_text(dictionary, (const uint8_t*)"cot",
                                            3, &live_contains);
    if (status != LDICT_STATUS_OK) fail("contains \"cot\"", status);
    printf("\"cot\": snapshot final=%u value=%" PRIu64
           " (has_value=%u); live contains=%u\n",
           pinned_final, pinned_value.value, pinned_value.has_value,
           live_contains);

    snapshot.vtable->release(snapshot.context);
    resource.vtable->release(resource.context);
    ldict_dictionary_free(dictionary);
    return EXIT_SUCCESS;
}
