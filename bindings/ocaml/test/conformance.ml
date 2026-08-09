(* Uniform facade conformance suite for the OCaml binding.

   Instantiates the family C1-C10 contract for OCaml against a live
   libdictenstein shared library. It needs only libdictenstein and the canonical
   fixture, never a liblevenshtein transducer, so it pins the *producer* ABI in
   isolation.

     C1  identity + kind/capabilities per backend
     C2  idempotent close + free-order independence
     C3  failure raised (+ non-empty message) for INVALID_UTF8 / DOMAIN_MISMATCH
         / IO_ERROR (the facade raises [Failure msg]; the numeric status is not
         carried, so status-code granularity is N/A)
     C4  canonical fixture replay (all four backends)
     C5  CRUD + value + batch + substring; capability-derived assertions
     C6  precomposed/combining/multibyte, byte-domain NUL + invalid UTF-8, u64 0/MAX
     C7  batch sizes 0/1/255/256/257/1000 (put_many)
     C8  CRUD op-script vs a Hashtbl oracle; substring vs a naive oracle
     C9  leak discipline (>=10k cycles, RSS bounded)
     C10 concurrency: independent per-domain dictionaries + readers during a
         writer (OCaml 5 Domain)

   Run (with the cdylib on the loader/linker path):
     LIBRARY_PATH=target/release LD_LIBRARY_PATH=target/release \
       dune exec bindings/ocaml/test/conformance.exe -- bindings/canonical_fixture.json *)

module D = Vinary_tree_libdictenstein
module I = Vinary_tree_interop
module U = Yojson.Safe.Util

let failures = ref 0
let check condition message =
  if not condition then begin
    Printf.eprintf "FAIL: %s\n" message;
    incr failures
  end

(* Capability bits (the LDICT_CAP_ family). *)
let cap_read = 1L
let cap_insert = 2L
let cap_remove = 4L
let cap_clear = 8L
let cap_compact = 16L
let cap_substring = 32L
let cap_checkpoint = 64L
let has caps bit = Int64.logand caps bit <> 0L

(* --------------------------------------------------------------------- *)
(* fixture (C4)                                                          *)
(* --------------------------------------------------------------------- *)

