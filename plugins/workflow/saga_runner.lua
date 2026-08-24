local M = {}

function M.run(saga, err)
  if not saga then
    return err
  end

  local compensation_failures = {}
  for i = #saga.compensations, 1, -1 do
    local ok, compensation_err = pcall(saga.compensations[i])
    if not ok then
      table.insert(compensation_failures, tostring(compensation_err))
    end
  end
  for _, handler in ipairs(saga.error_handlers) do
    pcall(handler, err)
  end
  if #compensation_failures > 0 then
    return tostring(err) .. "\ncompensation failures: " .. table.concat(compensation_failures, "; ")
  end
  return err
end

return M
