-- Append-only JSONL telemetry logger for multi-agent runs.
local M = {}

function M.open(log_dir, run_id)
  local ok = pcall(function()
    n00n.fs.mkdir(log_dir, { parents = true })
  end)
  if not ok then
    return nil, "cannot create telemetry directory"
  end
  local path = n00n.fs.joinpath(log_dir, run_id .. ".jsonl")
  local lock = n00n.async.semaphore(1)

  return {
    log = function(event, data)
      local permit = lock:acquire()
      local success, write_err = pcall(function()
        local existing = ""
        local read_ok, text = pcall(n00n.fs.read, path)
        if read_ok and text then
          existing = text
        end
        local line = n00n.json.encode({
          run_id = run_id,
          event = event,
          timestamp = os.time(),
          data = data or {},
        }) .. "\n"
        n00n.fs.write(path, existing .. line)
      end)
      permit:release()
      if not success then
        return nil, tostring(write_err)
      end
      return true
    end,
  }
end

return M
