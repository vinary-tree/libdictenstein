(* Public-package collection traversal benchmark. Construction and warmup are
   outside the timed interval; stdout is one host-collection-traversal.v1 JSON object. *)
module D = Vinary_tree_libdictenstein
module I = Vinary_tree_interop

type config = {
  arm : string;
  entries : int;
  passes : int;
  warmup_passes : int;
  batch_size : int;
  early_cancel : int;
}

let parse () =
  let arm = ref "" and entries = ref 65536 and passes = ref 1 in
  let warmup_passes = ref 1 and batch_size = ref 256 and early_cancel = ref 64 in
  let specs = [
    "--arm", Arg.Set_string arm, "materialized, stream, stream-cancel, or reduce";
    "--entries", Arg.Set_int entries, "dictionary entries";
    "--passes", Arg.Set_int passes, "timed passes";
    "--warmup-passes", Arg.Set_int warmup_passes, "untimed passes";
    "--batch-size", Arg.Set_int batch_size, "native entry batch size";
    "--early-cancel", Arg.Set_int early_cancel, "stream cancellation count";
  ] in
  Arg.parse specs (fun value -> raise (Arg.Bad ("unexpected argument " ^ value))) "";
  if not (List.mem !arm [ "materialized"; "stream"; "stream-cancel"; "reduce" ]) then
    raise (Arg.Bad "--arm must be materialized, stream, stream-cancel, or reduce");
  if !entries <= 0 || !passes <= 0 || !batch_size <= 0 || !early_cancel <= 0 then
    raise (Arg.Bad "--entries, --passes, --batch-size, and --early-cancel must be positive");
  if !warmup_passes < 0 || !batch_size > max_int / 38 then
    raise (Arg.Bad "invalid --warmup-passes or --batch-size");
  { arm = !arm; entries = !entries; passes = !passes;
    warmup_passes = !warmup_passes; batch_size = !batch_size;
    early_cancel = !early_cancel }

let make_key index =
  Printf.sprintf "collection/%04x/%08x/shared-suffix" (index land 0xfff) index

let entry_checksum (entry : D.entry) =
  let length = match entry.key with
    | D.Bytes value -> Bytes.length value
    | _ -> failwith "benchmark expected byte-domain entries"
  in
  Int64.logxor (Int64.of_int length) (Option.value entry.value ~default:0L)

let add_checksum left right = Int64.add left right

let () =
  let config = parse () in
  let corpus = Array.init config.entries (fun index -> make_key index, Some (Int64.of_int index)) in
  let ordered = Array.copy corpus in
  Array.sort (fun (left, _) (right, _) -> String.compare left right) ordered;
  let consumed = if config.arm = "stream-cancel"
    then min config.entries config.early_cancel else config.entries in
  let expected = ref 0L in
  for index = 0 to consumed - 1 do
    let key, value = ordered.(index) in
    expected := add_checksum !expected
      (Int64.logxor (Int64.of_int (String.length key)) (Option.get value))
  done;
  let dictionary = D.dynamic_dawg ~domain:I.Byte () in
  Fun.protect ~finally:(fun () -> D.close dictionary) (fun () ->
    if D.put_many dictionary corpus <> config.entries then
      failwith "generated corpus insertion was incomplete";
    let drain () =
      match config.arm with
      | "materialized" ->
          let snapshot = D.with_entries_seq dictionary
            (fun _ entries -> List.of_seq entries) in
          List.fold_left
            (fun (checksum, count) entry ->
              add_checksum checksum (entry_checksum entry), count + 1)
            (0L, 0) snapshot
      | "reduce" ->
          D.fold_entries ~max_entries:config.batch_size
            ~max_units:(config.batch_size * 38) ~max_values:config.batch_size
            dictionary ~init:(0L, 0)
            ~f:(fun (checksum, count) entry ->
              add_checksum checksum (entry_checksum entry), count + 1)
      | "stream" | "stream-cancel" ->
          D.with_entries_seq ~max_entries:config.batch_size
            ~max_units:(config.batch_size * 38) ~max_values:config.batch_size
            dictionary (fun _ entries ->
              let rec loop checksum count sequence =
                if count = consumed then checksum, count else
                match sequence () with
                | Seq.Nil -> checksum, count
                | Seq.Cons (entry, rest) ->
                    loop (add_checksum checksum (entry_checksum entry))
                      (count + 1) rest
              in loop 0L 0 entries)
      | _ -> assert false
    in
    let checked_drain () =
      let checksum, count = drain () in
      if count <> consumed || checksum <> !expected then
        failwith "collection traversal checksum/cardinality mismatch";
      checksum
    in
    for _ = 1 to config.warmup_passes do ignore (checked_drain ()) done;
    let started = Unix.gettimeofday () in
    let checksum = ref 0L in
    for _ = 1 to config.passes do
      checksum := add_checksum !checksum (checked_drain ())
    done;
    let elapsed_ns = max 1L
      (Int64.of_float ((Unix.gettimeofday () -. started) *. 1_000_000_000.)) in
    let batch = if config.arm = "materialized" then "null"
      else string_of_int config.batch_size in
    let early = if config.arm = "stream-cancel" then string_of_int config.early_cancel
      else "null" in
    Printf.printf
      "{\"schema\":\"libdictenstein.host-collection-traversal.v1\",\"runtime\":\"ocaml\",\"arm\":\"%s\",\"dictionary_entries\":%d,\"consumed_entries_per_pass\":%d,\"passes\":%d,\"warmup_passes\":%d,\"batch_size\":%s,\"early_cancel\":%s,\"elapsed_ns\":%Ld,\"checksum\":%s}\n"
      config.arm config.entries consumed config.passes config.warmup_passes
      batch early elapsed_ns (Printf.sprintf "%Lu" !checksum))
