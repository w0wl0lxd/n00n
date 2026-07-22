return function(U)
  local instructions = {
    add_instruction = true,
    arg_instruction = true,
    cmd_instruction = true,
    copy_instruction = true,
    entrypoint_instruction = true,
    env_instruction = true,
    expose_instruction = true,
    from_instruction = true,
    healthcheck_instruction = true,
    label_instruction = true,
    maintainer_instruction = true,
    onbuild_instruction = true,
    run_instruction = true,
    shell_instruction = true,
    stopsignal_instruction = true,
    user_instruction = true,
    volume_instruction = true,
    workdir_instruction = true,
  }
  local nodes = {}
  for kind in pairs(instructions) do
    nodes[kind] = { label = "" }
  end
  return require("lang.simple_outline")(U, { header = "instructions", nodes = nodes })
end
