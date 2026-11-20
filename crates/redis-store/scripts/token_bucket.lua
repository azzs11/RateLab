-- Atomic token bucket on Redis time.
--
-- KEYS[1]  bucket hash
-- ARGV[1]  limit (tokens per window)
-- ARGV[2]  window in microseconds
-- ARGV[3]  cost of this request
-- ARGV[4]  key TTL in milliseconds
--
-- Token amounts are scaled by the window, so one microsecond refills `limit` scaled
-- units and every value is an integer. The Rust side guarantees limit * window stays
-- below 2^53, where Lua's doubles are exact.
--
-- Returns {allowed (0 or 1), level after this request (scaled), Redis time in us}.

local limit = tonumber(ARGV[1])
local window = tonumber(ARGV[2])
local cost = tonumber(ARGV[3])
local ttl_ms = tonumber(ARGV[4])

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
    -- Redis time moved backwards (for example after a failover): refill nothing.
    elapsed = 0
end
if elapsed >= window then
    level = capacity
else
    level = math.min(capacity, level + elapsed * limit)
end

local allowed = 0
local need = cost * window
if need <= level then
    level = level - need
    allowed = 1
end

-- %.0f prints the exact integer; plain tostring would round large values.
redis.call('HSET', KEYS[1], 'level', string.format('%.0f', level), 'last', string.format('%.0f', now))
redis.call('PEXPIRE', KEYS[1], ttl_ms)

return {allowed, level, now}
