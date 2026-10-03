module VinaryTree.Libdictenstein.FSharpUsage.Search

open VinaryTree.Libdictenstein

/// Compile-checked F# use of native ownership and exact key-set algebra.
let containsEither () : bool =
    use left = new DynamicDawg()
    left.Put("cat") |> ignore
    use right = new DynamicDawg()
    right.Put("cot") |> ignore
    use combined = left.Union(right)
    combined.Contains("cat") && combined.Contains("cot")
