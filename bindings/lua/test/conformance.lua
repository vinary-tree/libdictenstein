-- Uniform facade conformance suite for the Lua binding.
--
-- Instantiates the family C1-C10 contract for Lua against a live libdictenstein
-- shared library. It needs only libdictenstein and the canonical fixture, never
-- a liblevenshtein transducer, so it pins the *producer* ABI in isolation.
--
--   C1  identity + kind/capabilities per backend
--   C2  idempotent close + free-order independence
--   C3  reachable status arms via pcall (INVALID_UTF8, DOMAIN_MISMATCH, IO_ERROR)
--   C4  canonical fixture replay (all four backends)
--   C5  CRUD + value + substring; capability-derived assertions
--   C6  precomposed/combining/multibyte, byte-domain NUL + invalid UTF-8, u64 0/MAX
--   C7  batch sizes 0/1/255/256/257/1000 (double-array-trie construction)
--   C8  CRUD op-script vs a table oracle; substring vs a naive oracle
--   C9  leak discipline (>=10k cycles, RSS bounded)
--   C10 concurrency: N/A (standard Lua has no native threads)
--
-- N/A notes:
--   * Batch insert for DynamicDawg is absent (only DAT construction batches),
--     so C7 exercises the DAT batch path. Full-width u64 values use canonical
--     decimal strings when they exceed lua_Integer's positive range.
--
-- Run (with the module on package.cpath and the cdylib on the loader path):
--   LUA_CPATH="<dir>/?.so" LD_LIBRARY_PATH=target/release \
--     lua bindings/lua/test/conformance.lua bindings/canonical_fixture.json

local m = require("vinary_tree.libdictenstein")

local failures = 0
local function check(condition, message)
  if not condition then
    io.stderr:write("FAIL: " .. tostring(message) .. "\n")
    failures = failures + 1
  end
end

-- Capability bits (LDICT_CAP_*).
local CAP_READ = 1 << 0
local CAP_INSERT = 1 << 1
local CAP_REMOVE = 1 << 2
local CAP_CLEAR = 1 << 3
local CAP_COMPACT = 1 << 4
local CAP_SUBSTRING = 1 << 5
local CAP_CHECKPOINT = 1 << 6

-- ---------------------------------------------------------------------------
-- minimal JSON decoder (objects, arrays, strings, numbers, true/false/null)
-- ---------------------------------------------------------------------------

local NULL = setmetatable({}, { __tostring = function() return "null" end })

