use std::path::Path;

use n00n_arbor::Client;

fn main() -> Result<(), n00n_arbor::ArborError> {
    Client::check_binary()?;

    let project = Path::new(".");
    let status = Client::status(project)?;
    println!("{status}");

    Ok(())
}
