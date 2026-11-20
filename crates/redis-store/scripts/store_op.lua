-- The coordination store protocol on one Redis token bucket (see ratelab::strategies).
--
-- Same layout and arithmetic as token_bucket.lua: amounts are scaled by the window in
-- microseconds, time is Redis TIME, values stay exact below 2^53.
--
-- KEYS[1]  bucket hash
-- ARGV[1]  limit (tokens per window)
-- ARGV[2]  window in microseconds
-- ARGV[3]  operation: "check", "lease", "return" or "sync"
-- ARGV[4]  amount: cost (check), tokens wanted (lease), tokens returned (return),
--          tokens spent locally (sync)
-- ARGV[5]  key TTL in milliseconds
--
-- Returns {result, level after the operation (scaled), Redis time in us}, where result is
--   check:  1 if allowed, 0 if not
--   lease:  tokens granted (0 or more)
--   return: 0
--   sync:   whole tokens left after applying the report

local limit = tonumber(ARGV[1])
local window = tonumber(ARGV[2])
local op = ARGV[3]
local amount = tonumber(ARGV[4])
local ttl_ms = tonumber(ARGV[5])

local t = redis.call('TIME')
local now = tonumber(t[1]) * 1000000 + tonumber(t[2])
local capacity = limit * window

local state = redis.call('HMGET', KEYS[1], 'level', 'last')
local level = tonumber(state[1])
local last = tonumber(state[2])
if level == nil or last == nil then
    level = capacity
    last = now
end
local elapsed = now - last
if elapsed < 0 then
    elapsed = 0
end
if elapsed >= window then
    level = capacity
else
    level = math.min(capacity, level + elapsed * limit)
end

local result = 0
if op == 'check' then
    local need = amount * window
    if need <= level then
        level = level - need
        result = 1
    end
elseif op == 'lease' then
    local available = math.floor(level / window)
    result = math.min(amount, available)
    level = level - result * window
elseif op == 'return' then
    level = math.min(capacity, level + amount * window)
elseif op == 'sync' then
    level = math.max(0, level - amount * window)
    result = math.floor(level / window)
else
    return redis.error_reply('unknown operation ' .. tostring(op))
end

redis.call('HSET', KEYS[1], 'level', string.format('%.0f', level), 'last', string.format('%.0f', now))
redis.call('PEXPIRE', KEYS[1], ttl_ms)

return {result, level, now}
