use std::path::PathBuf;

use crate::DataDir;

const LOG_FILE_NAME: &str = "maki.log";

pub fn log_path(dir: &DataDir) -> PathBuf {
    dir.path().join(LOG_FILE_NAME)
}
