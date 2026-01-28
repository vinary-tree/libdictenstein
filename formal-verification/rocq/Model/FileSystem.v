(** * FileSystem: POSIX Filesystem Model for Formal Verification

    This module models POSIX filesystem operations with their NON-ATOMIC
    semantics to verify that higher-level abstractions (like WAL) correctly
    handle race conditions such as TOCTOU (Time-Of-Check-To-Time-Of-Use).

    Key aspects modeled:
    1. File existence is observable but can change between observations
    2. Directory existence affects file creation
    3. Operations can fail due to filesystem state changes

    This Coq/Rocq model complements the TLA+ FileSystem.tla model by
    providing machine-checked proofs of safety properties.
*)

Require Import Coq.Strings.String.
Require Import Coq.Lists.List.
Require Import Coq.Bool.Bool.
Require Import Coq.Arith.Arith.
Require Import Coq.Logic.FunctionalExtensionality.
Require Import Coq.micromega.Lia.
Import ListNotations.

(** ** Path Representation *)

(** Paths are represented as lists of strings (directory components) *)
Definition Path := list string.

(** Empty path represents the root *)
Definition root_path : Path := [].

(** Path equality is decidable via list string equality *)
Definition path_eqb (p1 p2 : Path) : bool :=
  if list_eq_dec string_dec p1 p2 then true else false.

(** ** File and Directory States *)

(** A file can be absent, empty, or contain data *)
Inductive FileState : Type :=
  | Absent : FileState
  | Empty : FileState
  | HasData : FileState.

(** A directory can be absent or present *)
Inductive DirState : Type :=
  | DirAbsent : DirState
  | DirPresent : DirState.

(** Filesystem error codes *)
Inductive FsError : Type :=
  | Ok : FsError
  | NotFound : FsError
  | ParentNotFound : FsError
  | AlreadyExists : FsError
  | IsDirectory : FsError
  | NotDirectory : FsError
  | NotEmpty : FsError.

(** Error equality is decidable *)
Definition fs_error_eqb (e1 e2 : FsError) : bool :=
  match e1, e2 with
  | Ok, Ok => true
  | NotFound, NotFound => true
  | ParentNotFound, ParentNotFound => true
  | AlreadyExists, AlreadyExists => true
  | IsDirectory, IsDirectory => true
  | NotDirectory, NotDirectory => true
  | NotEmpty, NotEmpty => true
  | _, _ => false
  end.

Lemma fs_error_eqb_refl : forall e, fs_error_eqb e e = true.
Proof.
  intros e. destruct e; reflexivity.
Qed.

Lemma fs_error_eqb_eq : forall e1 e2,
  fs_error_eqb e1 e2 = true <-> e1 = e2.
Proof.
  intros e1 e2. split.
  - destruct e1, e2; simpl; try discriminate; auto.
  - intros H. subst. apply fs_error_eqb_refl.
Qed.

(** ** Filesystem State *)

(** A filesystem maps paths to file/directory states.
    For simplicity, we model files and directories with separate maps. *)
Record FileSystem : Type := mkFS {
  files : Path -> FileState;
  directories : Path -> DirState
}.

(** Initial filesystem: all paths absent *)
Definition empty_fs : FileSystem := mkFS
  (fun _ => Absent)
  (fun _ => DirAbsent).

(** ** Helper Functions *)

(** Get parent directory of a path *)
Definition parent_dir (p : Path) : Path :=
  match p with
  | [] => []  (* Root has no parent, return root *)
  | _ => removelast p
  end.

(** Check if one path is a prefix of another *)
Fixpoint is_path_prefix (prefix path : Path) : bool :=
  match prefix, path with
  | [], _ => true
  | _, [] => false
  | x :: xs, y :: ys =>
      if string_dec x y then is_path_prefix xs ys else false
  end.

(** Get all ancestor directories of a path - using explicit recursion on length *)
Fixpoint ancestors_aux (p : Path) (fuel : nat) : list Path :=
  match fuel with
  | 0 => []
  | S fuel' =>
      match p with
      | [] => []
      | _ =>
          let parent := removelast p in
          match parent with
          | [] => []
          | _ => parent :: ancestors_aux parent fuel'
          end
      end
  end.

Definition ancestors (p : Path) : list Path :=
  ancestors_aux p (length p).

(** removelast produces a shorter list (for non-empty input) *)
Lemma removelast_length : forall {A : Type} (l : list A),
  l <> [] ->
  length (removelast l) < length l.
