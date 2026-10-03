-- Lua 5.4: <close> releases owned native dictionaries deterministically.
local ld = require("vinary_tree.libdictenstein")

local left <close> = ld.dynamic_dawg("unicode")
left:put("cat", 1)
left:put("cut") -- Membership without a mapped value.

local snapshot = left:entries({ max_entries = 2, max_units = 64, max_values = 2 })
left:put("cat", 9)
local original = {}
for term, value, has_value in pairs(snapshot) do
  original[term] = { value = value, has_value = has_value }
end
assert(original.cat.value == 1 and original.cat.has_value)
assert(original.cut.value == nil and not original.cut.has_value)

local visited = {}
for term, value, has_value in left:entries_iter({ max_entries = 1 }) do
  visited[term] = has_value and value or true
end
assert(visited.cat == 9 and visited.cut == true)

local right <close> = ld.dynamic_dawg("unicode")
right:put("cat", 2)
right:put("cot", 3)
local joined <close> = left:union(right, "lattice_join")
assert(joined:get("cat").value == 9)
assert(joined:contains("cot") and joined:contains("cut"))

local exclusive <close> = left ~ right -- Symmetric difference.
assert(exclusive:contains("cut") and exclusive:contains("cot"))
assert(not exclusive:contains("cat"))
