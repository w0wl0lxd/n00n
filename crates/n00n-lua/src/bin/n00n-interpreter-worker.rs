use color_eyre::Result;
use n00n_interpreter::worker::run_stdio;

fn main() -> Result<()> {
    color_eyre::install()?;
    run_stdio()?;
    Ok(())
}
