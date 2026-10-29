//! Manual engineering console launcher; no association or installer is added.
#[cfg(windows)]
#[path = "../../native/windows/document_open.rs"]
// Shared callable primitives are public source API; visibility adds no CLI route.
pub mod document_open;

fn main() -> std::process::ExitCode {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let result = std::env::current_exe()
            .map_err(|_| ())
            .and_then(|executable| {
                let operands = std::env::args_os()
                    .skip(1)
                    .take(2)
                    .map(|value| value.encode_wide().collect())
                    .collect();
                document_open::open_engineering_document(
                    executable.as_os_str().encode_wide().collect(),
                    operands,
                    &mut document_open::WindowsEngineeringLayout,
                    document_open::WindowsProcessRunner::default(),
                    document_open::WindowsLoopbackBrowser,
                )
                .map_err(|_| ())
            });
        if result.is_ok() {
            return std::process::ExitCode::SUCCESS;
        }
        eprintln!("{}", document_open::ENGINEERING_LAUNCH_ERROR);
    }
    #[cfg(not(windows))]
    eprintln!("This engineering launcher requires Windows.");
    std::process::ExitCode::FAILURE
}