local function json_decode(s)
  local pos = 1
  local value

  local function ws()
    while pos <= #s do
      local c = s:sub(pos, pos)
      if c == " " or c == "\n" or c == "\r" or c == "\t" then pos = pos + 1 else break end
    end
  end

  local function parse_string()
    pos = pos + 1 -- opening quote
    local buffer = {}
    while true do
      local c = s:sub(pos, pos)
      if c == '"' then pos = pos + 1; break end
      if c == "\\" then
        pos = pos + 1
        local e = s:sub(pos, pos)
        if e == "n" then buffer[#buffer + 1] = "\n"
        elseif e == "t" then buffer[#buffer + 1] = "\t"
        elseif e == "r" then buffer[#buffer + 1] = "\r"
        elseif e == "u" then
          local code = tonumber(s:sub(pos + 1, pos + 4), 16); pos = pos + 4
          buffer[#buffer + 1] = utf8.char(code)
        else buffer[#buffer + 1] = e end
        pos = pos + 1
      else
        buffer[#buffer + 1] = c; pos = pos + 1
      end
    end
    return table.concat(buffer)
  end

  local function parse_number()
    local start = pos
    while pos <= #s and s:sub(pos, pos):match("[%d%.eE%+%-]") do pos = pos + 1 end
    return tonumber(s:sub(start, pos - 1))
  end

  local function parse_array()
    pos = pos + 1; ws()
    local out = {}
    if s:sub(pos, pos) == "]" then pos = pos + 1; return out end
    while true do
      ws(); out[#out + 1] = value(); ws()
      local c = s:sub(pos, pos); pos = pos + 1
      if c == "]" then break end
    end
    return out
  end

  local function parse_object()
    pos = pos + 1; ws()
    local out = {}
    if s:sub(pos, pos) == "}" then pos = pos + 1; return out end
    while true do
      ws(); local key = parse_string(); ws(); pos = pos + 1 -- ':'
      out[key] = value(); ws()
      local c = s:sub(pos, pos); pos = pos + 1
      if c == "}" then break end
    end
    return out
  end

  value = function()
    ws()
    local c = s:sub(pos, pos)
    if c == "{" then return parse_object()
    elseif c == "[" then return parse_array()
    elseif c == '"' then return parse_string()
    elseif c == "t" then pos = pos + 4; return true
    elseif c == "f" then pos = pos + 5; return false
    elseif c == "n" then pos = pos + 4; return NULL
    else return parse_number() end
  end

  ws()
  return value()
end

local function optval(v) if v == NULL or v == nil then return nil else return v end end
local function value_eq(expected, actual)
  if expected == NULL then return actual == nil else return actual == expected end
end

-- ---------------------------------------------------------------------------
-- fixture
-- ---------------------------------------------------------------------------

local fixture_path = arg[1]
if not fixture_path then
  for _, candidate in ipairs({ "bindings/canonical_fixture.json", "../canonical_fixture.json", "../../canonical_fixture.json" }) do
    local handle = io.open(candidate, "rb")
    if handle then handle:close(); fixture_path = candidate; break end
  end
end
local file = assert(io.open(fixture_path, "rb"), "cannot open canonical fixture")
local fixture = json_decode(file:read("a"))
file:close()

-- ---------------------------------------------------------------------------
-- C1 identity/version
-- ---------------------------------------------------------------------------

check(m.abi_version() == 1, "abi version == 1")
check(m.api_revision() == 6, "api revision == 6")

do
  local dawg <close> = m.dynamic_dawg("unicode")
  check(dawg:kind() == 1, "dawg kind")
  local caps = dawg:capabilities()
  check(caps & CAP_INSERT ~= 0 and caps & CAP_REMOVE ~= 0
    and caps & CAP_CLEAR ~= 0 and caps & CAP_COMPACT ~= 0, "dawg caps")
  check(caps & CAP_SUBSTRING == 0 and caps & CAP_CHECKPOINT == 0, "dawg lacks substring/checkpoint")
  local dat <close> = m.double_array_trie({ { "x" } }, "unicode")
  check(dat:kind() == 2, "dat kind")
  check(dat:capabilities() & CAP_READ ~= 0, "dat read")
  local scdawg <close> = m.scdawg("unicode")
  check(scdawg:kind() == 3, "scdawg kind")
  check(scdawg:capabilities() & CAP_SUBSTRING ~= 0, "scdawg substring")
end

-- ---------------------------------------------------------------------------
-- C2 lifecycle/ownership
-- ---------------------------------------------------------------------------

do
  local dawg = m.dynamic_dawg("unicode")
  dawg:put("a")
  dawg:close()
  dawg:close() -- idempotent
end
do
  local dawgs = {}
  for i = 1, 4 do dawgs[i] = m.dynamic_dawg("unicode"); dawgs[i]:put("term" .. i, i) end
  for _, index in ipairs({ 3, 1, 4, 2 }) do dawgs[index]:close() end
end

-- ---------------------------------------------------------------------------
-- C3 error-mapping matrix via pcall (errors are raised as Lua errors)
--
-- Reachable: INVALID_UTF8 (3), DOMAIN_MISMATCH (9), IO_ERROR (7). N/A:
-- NULL_POINTER (4) is guarded by luaL_argcheck; UNSUPPORTED (6) is
-- capability-derived (C5); LIMIT_EXCEEDED (10) is auto-sized away by :term.
-- ---------------------------------------------------------------------------

do
  local dawg <close> = m.dynamic_dawg("unicode")
  local ok, err = pcall(function() dawg:put("\xff") end)
  check(not ok and tostring(err):find("status 3", 1, true) ~= nil, "invalid utf8 -> status 3")
end
do
  local dawg <close> = m.dynamic_dawg("unicode")
  local ok, err = pcall(function() dawg:put_u64({ 1, 2 }) end)
  check(not ok and tostring(err):find("status 9", 1, true) ~= nil, "domain mismatch -> status 9")
end
do
  local path = os.tmpname() .. "-missing.part"
  os.remove(path)
  local ok, err = pcall(function() m.open_persistent_artrie(path, "unicode") end)
  check(not ok and tostring(err):find("status 7", 1, true) ~= nil, "io error -> status 7")
  check(not ok and #tostring(err) > 0, "io error message non-empty")
end

-- ---------------------------------------------------------------------------
-- C4 canonical fixture replay
-- ---------------------------------------------------------------------------

local function assert_fixture_reads(dictionary)
  check(dictionary:len() == fixture.size, "fixture size")
  for _, item in ipairs(fixture.contains) do
    check(dictionary:contains(item.term) == item.expected, "contains " .. item.term)
  end
  for _, item in ipairs(fixture.get) do
    local result = dictionary:get(item.term)
    check(result.found == item.found, "get.found " .. item.term)
    check(value_eq(item.value, result.value), "get.value " .. item.term)
  end
end

do
  local dawg <close> = m.dynamic_dawg("unicode")
  for _, entry in ipairs(fixture.entries) do dawg:put(entry.term, optval(entry.value)) end
  assert_fixture_reads(dawg)
end
do
  local entries = {}
  for _, entry in ipairs(fixture.entries) do entries[#entries + 1] = { entry.term, optval(entry.value) } end
  local dat <close> = m.double_array_trie(entries, "unicode")
  assert_fixture_reads(dat)
end
do
  local path = os.tmpname() .. "-c4.part"
  os.remove(path)
  local art = m.create_persistent_artrie(path, "unicode")
  for _, entry in ipairs(fixture.entries) do art:put(entry.term, optval(entry.value)) end
  assert_fixture_reads(art)
  art:close()
  os.remove(path); os.remove(path .. ".wal"); os.remove(path .. ".wlock")
end
do
  local scdawg <close> = m.scdawg("unicode")
  for _, entry in ipairs(fixture.entries) do scdawg:put(entry.term, optval(entry.value)) end
  for _, item in ipairs(fixture.substring_frequency) do
    check(scdawg:frequency(item.pattern) == item.expected, "frequency " .. item.pattern)
  end
  for _, item in ipairs(fixture.substring_contains) do
    check(scdawg:contains_substring(item.pattern) == item.expected, "contains_substring " .. item.pattern)
  end
end

-- ---------------------------------------------------------------------------
-- C5 CRUD + value + substring; capability-derived assertions
-- ---------------------------------------------------------------------------

do
  local dawg <close> = m.dynamic_dawg("unicode")
  check(dawg:put("cat", 1) == true, "insert cat")
  check(dawg:put("cat", 1) == false, "idempotent insert")
  check(dawg:get("cat").value == 1, "get cat")
  check(dawg:remove("cat") == true, "remove cat")
  check(dawg:remove("cat") == false, "second remove")
  check(dawg:contains("cat") == false, "cat gone")
end
do
  local dawg <close> = m.dynamic_dawg("unicode")
  for i = 0, 49 do dawg:put("t" .. i, i) end
  for i = 0, 49, 2 do check(dawg:remove("t" .. i) == true, "remove t" .. i) end
  dawg:compact()
  check(dawg:len() == 25, "compact size")
  check(dawg:get("t1").value == 1, "t1 survives")
  check(dawg:contains("t0") == false, "t0 gone")
end
do
  local scdawg <close> = m.scdawg("unicode")
  scdawg:put("cat", 1); scdawg:put("cot", 2)
  check(scdawg:frequency("t") == 2, "freq t == 2")
  check(scdawg:put("cut") == true, "insert cut")
  check(scdawg:frequency("t") == 3, "freq t == 3")
end
do
  local dat <close> = m.double_array_trie({ { "x" } }, "unicode")
  local caps = dat:capabilities()
  check(caps & (CAP_INSERT | CAP_REMOVE | CAP_CLEAR | CAP_COMPACT) == 0, "dat capability-derived reject")
end

-- ---------------------------------------------------------------------------
-- C6 text domains and values
-- ---------------------------------------------------------------------------

do
  local dawg <close> = m.dynamic_dawg("unicode")
  check(dawg:put("caf\xc3\xa9", 7) == true, "precomposed insert")     -- café, precomposed U+00E9
  check(dawg:put("\xf0\x9f\xa6\x80", 255) == true, "emoji insert")    -- 🦀, 4-byte scalar
  check(dawg:contains("caf\xc3\xa9") == true, "precomposed contains")
  check(dawg:get("\xf0\x9f\xa6\x80").value == 255, "emoji value")
end
do
  local dawg <close> = m.dynamic_dawg("unicode")
  local precomposed = "caf\xc3\xa9"  -- café, precomposed U+00E9
  local combining = "cafe\xcc\x81"   -- cafe + U+0301 combining acute
  check(dawg:put(precomposed, 1) == true, "precomposed distinct")
  check(dawg:put(combining, 2) == true, "combining distinct")
  check(dawg:len() == 2, "distinct scalar sequences")
  check(dawg:get(precomposed).value == 1, "precomposed value")
  check(dawg:get(combining).value == 2, "combining value")
end
do
  local dawg <close> = m.dynamic_dawg("byte")
  check(dawg:put("a\x00b", 1) == true, "embedded NUL insert")
  check(dawg:put("\xff\xfe", 2) == true, "invalid utf8 byte insert")
  check(dawg:contains("a\x00b") == true, "embedded NUL contains")
  check(dawg:get("\xff\xfe").value == 2, "invalid utf8 byte value")
end
do
  local dawg <close> = m.dynamic_dawg("u64")
  check(dawg:put_u64({ 1, 2, 3 }, 0) == true, "u64 value 0 insert")
  check(dawg:put_u64({ 9 }, math.maxinteger) == true, "u64 value MAX insert")
  check(dawg:put_u64({ "18446744073709551615" }, "18446744073709551615") == true,
    "u64 full-width decimal insert")
  check(dawg:get_u64({ 1, 2, 3 }).value == 0, "u64 value 0")
  check(dawg:get_u64({ 9 }).value == math.maxinteger, "u64 value math.maxinteger")
  check(dawg:get_u64({ "18446744073709551615" }).value == "18446744073709551615",
    "u64 full-width decimal lookup")
end

-- ---------------------------------------------------------------------------
-- C7 batch / paging edges (double-array-trie construction is the batch path)
-- ---------------------------------------------------------------------------

for _, size in ipairs({ 0, 1, 255, 256, 257, 1000 }) do
  local entries = {}
  for i = 0, size - 1 do entries[#entries + 1] = { "t" .. i, i } end
  local dat <close> = m.double_array_trie(entries, "unicode")
  check(dat:len() == size, "batch " .. size .. " size")
  if size > 0 then
    check(dat:get("t0").value == 0, "batch " .. size .. " first")
    check(dat:get("t" .. (size - 1)).value == size - 1, "batch " .. size .. " last")
  end
end

-- ---------------------------------------------------------------------------
-- C8 property-based testing vs an in-language oracle (deterministic LCG)
-- ---------------------------------------------------------------------------

local function make_rng(seed)
  local state = seed & 0xFFFFFFFFFFFFFFFF
  return function(n) -- integer in [0, n-1]
    state = state * 6364136223846793005 + 1442695040888963407 -- wraps mod 2^64
    return (state >> 33) % n
  end
end

do
  local rng = make_rng(0xC0FFEE)
  local keys = {}
  for i = 0, 39 do keys[i + 1] = "k" .. i end
  local oracle = {}
  local oracle_size = 0
  local dawg <close> = m.dynamic_dawg("unicode")
  for _ = 1, 3000 do
    local key = keys[rng(40) + 1]
    local present = oracle[key] ~= nil
    local op = rng(100)
    if op < 50 then
      local value = (rng(2) == 0) and NULL or rng(1 << 31)
      check(dawg:put(key, optval(value)) == (not present), "crud insert changed")
      if not present then oracle_size = oracle_size + 1 end
      oracle[key] = value
    elseif op < 75 then
      check(dawg:remove(key) == present, "crud remove changed")
      if present then oracle_size = oracle_size - 1 end
      oracle[key] = nil
    elseif op < 95 then
      check(dawg:contains(key) == present, "crud contains")
      if present then check(value_eq(oracle[key], dawg:get(key).value), "crud get value") end
    else
      dawg:compact()
    end
    check(dawg:len() == oracle_size, "crud size matches oracle")
  end
end

do
  local rng = make_rng(0x5CDA)
  local alphabet = "abcx"
  local function generate(max_len)
    local n = rng(max_len) + 1
    local chars = {}
    for i = 1, n do local k = rng(#alphabet) + 1; chars[i] = alphabet:sub(k, k) end
    return table.concat(chars)
  end
  local term_set = {}
  local terms = {}
  while #terms < 60 do
    local t = generate(6)
    if not term_set[t] then term_set[t] = true; terms[#terms + 1] = t end
  end
  local function naive(pattern)
    local total = 0
    for _, term in ipairs(terms) do
      for start = 1, #term - #pattern + 1 do
        if term:sub(start, start + #pattern - 1) == pattern then total = total + 1 end
      end
    end
    return total
  end
  local scdawg <close> = m.scdawg("unicode")
  for _, term in ipairs(terms) do scdawg:put(term) end
  for _ = 1, 200 do
    local pattern = generate(3)
    local expected = naive(pattern)
    check(scdawg:frequency(pattern) == expected, "pbt frequency " .. pattern)
    check(scdawg:contains_substring(pattern) == (expected > 0), "pbt contains " .. pattern)
  end
end

do
  local left <close> = m.dynamic_dawg("unicode")
  local right <close> = m.dynamic_dawg("unicode")
  left:put("a", 1); left:put("shared", 7); left:put("valueless")
  right:put("b", 2); right:put("shared", 11); right:put("valueless", 5)
  local joined <close> = left:union(right, "lattice_join")
  local common <close> = left & right
  local only_left <close> = left - right
  local exclusive <close> = left ~ right
  check(joined:len() == 4, "algebra union")
  check(joined:get("shared").value == 11, "algebra union joined value")
  check(joined:get("valueless").value == 5, "algebra union valueless join")
  check(common:len() == 2, "algebra intersection")
  check(common:get("shared").value == 7, "algebra intersection meet")
  check(common:get("valueless").found and common:get("valueless").value == nil,
    "algebra intersection valueless meet")
  check(only_left:contains("a"), "algebra difference")
  check(exclusive:len() == 2 and exclusive:contains("a") and exclusive:contains("b"),
    "algebra symmetric difference")
  left:put("later", 99)
  check(not joined:contains("later"), "algebra snapshot independence")
  check(joined:put("mutable-result", 23), "algebra result mutable")
end

-- ---------------------------------------------------------------------------
-- entries-v1: native order, immutable capture, all domains, and early cleanup
-- ---------------------------------------------------------------------------

do
  local dawg <close> = m.dynamic_dawg("unicode")
  dawg:put("cat")
  dawg:put("caf\195\169", "18446744073709551615")
  dawg:put("", 0)
  local cursor <close> = dawg:entry_cursor()
  local metadata = cursor:metadata()
  check(metadata.unit_domain == "unicode", "entries unicode domain")
  check(metadata.value_domain == "optional_u64", "entries optional-u64 values")
  check(metadata.exact_length == 3, "entries exact length")
  check(metadata.snapshot_identity ~= nil, "entries snapshot identity")
  dawg:put("dog", 4)
  local captured = {}
  while true do
    local key, value, has_value = cursor:next()
    if key == nil then break end
    captured[#captured + 1] = { key, value, has_value }
  end
  check(#captured == 3, "entries capture one immutable revision")
  check(captured[1][1] == "" and captured[1][2] == 0 and captured[1][3],
    "entries empty Unicode key/value")
  check(captured[2][1] == "caf\195\169" and
      captured[2][2] == "18446744073709551615" and captured[2][3],
    "entries Unicode/full-width value")
  check(captured[3][1] == "cat" and captured[3][2] == nil and not captured[3][3],
    "entries valueless key")

  local seen = 0
  for _key, _value, _has_value in dawg:entries_iter() do
    seen = seen + 1
    break
  end
  check(seen == 1 and dawg:contains("cat"), "entries iterator closes on early break")

  local snapshot = dawg:entries()
  check(snapshot.metadata.exact_length == 4, "materialized entries metadata")
  local ordered = {}
  for key, value, has_value in pairs(snapshot) do
    ordered[#ordered + 1] = { key, value, has_value }
  end
  check(#ordered == 4 and ordered[4][1] == "dog", "materialized entries native order")
end

do
  local dawg <close> = m.dynamic_dawg("byte")
  dawg:put("")
  dawg:put("\000\255", "18446744073709551615")
  dawg:put("\001", 1)
  local snapshot = dawg:entries()
  local ordered = {}
  for key, value, has_value in pairs(snapshot) do
    ordered[#ordered + 1] = { key, value, has_value }
  end
  check(#ordered == 3 and ordered[1][1] == "" and ordered[2][1] == "\000\255"
      and ordered[3][1] == "\001", "entries preserve arbitrary byte order")
  check(ordered[2][2] == "18446744073709551615" and ordered[2][3],
    "entries preserve byte-key full-width value")
end

do
  local dawg <close> = m.dynamic_dawg("u64")
  dawg:put_u64({ "18446744073709551615" }, "18446744073709551615")
  dawg:put_u64({ 0 })
  dawg:put_u64({ "9223372036854775808" }, 0)
  local snapshot = dawg:entries()
  local ordered = {}
  for key, value, has_value in pairs(snapshot) do
    ordered[#ordered + 1] = { key, value, has_value }
  end
  check(#ordered == 3 and ordered[1][1][1] == 0
      and ordered[2][1][1] == "9223372036854775808"
      and ordered[3][1][1] == "18446744073709551615",
    "entries preserve numeric u64 lexicographic order")
  check(ordered[3][2] == "18446744073709551615" and ordered[3][3],
    "entries preserve full-width u64 value")
end

-- ---------------------------------------------------------------------------
-- C9 leak discipline
-- ---------------------------------------------------------------------------

local function rss_kib()
  local handle = io.open("/proc/self/status", "r")
  if not handle then return 0 end
  for line in handle:lines() do
    local value = line:match("^VmRSS:%s+(%d+)")
    if value then handle:close(); return tonumber(value) end
  end
  handle:close()
  return 0
end

do
  local cycles = 12000
  for _ = 1, 2000 do
    local dawg = m.dynamic_dawg("unicode"); dawg:put("cat", 1); dawg:close()
  end
  collectgarbage("collect")
  local before = rss_kib()
  for _ = 1, cycles do
    local dawg = m.dynamic_dawg("unicode")
    dawg:put("cat", 1); dawg:put("cot", 2); dawg:put("cut")
    check(dawg:contains("cot") == true, "leak cycle contains")
    dawg:close()
  end
  collectgarbage("collect")
  local after = rss_kib()
  if before > 0 and after > before then
    check(after - before < 48 * 1024, "RSS grew " .. (after - before) .. " KiB over " .. cycles .. " cycles")
  end
end

-- ---------------------------------------------------------------------------
-- C10 concurrency: N/A (standard Lua has no native threads).
-- ---------------------------------------------------------------------------

if failures == 0 then
  print("lua conformance: all checks passed")
  os.exit(0)
else
  io.stderr:write("lua conformance: " .. failures .. " check(s) failed\n")
  os.exit(1)
end
