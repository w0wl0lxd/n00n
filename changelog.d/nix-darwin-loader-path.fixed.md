Fix Nix binary wrapping on macOS by using `DYLD_LIBRARY_PATH` instead of `LD_LIBRARY_PATH`, and scope wrapping to the computed package binary path.
