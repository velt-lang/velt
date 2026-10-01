-- HTTP/1.1 pipelining for wrk, as TechEmpower's plaintext test does:
-- wrk -s pipeline.lua <url> -- <depth> sends <depth> requests back to back per write.
init = function(args)
  local r = {}
  local depth = tonumber(args[1]) or 1
  for i = 1, depth do
    r[i] = wrk.format()
  end
  req = table.concat(r)
end

request = function()
  return req
end
