-- wrk script: POST {"args":[key, ...]} to ACTOR_PATH, cycling over KEYS distinct keys.
--   ACTOR_PATH=/_sigx/actor/Tiny/noop KEYS=1000 ARGS='' wrk -s post.lua http://127.0.0.1:5199
local path = os.getenv("ACTOR_PATH") or "/_sigx/actor/Tiny/noop"
local keys = tonumber(os.getenv("KEYS") or "1000")
local extra = os.getenv("ARGS") or ""
local bodies = {}

function init(args)
  for i = 1, keys do
    local k = "k" .. tostring(i)
    if extra ~= "" then
      bodies[i] = '{"args":["' .. k .. '",' .. extra .. ']}'
    else
      bodies[i] = '{"args":["' .. k .. '"]}'
    end
  end
end

local n = 0
function request()
  n = n + 1
  local body = bodies[(n % keys) + 1]
  return wrk.format("POST", path, { ["content-type"] = "application/json" }, body)
end

local bad = 0
function response(status, headers, body)
  if status ~= 200 then
    bad = bad + 1
    if bad <= 3 then
      io.stderr:write("non-200: " .. status .. " " .. body .. "\n")
    end
  end
end