let optval json = match json with `Null -> None | v -> Some (Int64.of_int (U.to_int v))

let fixture_path =
  if Array.length Sys.argv > 1 then Sys.argv.(1)
  else
    let candidates =
      [ "bindings/canonical_fixture.json"; "../canonical_fixture.json";
        "../../canonical_fixture.json"; "../../../bindings/canonical_fixture.json" ]
    in
    (try List.find Sys.file_exists candidates
     with Not_found -> "bindings/canonical_fixture.json")

let root = Yojson.Safe.from_file fixture_path
let entries =
  U.member "entries" root |> U.to_list
  |> List.map (fun e -> (U.to_string (U.member "term" e), optval (U.member "value" e)))
  |> Array.of_list
let size = U.to_int (U.member "size" root)

let assert_fixture_reads dictionary =
  check (D.length dictionary = size) "fixture size";
  List.iter
    (fun case ->
      let term = U.to_string (U.member "term" case) in
      check (D.contains dictionary term = U.to_bool (U.member "expected" case)) ("contains " ^ term))
    (U.to_list (U.member "contains" root));
  List.iter
    (fun case ->
      let term = U.to_string (U.member "term" case) in
      let { D.found; value } = D.get dictionary term in
      check (found = U.to_bool (U.member "found" case)) ("get.found " ^ term);
      check (value = optval (U.member "value" case)) ("get.value " ^ term))
    (U.to_list (U.member "get" root))

(* --------------------------------------------------------------------- *)
(* C1 identity/version                                                   *)
(* --------------------------------------------------------------------- *)

let c1 () =
  check (D.abi_version () = 1) "abi version == 1";
  check (D.api_revision () = 4) "api revision == 4";
  let dawg = D.dynamic_dawg () in
  check (D.kind dawg = 1) "dawg kind";
  let caps = D.capabilities dawg in
  check (has caps cap_insert && has caps cap_remove && has caps cap_clear && has caps cap_compact) "dawg caps";
  check (not (has caps cap_substring) && not (has caps cap_checkpoint)) "dawg lacks substring/checkpoint";
  D.close dawg;
  let dat = D.double_array_trie [| ("x", None) |] in
  check (D.kind dat = 2) "dat kind";
  check (has (D.capabilities dat) cap_read) "dat read";
  D.close dat;
  let scdawg = D.scdawg () in
  check (D.kind scdawg = 3) "scdawg kind";
  check (has (D.capabilities scdawg) cap_substring) "scdawg substring";
  D.close scdawg

(* --------------------------------------------------------------------- *)
(* C2 lifecycle/ownership                                                *)
(* --------------------------------------------------------------------- *)

let c2 () =
  let dawg = D.dynamic_dawg () in
  ignore (D.put dawg "a" None);
  D.close dawg;
  D.close dawg; (* idempotent *)
  let dawgs = Array.init 4 (fun i -> let d = D.dynamic_dawg () in
                             ignore (D.put d (Printf.sprintf "term%d" i) (Some (Int64.of_int i))); d) in
  List.iter (fun i -> D.close dawgs.(i)) [ 2; 0; 3; 1 ]

(* --------------------------------------------------------------------- *)
(* C3 error mapping (facade raises Failure with a message)              *)
(* --------------------------------------------------------------------- *)

let raises_with_message thunk =
  try thunk (); (false, "") with
  | Failure message -> (true, message)
  | _ -> (true, "?")

let c3 () =
  let dawg = D.dynamic_dawg ~domain:I.Unicode_scalar () in
  let raised, message = raises_with_message (fun () -> ignore (D.put dawg "\xff" None)) in
  check (raised && String.length message > 0) "invalid utf8 raises with message";
  let raised, _ = raises_with_message (fun () -> ignore (D.put_u64 dawg [| 1L; 2L |] None)) in
  check raised "domain mismatch raises";
  D.close dawg;
  let path = Filename.temp_file "ldict-ocaml" ".part" in
  Sys.remove path;
  let raised, message = raises_with_message (fun () -> ignore (D.open_persistent_artrie path)) in
  check (raised && String.length message > 0) "io error raises with message"

(* --------------------------------------------------------------------- *)
(* C4 canonical fixture replay                                           *)
(* --------------------------------------------------------------------- *)

let c4 () =
  let dawg = D.dynamic_dawg () in
  check (D.put_many dawg entries = size) "dawg batch count";
  assert_fixture_reads dawg;
  D.close dawg;
  let dat = D.double_array_trie entries in
  assert_fixture_reads dat;
  D.close dat;
  let path = Filename.temp_file "ldict-ocaml-c4" ".part" in
  Sys.remove path;
  let art = D.create_persistent_artrie path in
  Array.iter (fun (term, value) -> ignore (D.put art term value)) entries;
  assert_fixture_reads art;
  D.close art;
  List.iter (fun suffix -> try Sys.remove (path ^ suffix) with _ -> ()) [ ""; ".wal"; ".wlock" ];
  let scdawg = D.scdawg () in
  Array.iter (fun (term, value) -> ignore (D.put scdawg term value)) entries;
  List.iter
    (fun case ->
      let pattern = U.to_string (U.member "pattern" case) in
      check (D.substring_frequency scdawg pattern = U.to_int (U.member "expected" case)) ("frequency " ^ pattern))
    (U.to_list (U.member "substring_frequency" root));
  List.iter
    (fun case ->
      let pattern = U.to_string (U.member "pattern" case) in
      check (D.contains_substring scdawg pattern = U.to_bool (U.member "expected" case)) ("contains_substring " ^ pattern))
    (U.to_list (U.member "substring_contains" root));
  D.close scdawg

(* --------------------------------------------------------------------- *)
(* C5 CRUD + value + batch + substring; capability-derived assertions    *)
(* --------------------------------------------------------------------- *)

let c5 () =
  let dawg = D.dynamic_dawg () in
  check (D.put dawg "cat" (Some 1L)) "insert cat";
  check (not (D.put dawg "cat" (Some 1L))) "idempotent insert";
  check ((D.get dawg "cat").value = Some 1L) "get cat";
  check (D.remove dawg "cat") "remove cat";
  check (not (D.remove dawg "cat")) "second remove";
  check (not (D.contains dawg "cat")) "cat gone";
  for i = 0 to 49 do ignore (D.put dawg (Printf.sprintf "t%d" i) (Some (Int64.of_int i))) done;
  for i = 0 to 49 do if i mod 2 = 0 then check (D.remove dawg (Printf.sprintf "t%d" i)) "remove even" done;
  ignore (D.compact dawg);
  check (D.length dawg = 25) "compact size";
  check ((D.get dawg "t1").value = Some 1L) "t1 survives";
  check (not (D.contains dawg "t0")) "t0 gone";
  D.close dawg;
  let scdawg = D.scdawg () in
  ignore (D.put scdawg "cat" (Some 1L));
  ignore (D.put scdawg "cot" (Some 2L));
  check (D.substring_frequency scdawg "t" = 2) "freq t == 2";
  check (D.put scdawg "cut" None) "insert cut";
  check (D.substring_frequency scdawg "t" = 3) "freq t == 3";
  D.close scdawg;
  let dat = D.double_array_trie [| ("x", None) |] in
  let caps = D.capabilities dat in
  check (not (has caps cap_insert) && not (has caps cap_remove)
         && not (has caps cap_clear) && not (has caps cap_compact)) "dat capability-derived reject";
  D.close dat

(* --------------------------------------------------------------------- *)
(* C6 text domains and values                                            *)
(* --------------------------------------------------------------------- *)

let c6 () =
  let dawg = D.dynamic_dawg () in
  check (D.put dawg "caf\xc3\xa9" (Some 7L)) "precomposed insert";     (* café, precomposed U+00E9 *)
  check (D.put dawg "\xf0\x9f\xa6\x80" (Some 255L)) "emoji insert";    (* crab, 4-byte scalar *)
  check (D.contains dawg "caf\xc3\xa9") "precomposed contains";
  check ((D.get dawg "\xf0\x9f\xa6\x80").value = Some 255L) "emoji value";
  D.close dawg;
  let dawg = D.dynamic_dawg () in
  let precomposed = "caf\xc3\xa9" and combining = "cafe\xcc\x81" in
  check (D.put dawg precomposed (Some 1L)) "precomposed distinct";
  check (D.put dawg combining (Some 2L)) "combining distinct";
  check (D.length dawg = 2) "distinct scalar sequences";
  check ((D.get dawg precomposed).value = Some 1L) "precomposed value";
  check ((D.get dawg combining).value = Some 2L) "combining value";
  D.close dawg;
  let dawg = D.dynamic_dawg ~domain:I.Byte () in
  check (D.put dawg "a\x00b" (Some 1L)) "embedded NUL insert";
  check (D.put dawg "\xff\xfe" (Some 2L)) "invalid utf8 byte insert";
  check (D.contains dawg "a\x00b") "embedded NUL contains";
  check ((D.get dawg "\xff\xfe").value = Some 2L) "invalid utf8 byte value";
  D.close dawg;
  let dawg = D.dynamic_dawg ~domain:I.U64 () in
  check (D.put_u64 dawg [| 1L; 2L; 3L |] (Some 0L)) "u64 value 0 insert";
  check (D.put_u64 dawg [| 9L |] (Some Int64.minus_one)) "u64 value MAX insert"; (* -1L = 2^64-1 in u64 bits *)
  check ((D.get_u64 dawg [| 1L; 2L; 3L |]).value = Some 0L) "u64 value 0";
  check ((D.get_u64 dawg [| 9L |]).value = Some Int64.minus_one) "u64 value MAX";
  D.close dawg

(* --------------------------------------------------------------------- *)
(* C7 batch / paging edges                                               *)
(* --------------------------------------------------------------------- *)

let c7 () =
  List.iter
    (fun batch_size ->
      let dawg = D.dynamic_dawg () in
      let batch = Array.init batch_size (fun i -> (Printf.sprintf "t%d" i, Some (Int64.of_int i))) in
      check (D.put_many dawg batch = batch_size) (Printf.sprintf "batch %d count" batch_size);
      check (D.length dawg = batch_size) (Printf.sprintf "batch %d size" batch_size);
      if batch_size > 0 then begin
        check ((D.get dawg "t0").value = Some 0L) (Printf.sprintf "batch %d first" batch_size);
        check ((D.get dawg (Printf.sprintf "t%d" (batch_size - 1))).value = Some (Int64.of_int (batch_size - 1)))
          (Printf.sprintf "batch %d last" batch_size)
      end;
      D.close dawg)
    [ 0; 1; 255; 256; 257; 1000 ]

(* --------------------------------------------------------------------- *)
(* C8 property-based testing vs an in-language oracle (deterministic LCG) *)
(* --------------------------------------------------------------------- *)

let make_rng seed =
  let state = ref seed in
  fun n ->
    state := Int64.add (Int64.mul !state 6364136223846793005L) 1442695040888963407L;
    let hi = Int64.shift_right_logical !state 33 in
    Int64.to_int (Int64.rem hi (Int64.of_int n))

let c8_crud () =
  let rng = make_rng 0xC0FFEEL in
  let keys = Array.init 40 (fun i -> Printf.sprintf "k%d" i) in
  let oracle : (string, int64 option) Hashtbl.t = Hashtbl.create 64 in
  let dawg = D.dynamic_dawg () in
  for _ = 1 to 3000 do
    let key = keys.(rng 40) in
    let present = Hashtbl.mem oracle key in
    let op = rng 100 in
    if op < 50 then begin
      let value = if rng 2 = 0 then None else Some (Int64.of_int (rng 1000000000)) in
      check (D.put dawg key value = not present) "crud insert changed";
      Hashtbl.replace oracle key value
    end
    else if op < 75 then begin
      check (D.remove dawg key = present) "crud remove changed";
      Hashtbl.remove oracle key
    end
    else if op < 95 then begin
      check (D.contains dawg key = present) "crud contains";
      if present then
        check ((D.get dawg key).value = Hashtbl.find oracle key) "crud get value"
    end
    else ignore (D.compact dawg);
    check (D.length dawg = Hashtbl.length oracle) "crud size matches oracle"
  done;
  D.close dawg

let c8_substring () =
  let rng = make_rng 0x5CDAL in
  let alphabet = "abcx" in
  let generate max_len =
    let n = rng max_len + 1 in
    String.init n (fun _ -> alphabet.[rng (String.length alphabet)])
  in
  let seen = Hashtbl.create 64 in
  let terms = ref [] in
  while Hashtbl.length seen < 60 do
    let t = generate 6 in
    if not (Hashtbl.mem seen t) then begin Hashtbl.add seen t (); terms := t :: !terms end
  done;
  let terms = !terms in
  let naive pattern =
    List.fold_left
      (fun total term ->
        let count = ref 0 in
        for start = 0 to String.length term - String.length pattern do
          if String.sub term start (String.length pattern) = pattern then incr count
        done;
        total + !count)
      0 terms
  in
  let scdawg = D.scdawg () in
  List.iter (fun term -> ignore (D.put scdawg term None)) terms;
  for _ = 1 to 200 do
    let pattern = generate 3 in
    let expected = naive pattern in
    check (D.substring_frequency scdawg pattern = expected) ("pbt frequency " ^ pattern);
    check (D.contains_substring scdawg pattern = (expected > 0)) ("pbt contains " ^ pattern)
  done;
  D.close scdawg

(* --------------------------------------------------------------------- *)
(* C9 leak discipline                                                    *)
(* --------------------------------------------------------------------- *)

let rss_kib () =
  match (try Some (open_in "/proc/self/status") with _ -> None) with
  | None -> 0
  | Some channel ->
    let result = ref 0 in
    (try
       while true do
         let line = input_line channel in
         if String.length line >= 6 && String.sub line 0 6 = "VmRSS:" then
           (try Scanf.sscanf line "VmRSS: %d" (fun kb -> result := kb) with _ -> ())
       done
     with End_of_file -> ());
    close_in channel;
    !result

let c9 () =
  let cycles = 12000 in
  let batch = [| ("cat", Some 1L); ("cot", Some 2L); ("cut", None) |] in
  for _ = 1 to 2000 do
    let dawg = D.dynamic_dawg () in
    ignore (D.put dawg "cat" (Some 1L));
    D.close dawg
  done;
  Gc.full_major ();
  let before = rss_kib () in
  for _ = 1 to cycles do
    let dawg = D.dynamic_dawg () in
    ignore (D.put_many dawg batch);
    check (D.contains dawg "cot") "leak cycle contains";
    D.close dawg
  done;
  Gc.full_major ();
  let after = rss_kib () in
  if before > 0 && after > before then
    check (after - before < 48 * 1024)
      (Printf.sprintf "RSS grew %d KiB over %d cycles" (after - before) cycles)

(* --------------------------------------------------------------------- *)
(* C10 concurrency (OCaml 5 Domain)                                      *)
(* --------------------------------------------------------------------- *)

let c10_independent () =
  let domains =
    List.init 8 (fun seed ->
      Domain.spawn (fun () ->
        let dawg = D.dynamic_dawg () in
        for i = 0 to 1999 do ignore (D.put dawg (Printf.sprintf "t%d_%d" seed i) (Some (Int64.of_int i))) done;
        let ok = D.length dawg = 2000
                 && (D.get dawg (Printf.sprintf "t%d_1500" seed)).value = Some 1500L in
        D.close dawg;
        ok))
  in
  check (List.for_all Domain.join domains) "independent per-domain dictionaries"

let c10_readers_during_writer () =
  let dawg = D.dynamic_dawg () in
  ignore (D.put_many dawg (Array.init 500 (fun i -> (Printf.sprintf "seed%d" i, Some (Int64.of_int i)))));
  let stop = Atomic.make false in
  let readers =
    List.init 4 (fun _ ->
      Domain.spawn (fun () ->
        let ok = ref true in
        while not (Atomic.get stop) do
          if not (D.contains dawg "seed0") then ok := false;
          ignore (D.get dawg "seed250")
        done;
        !ok))
  in
  for i = 500 to 2999 do ignore (D.put dawg (Printf.sprintf "w%d" i) (Some (Int64.of_int i))) done;
  Atomic.set stop true;
  let all_ok = List.for_all Domain.join readers in
  check all_ok "concurrent readers during writer";
  check ((D.get dawg "w2999").value = Some 2999L) "final write present";
  D.close dawg

(* --------------------------------------------------------------------- *)

let () =
  c1 ();
  c2 ();
  c3 ();
  c4 ();
  c5 ();
  c6 ();
  c7 ();
  c8_crud ();
  c8_substring ();
  c9 ();
  c10_independent ();
  c10_readers_during_writer ();
  if !failures = 0 then print_endline "ocaml conformance: all checks passed"
  else begin
    Printf.eprintf "ocaml conformance: %d check(s) failed\n" !failures;
    exit 1
  end
