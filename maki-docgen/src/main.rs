mod gen_config;
mod gen_keybindings;
mod gen_providers;
mod gen_tools;

use std::fs;
use std::path::Path;

const CONTENT_DIR: &str = "site/docs/content";

fn write_page(section: &str, content: &str) {
    let dir = Path::new(CONTENT_DIR).join(section);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("_index.md");
    fs::write(&path, content).unwrap();
    println!("wrote {}", path.display());
}

fn main() {
    write_page("tools", &gen_tools::generate());
    write_page("providers", &gen_providers::generate());
    write_page("configuration", &gen_config::generate());
    write_page("keybindings", &gen_keybindings::generate());
}
