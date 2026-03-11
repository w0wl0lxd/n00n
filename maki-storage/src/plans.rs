use crate::{DataDir, StorageError, now_millis};

const PLANS_DIR: &str = "plans";

pub fn new_plan_path(dir: &DataDir) -> Result<String, StorageError> {
    let plans_dir = dir.ensure_subdir(PLANS_DIR)?;
    let ts = now_millis();
    Ok(format!("{}/{ts}.md", plans_dir.display()))
}