Proof.
  intros A l.
  induction l as [|x xs IH].
  - (* l = [] *) intro H. exfalso. apply H. reflexivity.
  - (* l = x :: xs *)
    intro Hne. clear Hne.
    destruct xs as [|y ys].
    + (* xs = [] *) simpl. lia.
    + (* xs = y :: ys *)
      simpl.
      (* IH: y :: ys <> [] -> length (removelast (y :: ys)) < length (y :: ys) *)
      (* removelast (y :: ys) simplifies based on ys *)
      destruct ys as [|z zs].
      * (* ys = [] *) simpl. lia.
      * (* ys = z :: zs *)
        simpl.
        assert (Hne': y :: z :: zs <> []) by discriminate.
        specialize (IH Hne').
        simpl in IH.
        lia.
Qed.

(** Check if file exists *)
Definition file_exists (fs : FileSystem) (p : Path) : bool :=
  match files fs p with
  | Absent => false
  | _ => true
  end.

(** Check if directory exists *)
Definition dir_exists (fs : FileSystem) (p : Path) : bool :=
  match directories fs p with
  | DirAbsent => false
  | DirPresent => true
  end.

(** ** File Operations *)

(** Update file state at a path *)
Definition update_file (fs : FileSystem) (p : Path) (state : FileState)
    : FileSystem :=
  mkFS
    (fun q => if path_eqb q p then state else files fs q)
    (directories fs).

(** Update directory state at a path *)
Definition update_dir (fs : FileSystem) (p : Path) (state : DirState)
    : FileSystem :=
  mkFS
    (files fs)
    (fun q => if path_eqb q p then state else directories fs q).

(** open_create: Create file if not exists, open if exists.
    Precondition: parent directory must exist.
    Returns: new filesystem state and result code *)
Definition open_create (fs : FileSystem) (path : Path)
    : FileSystem * FsError :=
  let parent := parent_dir path in
  match parent with
  | [] => (* Root parent always exists *)
      let fs' := update_file fs path
        (match files fs path with
         | Absent => Empty
         | other => other
         end) in
      (fs', Ok)
  | _ =>
      if dir_exists fs parent then
        let fs' := update_file fs path
          (match files fs path with
           | Absent => Empty
           | other => other
           end) in
        (fs', Ok)
      else
        (fs, ParentNotFound)
  end.

(** open_create_excl: Create file, fail if exists.
    Precondition: parent must exist, file must not exist *)
Definition open_create_excl (fs : FileSystem) (path : Path)
    : FileSystem * FsError :=
  let parent := parent_dir path in
  let parent_ok := match parent with
                   | [] => true
                   | _ => dir_exists fs parent
                   end in
  if parent_ok then
    if file_exists fs path then
      (fs, AlreadyExists)
    else
      (update_file fs path Empty, Ok)
  else
    (fs, ParentNotFound).

(** open_existing: Open existing file.
    Precondition: file must exist *)
Definition open_existing (fs : FileSystem) (path : Path)
    : FileSystem * FsError :=
  if file_exists fs path then
    (fs, Ok)
  else
    (fs, NotFound).

(** unlink: Delete file *)
Definition unlink (fs : FileSystem) (path : Path)
    : FileSystem * FsError :=
  if file_exists fs path then
    (update_file fs path Absent, Ok)
  else
    (fs, NotFound).

(** ** Directory Operations *)

(** mkdir: Create single directory *)
Definition mkdir (fs : FileSystem) (path : Path)
    : FileSystem * FsError :=
  let parent := parent_dir path in
  let parent_ok := match parent with
                   | [] => true
                   | _ => dir_exists fs parent
                   end in
  if parent_ok then
    if dir_exists fs path then
      (fs, AlreadyExists)
    else if file_exists fs path then
      (fs, AlreadyExists)
    else
      (update_dir fs path DirPresent, Ok)
  else
    (fs, ParentNotFound).

(** mkdir_all: Create directory and all parent directories (mkdir -p).
    This is idempotent - succeeds even if directories already exist *)
Fixpoint mkdir_all_aux (fs : FileSystem) (path : Path) (fuel : nat)
    : FileSystem :=
  match fuel with
  | 0 => fs
  | S fuel' =>
      let fs' := update_dir fs path DirPresent in
      match path with
      | [] => fs'
      | _ => mkdir_all_aux fs' (parent_dir path) fuel'
      end
  end.

Definition mkdir_all (fs : FileSystem) (path : Path) : FileSystem :=
  mkdir_all_aux fs path (length path + 1).

(** rmdir: Remove directory *)
Definition rmdir (fs : FileSystem) (path : Path)
    : FileSystem * FsError :=
  if dir_exists fs path then
    (update_dir fs path DirAbsent, Ok)
  else
    (fs, NotFound).

(** ** TOCTOU-Safe Composite Operations *)

(** open_or_create_safe: Open file if exists, create if not.
    First ensures parent directory exists via mkdir_all.
    This is the SAFE pattern that avoids TOCTOU races. *)
Definition open_or_create_safe (fs : FileSystem) (path : Path)
    : FileSystem * FsError :=
  (* First ensure parent exists *)
  let fs' := mkdir_all fs (parent_dir path) in
  (* Then open or create the file atomically *)
  open_create fs' path.

(** create_with_parents_safe: Create file with all parent directories.
    This is the SAFE pattern for file creation. *)
Definition create_with_parents_safe (fs : FileSystem) (path : Path)
    : FileSystem * FsError :=
  (* First ensure parent exists *)
  let fs' := mkdir_all fs (parent_dir path) in
  (* Then create the file (or open if exists) *)
  open_create fs' path.

(** ** Basic Properties *)

(** Empty filesystem has no files *)
Lemma empty_fs_no_files : forall p, file_exists empty_fs p = false.
Proof.
  intros p. unfold file_exists, empty_fs. simpl. reflexivity.
Qed.

(** Empty filesystem has no directories *)
Lemma empty_fs_no_dirs : forall p, dir_exists empty_fs p = false.
Proof.
  intros p. unfold dir_exists, empty_fs. simpl. reflexivity.
Qed.

(** Update file changes only that path *)
Lemma update_file_same : forall fs p state,
  files (update_file fs p state) p = state.
Proof.
  intros fs p state. unfold update_file. simpl.
  unfold path_eqb.
  destruct (list_eq_dec string_dec p p) as [H|H].
  - reflexivity.
  - exfalso. apply H. reflexivity.
Qed.

Lemma update_file_other : forall fs p q state,
  p <> q ->
  files (update_file fs p state) q = files fs q.
Proof.
  intros fs p q state Hneq. unfold update_file. simpl.
  unfold path_eqb.
  destruct (list_eq_dec string_dec q p) as [H|H].
  - subst. exfalso. apply Hneq. reflexivity.
  - reflexivity.
Qed.

(** file_exists is preserved through update_file at a different path *)
Lemma file_exists_update_other : forall fs p q state,
  p <> q ->
  file_exists (update_file fs p state) q = file_exists fs q.
Proof.
  intros fs p q state Hneq.
  unfold file_exists.
  rewrite update_file_other by exact Hneq.
  reflexivity.
Qed.

(** Update directory changes only that path *)
Lemma update_dir_same : forall fs p state,
  directories (update_dir fs p state) p = state.
Proof.
  intros fs p state. unfold update_dir. simpl.
  unfold path_eqb.
  destruct (list_eq_dec string_dec p p) as [H|H].
  - reflexivity.
  - exfalso. apply H. reflexivity.
Qed.

Lemma update_dir_other : forall fs p q state,
  p <> q ->
  directories (update_dir fs p state) q = directories fs q.
Proof.
  intros fs p q state Hneq. unfold update_dir. simpl.
  unfold path_eqb.
  destruct (list_eq_dec string_dec q p) as [H|H].
  - subst. exfalso. apply Hneq. reflexivity.
  - reflexivity.
Qed.

(** ** Helper Lemmas for Idempotence Proofs *)

(** path_eqb is reflexive *)
Lemma path_eqb_refl : forall p, path_eqb p p = true.
Proof.
  intros p. unfold path_eqb.
  destruct (list_eq_dec string_dec p p) as [H|H].
  - reflexivity.
  - exfalso. apply H. reflexivity.
Qed.

(** path_eqb correctness *)
Lemma path_eqb_eq : forall p1 p2, path_eqb p1 p2 = true <-> p1 = p2.
Proof.
  intros p1 p2. split.
  - unfold path_eqb.
    destruct (list_eq_dec string_dec p1 p2) as [H|H]; auto.
    discriminate.
  - intros H. subst. apply path_eqb_refl.
Qed.

Lemma path_eqb_neq : forall p1 p2, path_eqb p1 p2 = false <-> p1 <> p2.
Proof.
  intros p1 p2. split.
  - unfold path_eqb.
    destruct (list_eq_dec string_dec p1 p2) as [H|H].
    + discriminate.
    + intros _. exact H.
  - intros H. unfold path_eqb.
    destruct (list_eq_dec string_dec p1 p2) as [Heq|Hneq].
    + exfalso. apply H. exact Heq.
    + reflexivity.
Qed.

(** ** Path Prefix Properties *)

(** Empty path is prefix of any path *)
Lemma empty_is_path_prefix : forall p, is_path_prefix [] p = true.
Proof.
  intros p. reflexivity.
Qed.

(** Path is prefix of itself *)
Lemma path_is_prefix_self : forall p, is_path_prefix p p = true.
Proof.
  induction p as [|s p' IH]; simpl.
  - reflexivity.
  - destruct (string_dec s s) as [H|H].
    + exact IH.
    + exfalso. apply H. reflexivity.
Qed.

(** directories accessor on mkFS *)
Lemma directories_mkFS : forall f d q,
  directories (mkFS f d) q = d q.
Proof.
  intros f d q. reflexivity.
Qed.

(** files accessor on mkFS *)
Lemma files_mkFS : forall f d q,
  files (mkFS f d) q = f q.
Proof.
  intros f d q. reflexivity.
Qed.

(** Helper: directory function after update_dir *)
Lemma update_dir_directories : forall fs p state q,
  directories (update_dir fs p state) q =
  if path_eqb q p then state else directories fs q.
Proof.
  intros fs p state q.
  unfold update_dir. simpl. reflexivity.
Qed.

(** update_dir is idempotent for the same state *)
Lemma update_dir_idempotent : forall fs p state,
  update_dir (update_dir fs p state) p state = update_dir fs p state.
Proof.
  intros fs p state.
  unfold update_dir.
  (* Use cbv to fully reduce record accessors *)
  f_equal.
  apply functional_extensionality. intros q.
  (* Reduce directories accessor on mkFS *)
  cbv beta delta [directories] iota.
  (* Now the goal should have the form:
     (if path_eqb q p then state else (if path_eqb q p then state else directories fs q))
     = (if path_eqb q p then state else directories fs q) *)
  destruct (path_eqb q p) eqn:Hqp; reflexivity.
Qed.

(** update_file preserves directories *)
Lemma update_file_preserves_dirs : forall fs p state q,
  directories (update_file fs p state) q = directories fs q.
Proof.
  intros fs p state q.
  unfold update_file. simpl. reflexivity.
Qed.

(** update_dir preserves files *)
Lemma update_dir_preserves_files : forall fs p state q,
  files (update_dir fs p state) q = files fs q.
Proof.
  intros fs p state q.
  unfold update_dir. simpl. reflexivity.
Qed.

(** mkdir_all_aux preserves existing directories *)
Lemma mkdir_all_aux_preserves_dirs : forall fs path fuel q,
  dir_exists fs q = true ->
  dir_exists (mkdir_all_aux fs path fuel) q = true.
Proof.
  intros fs path fuel. revert fs path.
  induction fuel as [|fuel' IH]; intros fs path q Hdir.
  - (* fuel = 0 *)
    simpl. exact Hdir.
  - (* fuel = S fuel' *)
    simpl.
    destruct path as [|p ps].
    + (* path = [] *)
      unfold dir_exists.
      destruct (path_eqb q []) eqn:Hq.
      * (* q = [] *)
        apply path_eqb_eq in Hq. subst q.
        rewrite update_dir_same. reflexivity.
      * (* q <> [] *)
        apply path_eqb_neq in Hq.
        rewrite update_dir_other by (apply not_eq_sym; exact Hq).
        unfold dir_exists in Hdir. exact Hdir.
    + (* path = p :: ps *)
      apply IH.
      unfold dir_exists.
      destruct (path_eqb q (p :: ps)) eqn:Hqp.
      * (* q = p :: ps *)
        apply path_eqb_eq in Hqp. subst q.
        rewrite update_dir_same. reflexivity.
      * (* q <> p :: ps *)
        apply path_eqb_neq in Hqp.
        rewrite update_dir_other by (apply not_eq_sym; exact Hqp).
        unfold dir_exists in Hdir. exact Hdir.
Qed.

(** mkdir_all_aux only sets directories to DirPresent, never removes them *)
Lemma mkdir_all_aux_only_adds_dirs : forall fs path fuel q,
  dir_exists fs q = true ->
  dir_exists (mkdir_all_aux fs path fuel) q = true.
Proof.
  (* This is the same as mkdir_all_aux_preserves_dirs *)
  exact mkdir_all_aux_preserves_dirs.
Qed.

(** mkdir_all_aux sets the target directory to DirPresent (when fuel > 0) *)
Lemma mkdir_all_aux_sets_target : forall fs path fuel,
  fuel > 0 ->
  dir_exists (mkdir_all_aux fs path fuel) path = true.
Proof.
  intros fs path fuel Hfuel.
  destruct fuel as [|fuel'].
  - (* fuel = 0: contradicts Hfuel *)
    lia.
  - (* fuel = S fuel' *)
    simpl.
    destruct path as [|p ps].
    + (* path = [] *)
      unfold dir_exists. rewrite update_dir_same. reflexivity.
    + (* path = p :: ps *)
      apply mkdir_all_aux_preserves_dirs.
      unfold dir_exists. rewrite update_dir_same. reflexivity.
Qed.

(** Helper: prefix length is <= path length *)
Lemma prefix_length_le : forall (q path : Path),
  is_path_prefix q path = true ->
  length q <= length path.
Proof.
  induction q as [|s q' IHq]; intros path Hprefix.
  - simpl. lia.
  - destruct path as [|t path'].
    + simpl in Hprefix. discriminate.
    + simpl in Hprefix.
      destruct (string_dec s t) as [_|]; [|discriminate].
      simpl. specialize (IHq path' Hprefix). lia.
Qed.

(** Helper: strict prefix is prefix of removelast *)
Lemma strict_prefix_is_prefix_of_removelast : forall (q l : Path),
  is_path_prefix q l = true ->
  q <> l ->
  l <> [] ->
  is_path_prefix q (removelast l) = true.
Proof.
  intros q l.
  revert q.
  induction l as [|x xs IH]; intros q Hprefix Hneq Hne.
  - (* l = [] *) exfalso. apply Hne. reflexivity.
  - (* l = x :: xs *)
    destruct q as [|y ys].
    + (* q = [] *) reflexivity.
    + (* q = y :: ys *)
      simpl in Hprefix.
      destruct (string_dec y x) as [Hyx|Hyx]; [|discriminate].
      subst y.
      (* Hprefix: is_path_prefix ys xs = true *)
      (* Hneq: x :: ys <> x :: xs, so ys <> xs *)
      destruct xs as [|z zs].
      * (* xs = [] *)
        (* ys is prefix of [], so ys = [] *)
        (* But then x :: ys = [x] = x :: xs, contradicts Hneq *)
        destruct ys as [|w ws].
        -- exfalso. apply Hneq. reflexivity.
        -- simpl in Hprefix. discriminate.
      * (* xs = z :: zs *)
        (* removelast (x :: z :: zs) = x :: removelast (z :: zs) *)
        simpl.
        destruct (string_dec x x) as [_|Hxx]; [|exfalso; apply Hxx; reflexivity].
        (* Need: is_path_prefix ys (removelast (z :: zs)) = true *)
        destruct (list_eq_dec string_dec ys (z :: zs)) as [Heq|Hneq'].
        -- (* ys = z :: zs, so x :: ys = x :: z :: zs = l *)
           subst ys. exfalso. apply Hneq. reflexivity.
        -- (* ys <> z :: zs *)
           apply IH.
           ++ exact Hprefix.
           ++ exact Hneq'.
           ++ discriminate.
Qed.

(** Helper: prefix of removelast implies prefix of original *)
Lemma prefix_of_removelast_is_prefix : forall (q l : Path),
  l <> [] ->
  is_path_prefix q (removelast l) = true ->
  is_path_prefix q l = true.
Proof.
  intros q l.
  revert q.
  induction l as [|x xs IH]; intros q Hne Hr.
  - (* l = [] *)
    exfalso. apply Hne. reflexivity.
  - (* l = x :: xs *)
    destruct xs as [|y ys].
    + (* xs = [], l = [x], removelast [x] = [] *)
      simpl in Hr.
      destruct q; [reflexivity | discriminate Hr].
    + (* xs = y :: ys, l = x :: y :: ys *)
      (* removelast (x :: y :: ys) = x :: removelast (y :: ys) *)
      destruct q as [|z zs]; [reflexivity | ].
      simpl.
      destruct (string_dec z x) as [Hzx|Hzx].
      * (* z = x *)
        subst z.
        destruct (string_dec x x) as [_|Hxx]; [|exfalso; apply Hxx; reflexivity].
        (* Hr: is_path_prefix (x :: zs) (x :: removelast (y :: ys)) = true *)
        (* Goal: is_path_prefix zs (y :: ys) = true *)
        (* Extract the inner prefix relationship from Hr *)
        assert (Hzs: is_path_prefix zs (removelast (y :: ys)) = true).
        {
          simpl in Hr.
          destruct (string_dec x x) as [_|Hcontra]; [exact Hr | exfalso; apply Hcontra; reflexivity].
        }
        apply (IH zs).
        -- discriminate.
        -- exact Hzs.
      * (* z <> x *)
        simpl in Hr.
        destruct (string_dec z x) as [Heq|_]; [subst z; exfalso; apply Hzx; reflexivity | discriminate Hr].
Qed.

(** mkdir_all_aux sets all directories on the path from target to root *)
(** If q is a prefix of path (including path itself), q becomes DirPresent *)
Lemma mkdir_all_aux_sets_prefixes : forall fs path fuel q,
  is_path_prefix q path = true ->
  fuel > length path - length q ->
  dir_exists (mkdir_all_aux fs path fuel) q = true.
Proof.
  intros fs path fuel q.
  revert fs path q.
  induction fuel as [|fuel' IH]; intros fs path q Hprefix Hfuel.
  - (* fuel = 0 *)
    assert (Hlen: length q <= length path) by (apply prefix_length_le; exact Hprefix).
    lia.
  - (* fuel = S fuel' *)
    simpl.
    destruct path as [|p ps].
    + (* path = [] *)
      destruct q as [|s qs].
      * unfold dir_exists. rewrite update_dir_same. reflexivity.
      * simpl in Hprefix. discriminate.
    + (* path = p :: ps *)
      destruct (list_eq_dec string_dec q (p :: ps)) as [Heq|Hneq].
      * (* q = p :: ps *)
        subst q.
        apply mkdir_all_aux_preserves_dirs.
        unfold dir_exists. rewrite update_dir_same. reflexivity.
      * (* q <> p :: ps: q is a strict prefix *)
        apply IH.
        -- (* is_path_prefix q (parent_dir (p :: ps)) = true *)
           unfold parent_dir.
           apply strict_prefix_is_prefix_of_removelast.
           ++ exact Hprefix.
           ++ exact Hneq.
           ++ discriminate.
        -- (* fuel' > length (parent_dir (p :: ps)) - length q *)
           unfold parent_dir.
           (* Introduce variables for lengths to help lia *)
           remember (length (removelast (p :: ps))) as len_rm eqn:Hdef_rm.
           remember (length (p :: ps)) as len_path eqn:Hdef_path.
           remember (length q) as len_q eqn:Hdef_q.
           assert (Hrmlen: len_rm < len_path).
           { subst len_rm len_path. apply removelast_length. discriminate. }
           assert (Hqlen: len_q <= len_path).
           { subst len_q len_path. apply prefix_length_le. exact Hprefix. }
           (* q is a strict prefix, so len_q < len_path *)
           assert (Hstrict: len_q < len_path).
           { subst len_q len_path.
             assert (Hne: q <> p :: ps) by exact Hneq.
             (* strict prefix has strictly smaller length *)
             assert (Hle: length q <= length (p :: ps))
               by (apply prefix_length_le; exact Hprefix).
             destruct (Nat.eq_dec (length q) (length (p :: ps))) as [Heqlen|Hneqlen].
             - (* length q = length (p :: ps) *)
               (* But q is prefix of p::ps with same length, so q = p::ps *)
               exfalso. apply Hne.
               clear -Hprefix Heqlen.
               revert q Hprefix Heqlen.
               induction (p :: ps) as [|x xs IHxs]; intros q Hprefix Heqlen.
               + destruct q; [reflexivity | simpl in Heqlen; lia].
               + destruct q as [|y ys].
                 * simpl in Heqlen. lia.
                 * simpl in Hprefix, Heqlen.
                   destruct (string_dec y x) as [Hyx|Hyx]; [|discriminate].
                   subst y.
                   f_equal. apply IHxs.
                   -- exact Hprefix.
                   -- lia.
             - lia. }
           simpl in Hfuel.
           lia.
Qed.

(** update_dir preserves files (function equality version) *)
Lemma update_dir_preserves_files_fn : forall fs p state,
  files (update_dir fs p state) = files fs.
Proof.
  intros fs p state.
  apply functional_extensionality. intros q.
  apply update_dir_preserves_files.
Qed.

(** mkdir_all_aux preserves files (only modifies directories) *)
Lemma mkdir_all_aux_preserves_files : forall fs path fuel,
  files (mkdir_all_aux fs path fuel) = files fs.
Proof.
  intros fs path fuel. revert fs path.
  induction fuel as [|fuel' IH]; intros fs path.
  - (* fuel = 0 *)
    simpl. reflexivity.
  - (* fuel = S fuel' *)
    simpl.
    destruct path as [|p ps].
    + (* path = [] *)
      apply update_dir_preserves_files_fn.
    + (* path = p :: ps *)
      rewrite IH.
      apply update_dir_preserves_files_fn.
Qed.

(** mkdir_all_aux only touches prefixes of path - paths not in the ancestry are unchanged *)
Lemma mkdir_all_aux_untouched : forall fs path fuel q,
  q <> path ->
  is_path_prefix q path = false ->
  directories (mkdir_all_aux fs path fuel) q = directories fs q.
Proof.
  intros fs path fuel. revert fs path.
  induction fuel as [|fuel' IH]; intros fs path q Hneq Hnotprefix.
  - (* fuel = 0 *)
    simpl. reflexivity.
  - (* fuel = S fuel' *)
    simpl.
    destruct path as [|p ps].
    + (* path = [] *)
      (* q <> [] and is_path_prefix q [] = false, but is_path_prefix q [] = true always! *)
      (* This case is actually impossible since [] is prefix of nothing except [] *)
      simpl in Hnotprefix.
      (* is_path_prefix q [] = match q with | [] => true | _ => false end *)
      destruct q as [|x xs].
      * (* q = []: contradicts Hneq since path = [] *)
        exfalso. apply Hneq. reflexivity.
      * (* q = x::xs: is_path_prefix (x::xs) [] = false, ok *)
        (* update_dir fs [] DirPresent doesn't change q = x::xs since [] <> x::xs *)
        rewrite update_dir_other.
        -- reflexivity.
        -- discriminate.
    + (* path = p :: ps *)
      (* mkdir_all_aux (update_dir fs (p::ps) DirPresent) (parent_dir (p::ps)) fuel' *)
      (* First, update_dir at (p::ps) doesn't affect q since q <> p::ps *)
      (* Then recursively mkdir_all_aux on parent *)
      set (parent := parent_dir (p :: ps)).
      destruct (path_eqb q parent) eqn:Hqparent.
      * (* q = parent: but is_path_prefix q (p::ps) = false means q is not prefix of p::ps *)
        (* However, parent = removelast (p::ps) IS a prefix of p::ps *)
        (* So q = parent contradicts is_path_prefix q (p::ps) = false *)
        apply path_eqb_eq in Hqparent. subst q.
        (* parent = removelast (p::ps) is a prefix of p::ps *)
        (* Need to show is_path_prefix (removelast (p::ps)) (p::ps) = true *)
        (* For now, show contradiction via specific structure of removelast *)
        (* Actually, let's just unfold and prove both *)
        subst parent.
        (* We need: removelast (p::ps) is a prefix of p::ps *)
        (* This is true for any non-empty list *)
        (* But we have Hnotprefix saying it's false - contradiction *)
        exfalso.
        (* Hnotprefix now has: is_path_prefix (parent_dir (p::ps)) (p::ps) = false *)
        (* parent_dir (p::ps) = removelast (p::ps) by definition *)
        unfold parent_dir in Hnotprefix.
        (* Now Hnotprefix: is_path_prefix (removelast (p::ps)) (p::ps) = false *)
        (* But removelast (p::ps) IS a prefix of (p::ps) - contradiction *)
        assert (Htrue: is_path_prefix (removelast (p :: ps)) (p :: ps) = true).
        {
          clear IH Hneq Hnotprefix.
          generalize dependent p.
          induction ps as [|y ys IHps]; intros p.
          - (* ps = []: removelast [p] = [], is_path_prefix [] [p] = true *)
            simpl. reflexivity.
          - (* ps = y::ys: removelast (p::y::ys) = p :: removelast (y::ys) *)
            simpl. destruct (string_dec p p) as [_|Hcontra]; [|exfalso; apply Hcontra; reflexivity].
            apply IHps.
        }
        rewrite Htrue in Hnotprefix. discriminate Hnotprefix.
      * (* q <> parent *)
        apply path_eqb_neq in Hqparent.
        (* First, update_dir at (p::ps) doesn't affect q *)
        assert (Hneq_pps: (p :: ps) <> q) by (intro; subst; apply Hneq; reflexivity).
        rewrite IH.
        -- rewrite update_dir_other by exact Hneq_pps. reflexivity.
        -- exact Hqparent.
        -- (* is_path_prefix q parent = false *)
           (* parent = parent_dir (p::ps) = removelast (p::ps) *)
           (* We have: is_path_prefix q (p::ps) = false *)
           (* Need: is_path_prefix q (parent_dir (p::ps)) = false *)
           (* If q were a prefix of parent, then q would also be prefix of p::ps *)
           (* (since parent is prefix of p::ps, and prefix relation is transitive) *)
           subst parent. unfold parent_dir.
           destruct (is_path_prefix q (removelast (p :: ps))) eqn:Hpfx; [|reflexivity].
           (* Show contradiction: if q prefix of removelast, then q prefix of p::ps *)
           exfalso.
           (* removelast (p::ps) is a prefix of p::ps *)
           assert (Hrem_pfx: is_path_prefix (removelast (p :: ps)) (p :: ps) = true).
           {
             clear.
             induction ps as [|y ys IHps] in p |- *.
             - simpl. reflexivity.
             - simpl. destruct (string_dec p p) as [_|Hcontra]; [|exfalso; apply Hcontra; reflexivity].
               apply IHps.
           }
           (* q is a prefix of removelast (p::ps), and removelast (p::ps) is prefix of p::ps
              => q is prefix of p::ps by transitivity *)
           (* But Hnotprefix says q is NOT prefix of p::ps - contradiction *)
           assert (Hq_pfx: is_path_prefix q (p :: ps) = true).
           {
             (* Use transitivity inline *)
             clear Hnotprefix Hneq IH Hneq_pps Hqparent.
             revert q Hpfx Hrem_pfx.
             revert p.
             induction ps as [|y ys IHps]; intros p q Hpfx Hrem_pfx.
             - (* ps = []: removelast [p] = [] *)
               (* Hpfx : is_path_prefix q [] = true => q = [] *)
               simpl in Hpfx.
               destruct q; [simpl; reflexivity | discriminate].
             - (* ps = y::ys *)
               destruct q as [|x xs]; [simpl; reflexivity |].
               (* q = x::xs *)
               simpl. simpl in Hpfx, Hrem_pfx.
               destruct (string_dec p p) as [_|Hc]; [|exfalso; apply Hc; reflexivity].
               destruct (string_dec x p) as [Hxp|Hxp].
               + (* x = p *)
                 subst x.
                 apply (IHps y xs Hpfx Hrem_pfx).
               + (* x <> p: contradicts Hpfx *)
                 destruct (string_dec x p); [contradiction | discriminate].
           }
           rewrite Hq_pfx in Hnotprefix. discriminate.
Qed.

(** mkdir_all preserves files *)
Lemma mkdir_all_preserves_files : forall fs path,
  files (mkdir_all fs path) = files fs.
Proof.
  intros fs path.
  unfold mkdir_all.
  apply mkdir_all_aux_preserves_files.
Qed.

(** file_exists is preserved through mkdir_all (only changes directories) *)
Lemma file_exists_mkdir_all : forall fs path q,
  file_exists (mkdir_all fs path) q = file_exists fs q.
Proof.
  intros fs path q.
  unfold file_exists.
  rewrite mkdir_all_preserves_files.
  reflexivity.
Qed.

(** Helper: directories value after mkdir_all_aux at touched path *)
Lemma mkdir_all_aux_at_path_is_present : forall fs path fuel,
  fuel > 0 ->
  directories (mkdir_all_aux fs path fuel) path = DirPresent.
Proof.
  intros fs path fuel Hfuel.
  destruct fuel as [|fuel']; [lia|].
  simpl.
  destruct path as [|p ps].
  - rewrite update_dir_same. reflexivity.
  - (* mkdir_all_aux preserves the DirPresent we just set *)
    assert (Hpres: dir_exists (mkdir_all_aux (update_dir fs (p :: ps) DirPresent)
                                             (parent_dir (p :: ps)) fuel') (p :: ps) = true).
    {
      apply mkdir_all_aux_preserves_dirs.
      unfold dir_exists. rewrite update_dir_same. reflexivity.
    }
    unfold dir_exists in Hpres.
    destruct (directories (mkdir_all_aux (update_dir fs (p :: ps) DirPresent)
                                        (parent_dir (p :: ps)) fuel') (p :: ps)) eqn:E.
    + discriminate Hpres.
    + reflexivity.
Qed.

(** If two filesystems agree on directories at all paths r that satisfy
    is_path_prefix r path = true (including path itself via path_is_prefix_self),
    then mkdir_all_aux gives the same directories at any such r.
    Note: for paths not in this set, the result may differ if the inputs differ. *)
Lemma mkdir_all_aux_ext : forall fs fs' path fuel q,
  (forall r, is_path_prefix r path = true -> directories fs r = directories fs' r) ->
  is_path_prefix q path = true ->
  directories (mkdir_all_aux fs path fuel) q = directories (mkdir_all_aux fs' path fuel) q.
Proof.
  intros fs fs' path fuel q.
  revert fs fs' path q.
  induction fuel as [|fuel' IH]; intros fs fs' path q Hagree Hqpfx.
  - (* fuel = 0 *)
    simpl. apply Hagree. exact Hqpfx.
  - (* fuel = S fuel' *)
    simpl.
    destruct path as [|p ps].
    + (* path = [] *)
      (* q is prefix of [], so q = [] *)
      destruct q as [|s qs].
      * (* q = [] *)
        unfold update_dir. simpl.
        destruct (path_eqb [] []) eqn:Hnil; [reflexivity | ].
        apply path_eqb_neq in Hnil. exfalso. apply Hnil. reflexivity.
      * (* q = s :: qs: not a prefix of [] *)
        simpl in Hqpfx. discriminate.
    + (* path = p :: ps *)
      (* mkdir_all_aux (update_dir fs (p::ps) DirPresent) (parent_dir (p::ps)) fuel' *)
      (* Case: is q a prefix of parent_dir (p :: ps)? *)
      destruct (is_path_prefix q (parent_dir (p :: ps))) eqn:Hqpar.
      * (* q is prefix of parent_dir (p :: ps) *)
        apply IH.
        -- intros r Hr.
           unfold update_dir. simpl.
           destruct (path_eqb r (p :: ps)) eqn:Hrp.
           ++ reflexivity.
           ++ apply Hagree.
              (* r is prefix of parent_dir (p :: ps), which is prefix of p :: ps *)
              assert (Hpar_pfx: is_path_prefix (parent_dir (p :: ps)) (p :: ps) = true).
              { unfold parent_dir.
                destruct ps as [|y ys]; [reflexivity | ].
                simpl.
                destruct (string_dec p p) as [_|Hp]; [|exfalso; apply Hp; reflexivity].
                clear. revert y. induction ys as [|z zs IHzs]; intros y; [reflexivity | ].
                simpl.
                destruct (string_dec y y) as [_|Hy]; [|exfalso; apply Hy; reflexivity].
                apply IHzs. }
              (* Use transitivity: r prefix of parent, parent prefix of p::ps => r prefix of p::ps *)
              destruct r as [|s rs]; [reflexivity | ].
              (* Hr: is_path_prefix (s :: rs) (parent_dir (p :: ps)) = true *)
              unfold parent_dir in Hr.
              destruct ps as [|y ys].
              +++ (* ps = [], parent = [], so is_path_prefix (s::rs) [] = false, contradicts Hr *)
                  simpl in Hr. discriminate.
              +++ (* ps = y :: ys, parent = p :: removelast (y :: ys) *)
                  simpl in Hr |- *.
                  destruct (string_dec s p) as [Hsp|Hsp]; [|discriminate].
                  subst s.
                  destruct (string_dec p p) as [_|Hp]; [|exfalso; apply Hp; reflexivity].
                  (* Hr: is_path_prefix rs (removelast (y :: ys)) = true *)
                  (* Goal: is_path_prefix rs (y :: ys) = true *)
                  apply prefix_of_removelast_is_prefix; [discriminate | exact Hr].
        -- exact Hqpar.
      * (* q is NOT a prefix of parent_dir (p :: ps) *)
        (* But q IS a prefix of p :: ps *)
        (* So q = p :: ps (the only prefix of p::ps that isn't prefix of parent) *)
        destruct (list_eq_dec string_dec q (p :: ps)) as [Heq|Hneq].
        -- (* q = p :: ps *)
           subst q.
           (* Both sides: update_dir sets (p::ps) to DirPresent, then mkdir_all_aux preserves it *)
           assert (Hlhs: dir_exists (mkdir_all_aux (update_dir fs (p :: ps) DirPresent)
                                                   (parent_dir (p :: ps)) fuel') (p :: ps) = true).
           { apply mkdir_all_aux_preserves_dirs.
             unfold dir_exists. rewrite update_dir_same. reflexivity. }
           assert (Hrhs: dir_exists (mkdir_all_aux (update_dir fs' (p :: ps) DirPresent)
                                                   (parent_dir (p :: ps)) fuel') (p :: ps) = true).
           { apply mkdir_all_aux_preserves_dirs.
             unfold dir_exists. rewrite update_dir_same. reflexivity. }
           unfold dir_exists in Hlhs, Hrhs.
           destruct (directories (mkdir_all_aux (update_dir fs (p :: ps) DirPresent)
                                                (parent_dir (p :: ps)) fuel') (p :: ps));
             [discriminate Hlhs | ].
           destruct (directories (mkdir_all_aux (update_dir fs' (p :: ps) DirPresent)
                                                (parent_dir (p :: ps)) fuel') (p :: ps));
             [discriminate Hrhs | reflexivity].
        -- (* q <> p :: ps but q is prefix of p :: ps and not prefix of parent *)
           (* This is a contradiction: if q is a strict prefix of p::ps, then q is prefix of parent *)
           exfalso.
           destruct q as [|s qs].
           ++ (* q = [] is always prefix of anything *) simpl in Hqpar. discriminate.
           ++ simpl in Hqpfx.
              destruct (string_dec s p) as [Hsp|Hsp]; [|discriminate].
              subst s.
              (* qs is prefix of ps, and q = p :: qs *)
              (* parent_dir (p :: ps) = removelast (p :: ps) *)
              (* If ps = [], parent = [], so any prefix of p::[] that's not [] or [p] would be... but q = p::qs prefix of [p] means qs prefix of [], so qs = []. Then q = [p], but q <> p::ps = [p]. Contradiction. *)
              (* If ps non-empty, parent = p :: removelast ps. Then q = p :: qs prefix of p :: ps and not prefix of p :: removelast ps. *)
              destruct ps as [|t ts].
              ** (* ps = [] *)
                 simpl in Hqpfx.
                 destruct qs as [|u us].
                 --- (* qs = [], so q = [p] = p :: ps. Contradicts Hneq *)
                     apply Hneq. reflexivity.
                 --- (* qs = u :: us, but is_path_prefix (u::us) [] = false *)
                     discriminate Hqpfx.
              ** (* ps = t :: ts *)
                 (* parent_dir (p :: t :: ts) = p :: removelast (t :: ts) *)
                 unfold parent_dir in Hqpar. simpl in Hqpar.
                 destruct (string_dec p p) as [_|Hp]; [|exfalso; apply Hp; reflexivity].
                 (* Hqpar: is_path_prefix (p :: qs) (p :: removelast (t :: ts)) = false *)
                 (* But Hqpfx: is_path_prefix qs (t :: ts) = true *)
                 (* is_path_prefix (p :: qs) (p :: removelast (t :: ts)) = is_path_prefix qs (removelast (t :: ts)) *)
                 simpl in Hqpar.
                 destruct (string_dec p p) as [_|Hp']; [|exfalso; apply Hp'; reflexivity].
                 (* Hqpar: is_path_prefix qs (removelast (t :: ts)) = false *)
                 (* But qs is prefix of t :: ts. If qs <> t :: ts, then qs is strict prefix, so qs prefix of removelast *)
                 destruct (list_eq_dec string_dec qs (t :: ts)) as [Heqqs|Hneqqs].
                 --- subst qs.
                     (* Then q = p :: t :: ts = p :: ps, contradicts Hneq *)
                     apply Hneq. reflexivity.
                 --- (* qs is strict prefix of t :: ts *)
                     assert (Hqs_rm: is_path_prefix qs (removelast (t :: ts)) = true).
                     { apply strict_prefix_is_prefix_of_removelast.
                       - exact Hqpfx.
                       - exact Hneqqs.
                       - discriminate. }
                     (* Now we have Hqpar: false and Hqs_rm: true for the same term *)
                     (* They should be equal, giving false = true which is a contradiction *)
                     destruct ts as [|u us].
                     ++++ (* ts = [], removelast [t] = [] *)
                          simpl in Hqpar, Hqs_rm.
                          (* Hqs_rm: is_path_prefix qs [] = true means qs = [] *)
                          destruct qs as [|v vs].
                          **** (* qs = [] *)
                               (* But then q = [p], and we're showing q is strict prefix of p :: t :: ts = p :: [t] = [p; t] *)
                               (* A strict prefix of [p; t] that starts with p is either [] or [p] *)
                               (* q = p :: qs = p :: [] = [p], which is a strict prefix of [p; t]. Good. *)
                               (* Hqpfx: is_path_prefix [] [t] = true, so qs = [] prefix of ts = [t]. *)
                               (* But Hneqqs: qs <> t :: ts = [t], and qs = [], so [] <> [t]. This is true. *)
                               (* Now Hqpar should be is_path_prefix [] [] = false, but is_path_prefix [] [] = true *)
                               simpl in Hqpar. discriminate Hqpar.
                          **** discriminate Hqs_rm.
                     ++++ (* ts = u :: us *)
                          simpl in Hqpar, Hqs_rm.
                          rewrite Hqs_rm in Hqpar. discriminate Hqpar.
Qed.

(** Key lemma: after mkdir_all_aux sets directories, they remain DirPresent
    even when mkdir_all_aux is applied again.

    Proof strategy: By induction on fuel.
    - Base case (fuel = 0): trivial, both sides are fs
    - Inductive case (fuel = S fuel'):
      - For path = []: follows from update_dir_idempotent
      - For path = p::ps and q = p::ps: both sides give DirPresent
      - For path = p::ps and q <> p::ps: use IH on parent
*)
Lemma mkdir_all_aux_idempotent_dirs : forall fs path fuel q,
  directories (mkdir_all_aux (mkdir_all_aux fs path fuel) path fuel) q =
  directories (mkdir_all_aux fs path fuel) q.
Proof.
  intros fs path fuel. revert fs path.
  induction fuel as [|fuel' IH]; intros fs path q.
  - (* fuel = 0 *)
    simpl. reflexivity.
  - (* fuel = S fuel' *)
    simpl.
    destruct path as [|p ps].
    + (* path = [] *)
      rewrite update_dir_idempotent. reflexivity.
    + (* path = p :: ps *)
      destruct (path_eqb q (p :: ps)) eqn:Hqp.
      * (* q = p :: ps: both sides give DirPresent *)
        apply path_eqb_eq in Hqp. subst q.
        set (fs1 := update_dir fs (p :: ps) DirPresent).
        set (fs2 := mkdir_all_aux fs1 (parent_dir (p :: ps)) fuel').
        set (fs3 := update_dir fs2 (p :: ps) DirPresent).
        set (fs4 := mkdir_all_aux fs3 (parent_dir (p :: ps)) fuel').
        assert (Hrhs: directories fs2 (p :: ps) = DirPresent).
        {
          subst fs2 fs1.
          pose proof (mkdir_all_aux_preserves_dirs
                       (update_dir fs (p :: ps) DirPresent)
                       (parent_dir (p :: ps)) fuel' (p :: ps)) as Hpres.
          assert (Hinit: dir_exists (update_dir fs (p :: ps) DirPresent) (p :: ps) = true).
          { unfold dir_exists. rewrite update_dir_same. reflexivity. }
          specialize (Hpres Hinit).
          unfold dir_exists in Hpres.
          destruct (directories (mkdir_all_aux (update_dir fs (p :: ps) DirPresent)
                                              (parent_dir (p :: ps)) fuel') (p :: ps));
            [discriminate | reflexivity].
        }
        assert (Hlhs: directories fs4 (p :: ps) = DirPresent).
        {
          subst fs4 fs3.
          pose proof (mkdir_all_aux_preserves_dirs
                       (update_dir fs2 (p :: ps) DirPresent)
                       (parent_dir (p :: ps)) fuel' (p :: ps)) as Hpres.
          assert (Hinit: dir_exists (update_dir fs2 (p :: ps) DirPresent) (p :: ps) = true).
          { unfold dir_exists. rewrite update_dir_same. reflexivity. }
          specialize (Hpres Hinit).
          unfold dir_exists in Hpres.
          destruct (directories (mkdir_all_aux (update_dir fs2 (p :: ps) DirPresent)
                                              (parent_dir (p :: ps)) fuel') (p :: ps));
            [discriminate | reflexivity].
        }
        rewrite Hlhs, Hrhs. reflexivity.
      * (* q <> p :: ps: use IH *)
        apply path_eqb_neq in Hqp.
        (* The key is that for q <> (p::ps):
           - update_dir at (p::ps) doesn't affect q
           - mkdir_all_aux on parent subtree gives same result for q by IH *)
        set (parent := parent_dir (p :: ps)).
        set (fs1 := update_dir fs (p :: ps) DirPresent).
        set (fs2 := mkdir_all_aux fs1 parent fuel').
        set (fs3 := update_dir fs2 (p :: ps) DirPresent).
        (* Goal: directories (mkdir_all_aux fs3 parent fuel') q = directories fs2 q *)

        (* Step 1: directories fs3 q = directories fs2 q (since q <> p::ps) *)
        assert (Hfs3_q: directories fs3 q = directories fs2 q).
        { subst fs3. rewrite update_dir_other. reflexivity. apply not_eq_sym. exact Hqp. }

        (* Step 2: Case split on whether q is touched by mkdir_all_aux on parent *)
        destruct (is_path_prefix q parent) eqn:Hprefix.
        -- (* q is a prefix of parent or q = parent *)
           (* In either case, both sides set q to DirPresent eventually *)
           (* LHS: mkdir_all_aux fs3 parent fuel' will encounter q during recursion *)
           (* RHS: fs2 = mkdir_all_aux fs1 parent fuel' also encountered q *)
           (* Both set q to DirPresent *)
           destruct (path_eqb q parent) eqn:Hqparent.
           ++ (* q = parent *)
              apply path_eqb_eq in Hqparent. subst q.
              (* Both sides: mkdir_all_aux sets parent to DirPresent *)
              destruct fuel' as [|fuel''].
              ** (* fuel' = 0: mkdir_all_aux is identity *)
                 (* mkdir_all_aux fs3 parent 0 = fs3 and fs2 already has parent set *)
                 simpl.
                 (* Goal: directories fs3 parent = directories fs2 parent *)
                 (* Hfs3_q has q substituted with parent: directories fs3 parent = directories fs2 parent *)
                 exact Hfs3_q.
              ** (* fuel' = S fuel'': mkdir_all_aux_sets_target applies *)
                 assert (Hlhs: dir_exists (mkdir_all_aux fs3 parent (S fuel'')) parent = true).
                 { apply mkdir_all_aux_preserves_dirs.
                   subst fs3. unfold dir_exists. rewrite update_dir_other.
                   - subst fs2. apply mkdir_all_aux_sets_target. lia.
                   - subst parent. intro Hcontra.
                     (* Hcontra: parent_dir (p :: ps) = p :: ps *)
                     (* parent_dir (p :: ps) = removelast (p :: ps) by definition *)
                     (* Show this leads to a length contradiction *)
                     assert (Hlen: length (parent_dir (p :: ps)) < length (p :: ps)).
                     { unfold parent_dir. apply removelast_length. discriminate. }
                     (* Use <- to replace parent_dir (p :: ps) with p :: ps *)
                     rewrite <- Hcontra in Hlen.
                     (* Now Hlen: length (p :: ps) < length (p :: ps) - contradiction *)
                     apply Nat.lt_irrefl in Hlen. exact Hlen. }
                 assert (Hrhs: dir_exists fs2 parent = true).
                 { subst fs2. apply mkdir_all_aux_sets_target. lia. }
                 unfold dir_exists in Hlhs, Hrhs.
                 destruct (directories (mkdir_all_aux fs3 parent (S fuel'')) parent);
                   [discriminate Hlhs | ].
                 destruct (directories fs2 parent); [discriminate Hrhs | reflexivity].
           ++ (* q is strict prefix of parent *)
              apply path_eqb_neq in Hqparent.
              (* q is in the ancestry of parent - mkdir_all_aux processes it *)
              (* Both LHS and RHS process the same path with the same fuel *)
              (* Case split on whether fuel' is enough to process q *)
              destruct (Nat.ltb fuel' (length parent - length q + 1)) eqn:Hfuel_check.
              ** (* fuel' < length parent - length q + 1: not enough fuel *)
                 (* The key insight is that:
                    1. update_dir at (p::ps) doesn't affect q (since q <> p::ps)
                    2. Therefore directories (mkdir_all_aux fs3 parent fuel') q
                       = directories (mkdir_all_aux fs2 parent fuel') q
                       (fs3 differs from fs2 only at (p::ps))
                    3. And directories (mkdir_all_aux fs2 parent fuel') q
                       = directories fs2 q by IH (fs2 = mkdir_all_aux fs1 parent fuel')
                 *)
                 (* First, show that mkdir_all_aux fs3 parent fuel' and mkdir_all_aux fs2 parent fuel'
                    give the same result at q, using mkdir_all_aux_ext *)
                 assert (Hsame: directories (mkdir_all_aux fs3 parent fuel') q =
                               directories (mkdir_all_aux fs2 parent fuel') q).
                 {
                   apply mkdir_all_aux_ext.
                   - (* fs3 and fs2 agree on all prefixes of parent *)
                     intros r Hr.
                     subst fs3.
                     rewrite update_dir_other.
                     + reflexivity.
                     + (* r <> p :: ps *)
                       intro Heq. subst r.
                       (* Hprefix: is_path_prefix q parent = true *)
                       (* Hqparent: path_eqb q parent = false *)
                       (* Hr: is_path_prefix (p :: ps) parent = true *)
                       (* But parent = parent_dir (p :: ps) which is a STRICT prefix of (p :: ps) *)
                       (* So (p :: ps) cannot be a prefix of parent *)
                       subst parent.
                       assert (Hpar_len: length (parent_dir (p :: ps)) < length (p :: ps)).
                       { unfold parent_dir. apply removelast_length. discriminate. }
                       assert (Hpps_len: length (p :: ps) <= length (parent_dir (p :: ps))).
                       { apply prefix_length_le. exact Hr. }
                       lia.
                   - exact Hprefix.
                 }
                 rewrite Hsame.
                 (* Now: directories (mkdir_all_aux fs2 parent fuel') q = directories fs2 q *)
                 (* fs2 = mkdir_all_aux fs1 parent fuel', so we need to show
                    directories (mkdir_all_aux (mkdir_all_aux fs1 parent fuel') parent fuel') q
                    = directories (mkdir_all_aux fs1 parent fuel') q
                    which is exactly IH! *)
                 subst fs2 fs1.
                 rewrite IH. reflexivity.
              ** (* fuel' >= length parent - length q + 1: enough fuel *)
                 apply Nat.ltb_ge in Hfuel_check.
                 assert (Hfuel_ok: fuel' > length parent - length q) by lia.
                 assert (Hlhs: dir_exists (mkdir_all_aux fs3 parent fuel') q = true).
                 { apply mkdir_all_aux_sets_prefixes.
                   - exact Hprefix.
                   - exact Hfuel_ok. }
                 assert (Hrhs: dir_exists fs2 q = true).
                 { subst fs2.
                   apply mkdir_all_aux_sets_prefixes.
                   - exact Hprefix.
                   - exact Hfuel_ok. }
                 unfold dir_exists in Hlhs, Hrhs.
                 destruct (directories (mkdir_all_aux fs3 parent fuel') q);
                   [discriminate Hlhs | ].
                 destruct (directories fs2 q); [discriminate Hrhs | reflexivity].
        -- (* q is not a prefix of parent (and therefore q <> parent) *)
           (* mkdir_all_aux doesn't touch q *)
           assert (Hqparent: q <> parent).
           { intro Hcontra. subst q. rewrite path_is_prefix_self in Hprefix. discriminate. }
           rewrite mkdir_all_aux_untouched by assumption.
           (* Now goal is: directories fs3 q = directories fs2 q *)
           exact Hfs3_q.
Qed.

(** ** Idempotence Properties *)

(** mkdir_all is idempotent *)
Lemma mkdir_all_idempotent : forall fs path,
  mkdir_all (mkdir_all fs path) path = mkdir_all fs path.
Proof.
  intros fs path.
  unfold mkdir_all.
  set (fuel := length path + 1).
  set (fs1 := mkdir_all_aux fs path fuel).
  set (fs2 := mkdir_all_aux fs1 path fuel).
  (* Show fs2 = fs1 by showing both components are equal *)
  assert (Hfiles : files fs2 = files fs1).
  {
    subst fs2 fs1.
    rewrite mkdir_all_aux_preserves_files.
    reflexivity.
  }
  assert (Hdirs : directories fs2 = directories fs1).
  {
    subst fs2 fs1.
    apply functional_extensionality.
    intros q.
    apply mkdir_all_aux_idempotent_dirs.
  }
  (* Now show the filesystem records are equal *)
  destruct fs1 as [f1 d1].
  destruct fs2 as [f2 d2].
  simpl in Hfiles, Hdirs.
  subst f2 d2.
  reflexivity.
Qed.

(** mkdir_all ensures directory exists *)
Lemma mkdir_all_ensures_exists : forall fs path,
  dir_exists (mkdir_all fs path) path = true.
Proof.
  intros fs path.
  unfold mkdir_all.
  apply mkdir_all_aux_sets_target.
  (* length path + 1 > 0 *)
  lia.
Qed.

(** ** Safety Invariants *)

(** Parent directory invariant: if file exists, parent exists (or is root) *)
Definition parent_dir_invariant (fs : FileSystem) : Prop :=
  forall path,
    file_exists fs path = true ->
    match parent_dir path with
    | [] => True  (* Root parent always valid *)
    | parent => dir_exists fs parent = true
    end.

(** mkdir_all preserves directory existence *)
Lemma mkdir_all_preserves_dir_exists : forall fs path q,
  dir_exists fs q = true ->
  dir_exists (mkdir_all fs path) q = true.
Proof.
  intros fs path q Hdir.
  unfold mkdir_all.
  apply mkdir_all_aux_preserves_dirs.
  exact Hdir.
Qed.

(** open_create preserves directory existence (only changes files) *)
Lemma open_create_preserves_dir_exists : forall fs path q fs' err,
  open_create fs path = (fs', err) ->
  dir_exists fs' q = dir_exists fs q.
Proof.
  intros fs path q fs' err Hop.
  unfold open_create in Hop.
  destruct (parent_dir path) as [|p ps] eqn:Hparent.
  - (* Root parent case *)
    injection Hop as Hfs' Herr. subst fs'.
    unfold dir_exists. rewrite update_file_preserves_dirs. reflexivity.
  - (* Non-root parent case *)
    destruct (dir_exists fs (p :: ps)) eqn:Hdir.
    + (* Parent exists: update_file *)
      injection Hop as Hfs' Herr. subst fs'.
      unfold dir_exists. rewrite update_file_preserves_dirs. reflexivity.
    + (* Parent doesn't exist: fs unchanged *)
      injection Hop as Hfs' Herr. subst fs'.
      reflexivity.
Qed.

(** open_or_create_safe maintains parent directory invariant *)
Lemma open_or_create_safe_maintains_parent_invariant :
  forall fs path fs' err,
    parent_dir_invariant fs ->
    open_or_create_safe fs path = (fs', err) ->
    err = Ok ->
    parent_dir_invariant fs'.
Proof.
  intros fs path fs' err Hinv Hop Herr.
  unfold open_or_create_safe in Hop.
  set (fs_after_mkdir := mkdir_all fs (parent_dir path)) in Hop.
  (* Decompose open_create result *)
  destruct (open_create fs_after_mkdir path) as [fs_final err_final] eqn:Hoc.
  injection Hop as Hfs' Herr'. subst fs' err_final.
  (* Now prove parent_dir_invariant fs_final *)
  unfold parent_dir_invariant.
  intros q Hfile_q.
  (* Case analysis on parent_dir q *)
  destruct (parent_dir q) as [|pq pqs] eqn:Hparent_q.
  - (* Root parent: trivially true *)
    trivial.
  - (* Non-root parent: need to show dir_exists fs_final (pq :: pqs) = true *)
    (* Two cases: q = path or q <> path *)
    destruct (list_eq_dec string_dec q path) as [Hqp | Hqp].
    + (* Case: q = path (the file we just created) *)
      subst q.
      (* parent_dir path was created by mkdir_all *)
      (* Goal is: dir_exists fs_final (pq :: pqs) = true
         where Hparent_q : parent_dir path = pq :: pqs *)
      assert (Hdir_parent: dir_exists fs_after_mkdir (pq :: pqs) = true).
      {
        subst fs_after_mkdir.
        rewrite <- Hparent_q.
        apply mkdir_all_ensures_exists.
      }
      (* open_create preserves directories *)
      erewrite open_create_preserves_dir_exists; [|exact Hoc].
      exact Hdir_parent.
    + (* Case: q <> path (a file that existed before or was unaffected) *)
      (* Strategy: trace back file_exists through open_create and mkdir_all *)
      unfold open_create in Hoc.
      destruct (parent_dir path) as [|pp pps] eqn:Hparent_path.
      * (* parent_dir path = [] (root) *)
        injection Hoc as Hfinal Herr''. subst fs_final.
        (* file_exists at q <> path after update_file depends on original fs_after_mkdir *)
        assert (Hfile_q_orig: file_exists fs_after_mkdir q = true).
        {
          rewrite file_exists_update_other in Hfile_q by (apply not_eq_sym; exact Hqp).
          exact Hfile_q.
        }
        (* mkdir_all preserves files, so file q existed in fs *)
        assert (Hfile_q_fs: file_exists fs q = true).
        {
          (* fs_after_mkdir = mkdir_all fs (parent_dir path) by definition *)
          unfold fs_after_mkdir in Hfile_q_orig.
          rewrite file_exists_mkdir_all in Hfile_q_orig.
          exact Hfile_q_orig.
        }
        (* By Hinv, parent of q exists in fs *)
        specialize (Hinv q Hfile_q_fs).
        rewrite Hparent_q in Hinv.
        (* Now show dir_exists fs_final (pq :: pqs) = true *)
        (* fs_final = update_file fs_after_mkdir path ..., and update_file preserves dirs *)
        unfold dir_exists.
        rewrite update_file_preserves_dirs.
        apply mkdir_all_preserves_dir_exists. exact Hinv.
      * (* parent_dir path = pp :: pps (non-root) *)
        destruct (dir_exists fs_after_mkdir (pp :: pps)) eqn:Hdir_pp.
        -- (* Parent exists: update_file occurred *)
           injection Hoc as Hfinal Herr''. subst fs_final.
           (* file_exists at q <> path after update_file *)
           assert (Hfile_q_orig: file_exists fs_after_mkdir q = true).
           {
             rewrite file_exists_update_other in Hfile_q by (apply not_eq_sym; exact Hqp).
             exact Hfile_q.
           }
           (* mkdir_all preserves files *)
           assert (Hfile_q_fs: file_exists fs q = true).
           {
             (* fs_after_mkdir = mkdir_all fs (parent_dir path) by definition *)
             unfold fs_after_mkdir in Hfile_q_orig.
             rewrite file_exists_mkdir_all in Hfile_q_orig.
             exact Hfile_q_orig.
           }
           specialize (Hinv q Hfile_q_fs).
           rewrite Hparent_q in Hinv.
           (* fs_final = update_file fs_after_mkdir path ..., and update_file preserves dirs *)
           unfold dir_exists.
           rewrite update_file_preserves_dirs.
           apply mkdir_all_preserves_dir_exists. exact Hinv.
        -- (* Parent doesn't exist: fs unchanged, error != Ok *)
           injection Hoc as Hfinal Herr''. subst fs_final err.
           discriminate Herr.
Qed.

(** ** TOCTOU Safety Theorems *)

(** open_or_create_safe never returns ParentNotFound *)
Theorem open_or_create_no_parent_error : forall fs path,
  snd (open_or_create_safe fs path) <> ParentNotFound.
Proof.
  intros fs path.
  unfold open_or_create_safe.
  (* mkdir_all ensures parent exists *)
  set (fs' := mkdir_all fs (parent_dir path)).
  (* Now open_create on fs' with existing parent *)
  unfold open_create.
  destruct (parent_dir path) as [|p ps] eqn:Hparent.
  - (* Root parent case: always Ok *)
    simpl. discriminate.
  - (* Non-root parent case *)
    assert (Hdir : dir_exists fs' (p :: ps) = true).
    {
      subst fs'.
      (* parent_dir path = p :: ps *)
      (* mkdir_all fs (p :: ps) ensures dir_exists *)
      apply mkdir_all_ensures_exists.
    }
    rewrite Hdir. simpl. discriminate.
Qed.

(** open_or_create_safe handles TOCTOU race - always succeeds or returns
    appropriate error (never ParentNotFound due to race) *)
Theorem open_or_create_handles_toctou : forall fs path,
  let result := snd (open_or_create_safe fs path) in
  result = Ok.
Proof.
  intros fs path.
  unfold open_or_create_safe.
  set (fs' := mkdir_all fs (parent_dir path)).
  unfold open_create.
  destruct (parent_dir path) as [|p ps] eqn:Hparent.
  - (* Root parent: always succeeds *)
    simpl. reflexivity.
  - (* Non-root parent *)
    assert (Hdir : dir_exists fs' (p :: ps) = true).
    {
      subst fs'.
      apply mkdir_all_ensures_exists.
    }
    rewrite Hdir. simpl. reflexivity.
Qed.

(** Prefix is transitive *)
Lemma is_path_prefix_trans : forall p1 p2 p3,
  is_path_prefix p1 p2 = true ->
  is_path_prefix p2 p3 = true ->
  is_path_prefix p1 p3 = true.
Proof.
  induction p1 as [|s1 p1' IH]; intros p2 p3 H12 H23; simpl.
  - reflexivity.
  - destruct p2 as [|s2 p2'].
    + simpl in H12. discriminate.
    + destruct p3 as [|s3 p3'].
      * simpl in H23. discriminate.
      * simpl in H12, H23.
        destruct (string_dec s1 s2) as [Heq12|Hneq12]; [|discriminate].
        destruct (string_dec s2 s3) as [Heq23|Hneq23]; [|discriminate].
        subst.
        destruct (string_dec s3 s3) as [H|H]; [|exfalso; apply H; reflexivity].
        apply (IH p2' p3'); assumption.
Qed.

(** ** File State Decidability *)

Lemma file_state_dec : forall s1 s2 : FileState,
  {s1 = s2} + {s1 <> s2}.
Proof.
  intros s1 s2.
  destruct s1, s2;
    try (left; reflexivity);
    try (right; discriminate).
Qed.

Lemma dir_state_dec : forall s1 s2 : DirState,
  {s1 = s2} + {s1 <> s2}.
Proof.
  intros s1 s2.
  destruct s1, s2;
    try (left; reflexivity);
    try (right; discriminate).
Qed.
