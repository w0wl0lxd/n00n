# n00n-arbor

Rust bindings for the Arbor CLI. Arbor is a graph-based code analysis tool that maps caller and callee relationships, ranks project symbols, and measures the blast radius of a diff.

This crate is used by the `arbor` built-in plugin in `n00n-lua`. Most users interact with Arbor through the plugin, not this crate directly.

## Requirements

Install Arbor first:

```bash
cargo install arbor-graph-cli
```

Then make sure `arbor` is on your `PATH`.

## Usage

```rust
use std::path::Path;
use n00n_arbor::Client;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    Client::check_binary()?;

    let project = Path::new(".");
    let status = Client::status(project)?;
    println!("{}", status);
    Ok(())
}
```

See the `examples/` directory for more commands.

## Supported commands

- `callers(symbol, project)` – symbols that call the given name.
- `callees(symbol, project)` – symbols called by the given name.
- `map(project, token_budget)` – ranked project skeleton.
- `query(text, project)` – free-text search over the project.
- `status(project)` – index status.
- `diff(project)` – blast-radius impact of unpushed changes.
- `ensure_indexed(project)` – make sure the project is indexed.

## Tests

Unit tests cover JSON serialization and deserialization against the Arbor wire format. Running the full integration surface requires a working `arbor` binary.
