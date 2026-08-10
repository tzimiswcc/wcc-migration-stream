use std::{fs, path::Path};

pub fn initialize_test_workspace(root: &Path) {
    fs::create_dir_all(root).expect("create test object workspace");
    migration_stream::object_workspace::establish_for_local_automation(
        root,
        "Automated test workspace",
        "Synthetic migration object",
        "Migration Stream test suite",
    )
    .expect("initialize test object workspace");
}
