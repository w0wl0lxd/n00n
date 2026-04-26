use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use etcetera::base_strategy::BaseStrategy;

const FALLBACK_DIR: &str = ".maki";
const APP_NAME: &str = "maki";

static STRATEGY: OnceLock<Option<Paths>> = OnceLock::new();

struct Paths {
    config: PathBuf,
    data: PathBuf,
    cache: PathBuf,
}

fn resolve() -> Option<&'static Paths> {
    STRATEGY
        .get_or_init(|| {
            let home = etcetera::home_dir().ok()?;
            if home.join(FALLBACK_DIR).is_dir() {
                let fallback = home.join(FALLBACK_DIR);
                return Some(Paths {
                    config: fallback.clone(),
                    data: fallback.clone(),
                    cache: fallback,
                });
            }
            let s = etcetera::choose_base_strategy().ok()?;
            Some(Paths {
                config: s.config_dir().join(APP_NAME),
                data: s.data_dir().join(APP_NAME),
                cache: s.cache_dir().join(APP_NAME),
            })
        })
        .as_ref()
}

fn err() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "cannot determine base directories",
    )
}

fn ensure(path: &Path) -> Result<PathBuf, std::io::Error> {
    fs::create_dir_all(path)?;
    Ok(path.to_path_buf())
}

fn xdg_sibling(data: &Path, name: &str) -> PathBuf {
    data.parent()
        .and_then(|p| p.parent())
        .map(|base| base.join(name).join(APP_NAME))
        .unwrap_or_else(|| data.join(name))
}

pub fn config_dir() -> Result<PathBuf, std::io::Error> {
    let p = resolve().ok_or_else(err)?;
    ensure(&p.config)
}

pub fn data_dir() -> Result<PathBuf, std::io::Error> {
    let p = resolve().ok_or_else(err)?;
    ensure(&p.data)
}

pub fn state_dir() -> Result<PathBuf, std::io::Error> {
    let p = resolve().ok_or_else(err)?;
    if p.config == p.data {
        return ensure(&p.data);
    }
    ensure(&xdg_sibling(&p.data, "state"))
}

pub fn logs_dir() -> Result<PathBuf, std::io::Error> {
    let p = resolve().ok_or_else(err)?;
    if p.config == p.data {
        return ensure(&p.data);
    }
    ensure(&xdg_sibling(&p.data, "logs"))
}

pub fn cache_dir() -> Result<PathBuf, std::io::Error> {
    let p = resolve().ok_or_else(err)?;
    ensure(&p.cache)
}

pub fn home() -> Option<PathBuf> {
    etcetera::home_dir().ok()
}
