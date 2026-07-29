use std::path::Path;

use n00n_arbor::Client;

fn main() -> Result<(), n00n_arbor::ArborError> {
    Client::check_binary()?;

    let project = Path::new(".");
    let symbol = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "main".to_string());

    Client::ensure_indexed(project)?;
    let relations = Client::callers(&symbol, project)?;

    for relation in relations {
        let kind = relation.kind.as_deref().unwrap_or("unknown");
        let line = match relation.line {
            Some(line) => format!(":{line}"),
            None => String::new(),
        };
        println!("{} ({} at {}{})", relation.name, kind, relation.path, line);
    }

    Ok(())
}
