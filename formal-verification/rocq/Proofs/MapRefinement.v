(** * MapRefinement: Certified Map Interface for ARTrie

    Raw [ARTrie] operations remain preconditioned by representation
    completeness. This module exposes a total [MapImpl] over a wrapper that
    carries an explicit semantic entry list and a proof that the cached trie
    lookup agrees with that list.
*)

Require Import Coq.Lists.List.
Require Import ARTrie.Spec.MapSpec.
Require Import ARTrie.Spec.ARTrieSpec.
Require Import ARTrie.Model.Bucket.
Import ListNotations.

Record WFARTrie := mkWFARTrie {
  wf_entries : list KeyValue;
  wf_raw : ARTrie;
  wf_lookup_refines : forall k,
    trie_lookup wf_raw k = kv_lookup wf_entries k
}.

Definition wf_from_entries (entries : list KeyValue) : WFARTrie :=
  mkWFARTrie entries (build_canonical_trie entries)
    (canonical_lookup_correct entries).

Definition wf_empty : WFARTrie := wf_from_entries [].

Definition wf_lookup (t : WFARTrie) (k : MapSpec.Key) : option Value :=
  trie_lookup (wf_raw t) k.

Definition wf_insert (t : WFARTrie) (k : MapSpec.Key) (v : Value)
  : WFARTrie :=
  wf_from_entries (kv_upsert (wf_entries t) k v).

Definition wf_delete (t : WFARTrie) (k : MapSpec.Key) : WFARTrie :=
  wf_from_entries (kv_delete (wf_entries t) k).

Definition wf_interpret (t : WFARTrie) (k : MapSpec.Key) : option Value :=
  trie_lookup (wf_raw t) k.

Theorem wf_lookup_correct : forall t k,
  wf_lookup t k = wf_interpret t k.
Proof.
  reflexivity.
Qed.

Theorem wf_insert_correct : forall t k v k',
  wf_interpret (wf_insert t k v) k' =
    if MapSpec.key_eq_dec k k' then Some v else wf_interpret t k'.
Proof.
  intros t k v k'.
  unfold wf_interpret, wf_insert, wf_from_entries.
  change (trie_lookup
    (build_canonical_trie (kv_upsert (wf_entries t) k v)) k' =
    if MapSpec.key_eq_dec k k' then Some v else trie_lookup (wf_raw t) k').
  rewrite canonical_lookup_correct.
  destruct (MapSpec.key_eq_dec k k') as [Heq | Hneq].
  - subst. apply kv_lookup_upsert_same.
  - rewrite kv_lookup_upsert_other; [| exact Hneq].
    symmetry. apply wf_lookup_refines.
Qed.

Theorem wf_delete_correct : forall t k k',
  wf_interpret (wf_delete t k) k' =
    if MapSpec.key_eq_dec k k' then None else wf_interpret t k'.
Proof.
  intros t k k'.
  unfold wf_interpret, wf_delete, wf_from_entries.
  change (trie_lookup
    (build_canonical_trie (kv_delete (wf_entries t) k)) k' =
    if MapSpec.key_eq_dec k k' then None else trie_lookup (wf_raw t) k').
  rewrite canonical_lookup_correct.
  destruct (MapSpec.key_eq_dec k k') as [Heq | Hneq].
  - subst. apply kv_lookup_delete_same.
  - rewrite kv_lookup_delete_other; [| exact Hneq].
    symmetry. apply wf_lookup_refines.
Qed.

#[global]
Instance WFARTrieMapImpl : MapImpl Value := {
  map_t := WFARTrie;
  impl_empty := wf_empty;
  impl_lookup := wf_lookup;
  impl_insert := wf_insert;
  impl_delete := wf_delete;
  impl_interpret := wf_interpret;
  impl_lookup_correct := wf_lookup_correct;
  impl_insert_correct := wf_insert_correct;
  impl_delete_correct := wf_delete_correct;
}.
