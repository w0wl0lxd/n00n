Treat empty Devin tool argument JSON as an empty object instead of failing to parse, preventing `batch`, `bash`, and other parameterless tool calls from crashing the Devin provider stream.
