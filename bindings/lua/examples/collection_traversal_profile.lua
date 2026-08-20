-- Public-package collection traversal benchmark. Construction and warmup are
-- outside the timed region; stdout is one host-collection-traversal.v1 object.
local ld = require("vinary_tree.libdictenstein")

local config = {
  entries = 65536, passes = 1, warmup_passes = 1,
  batch_size = 256, early_cancel = 64
}
local index = 1
while index <= #arg do
  local name, value = arg[index], arg[index + 1]
  if name == "--arm" then config.arm = value
  elseif name == "--entries" then config.entries = tonumber(value)
  elseif name == "--passes" then config.passes = tonumber(value)
  elseif name == "--warmup-passes" then config.warmup_passes = tonumber(value)
  elseif name == "--batch-size" then config.batch_size = tonumber(value)
  elseif name == "--early-cancel" then config.early_cancel = tonumber(value)
  else error("unknown or incomplete argument: " .. tostring(name)) end
  index = index + 2
end
if config.arm ~= "materialized" and config.arm ~= "stream"
    and config.arm ~= "stream-cancel" then
  error("--arm must be materialized, stream, or stream-cancel")
end
for _, name in ipairs({ "entries", "passes", "batch_size", "early_cancel" }) do
  assert(config[name] and config[name] > 0 and config[name] % 1 == 0,
    "--" .. name:gsub("_", "-") .. " must be a positive integer")
end
assert(config.warmup_passes and config.warmup_passes >= 0
  and config.warmup_passes % 1 == 0, "--warmup-passes must be nonnegative")

local corpus = {}
local dictionary <close> = ld.dynamic_dawg("byte")
for item = 0, config.entries - 1 do
  local key = string.format("collection/%04x/%08x/shared-suffix", item & 0xfff, item)
  corpus[#corpus + 1] = { key, item }
  assert(dictionary:put(key, item), "generated key was not unique")
end
table.sort(corpus, function(left, right) return left[1] < right[1] end)

local consumed = config.entries
if config.arm == "stream-cancel" then
  consumed = math.min(config.entries, config.early_cancel)
end
local expected = 0
for item = 1, consumed do
  expected = expected + (#corpus[item][1] ~ corpus[item][2])
end

local limits = {
  max_entries = config.batch_size,
  max_units = config.batch_size * 38,
  max_values = config.batch_size
}
local function drain()
  local checksum, count = 0, 0
  if config.arm == "materialized" then
    for key, value in pairs(dictionary:entries()) do
      checksum = checksum + (#key ~ (value or 0))
      count = count + 1
    end
  else
    for key, value in dictionary:entries_iter(limits) do
      checksum = checksum + (#key ~ (value or 0))
      count = count + 1
      if count == consumed then break end
    end
  end
  assert(count == consumed and checksum == expected,
    "collection traversal checksum/cardinality mismatch")
  return checksum
end

for _ = 1, config.warmup_passes do drain() end
local started = os.clock()
local checksum = 0
for _ = 1, config.passes do checksum = checksum + drain() end
local elapsed_ns = math.max(1, math.floor((os.clock() - started) * 1000000000))

local batch = config.arm == "materialized" and "null" or tostring(config.batch_size)
local early = config.arm == "stream-cancel" and tostring(config.early_cancel) or "null"
io.write(string.format(
  '{"schema":"libdictenstein.host-collection-traversal.v1",' ..
  '"runtime":"lua","arm":"%s","dictionary_entries":%d,' ..
  '"consumed_entries_per_pass":%d,"passes":%d,"warmup_passes":%d,' ..
  '"batch_size":%s,"early_cancel":%s,"elapsed_ns":%d,"checksum":%d}\n',
  config.arm, config.entries, consumed, config.passes, config.warmup_passes,
  batch, early, elapsed_ns, checksum))
