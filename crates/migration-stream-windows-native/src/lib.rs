//! Deliberately narrow Windows systems boundary.
//!
//! The product crate is `unsafe_code = forbid`.  This crate contains the few
//! handle operations for which Rust's standard library has no safe equivalent.
//! Every public operation owns or borrows a Windows handle in a way that makes
//! its lifetime explicit; callers never receive a raw handle.
//!
//! This boundary is deliberately not an authority to launch a portable runtime
//! until callers can use handle-relative traversal for every mutable path
//! component. `CreateProcessW` still receives path strings for its executable
//! and current directory, so retained final handles provide replacement
//! detection and delete sharing protection, not a complete ancestor-race proof.

// The workspace is checked on macOS and Linux too. Its Windows-only public API
// is absent there; the crate deliberately carries no fallback implementation.

#[cfg(windows)]
mod native {
    use std::{
        ffi::{OsStr, OsString},
        fmt::Write as _,
        io,
        os::windows::ffi::{OsStrExt as _, OsStringExt as _},
        path::{Path, PathBuf},
        rc::Rc,
        time::Instant,
    };

    use sha2::{Digest as _, Sha256};
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER, ERROR_IO_INCOMPLETE,
            ERROR_IO_PENDING, ERROR_LOCK_VIOLATION, ERROR_NOT_FOUND, ERROR_PIPE_BUSY,
            ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, ERROR_SEM_TIMEOUT, GENERIC_READ,
            GENERIC_WRITE, GetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
            LocalFree, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Security::Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SE_FILE_OBJECT,
        },
        Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom},
        Security::{
            ACCESS_ALLOWED_ACE, ACE_HEADER, ACL_SIZE_INFORMATION, AclSizeInformation,
            DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation, GetLengthSid,
            GetSecurityDescriptorControl, GetSecurityDescriptorLength, GetTokenInformation,
            IsValidSid, OWNER_SECURITY_INFORMATION, PSID, RevertToSelf, SE_DACL_PROTECTED,
            SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
        },
        Storage::FileSystem::{
            CREATE_NEW, CreateDirectoryW, CreateFileW, DELETE, FILE_ALL_ACCESS,
            FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
            FILE_ATTRIBUTE_TAG_INFO, FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_OVERLAPPED,
            FILE_ID_INFO, FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_SHARE_READ,
            FILE_SHARE_WRITE, FILE_STANDARD_INFO, FileAttributeTagInfo, FileDispositionInfo,
            FileIdInfo, FileStandardInfo, FlushFileBuffers, GetFileInformationByHandleEx,
            GetFinalPathNameByHandleW, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
            LockFileEx, OPEN_ALWAYS, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, READ_CONTROL, ReadFile,
            SetFileInformationByHandle, VOLUME_NAME_GUID, WriteFile,
        },
        System::{
            Com::CoTaskMemFree,
            IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject, TerminateJobObject,
            },
            Pipes::{
                ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe,
                ImpersonateNamedPipeClient, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_TYPE_BYTE, PIPE_WAIT, WaitNamedPipeW,
            },
            SystemInformation::GetSystemWindowsDirectoryW,
            Threading::{
                CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateEventW, CreateProcessW,
                DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
                GetCurrentThread, GetExitCodeProcess, InitializeProcThreadAttributeList,
                OpenProcessToken, OpenThreadToken, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                PROCESS_INFORMATION, QueryFullProcessImageNameW, ResumeThread,
                STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
                WaitForSingleObject,
            },
        },
        UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath},
    };

    const SYSTEM_ROOT: [u16; 10] = [83, 121, 115, 116, 101, 109, 82, 111, 111, 116];
    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;

    /// Stable filesystem identity read from an opened handle, not a path.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct PathIdentity {
        volume_serial_number: u64,
        file_id: [u8; 16],
    }

    impl PathIdentity {
        #[must_use]
        pub const fn volume_serial_number(self) -> u64 {
            self.volume_serial_number
        }

        #[must_use]
        pub const fn file_id(self) -> [u8; 16] {
            self.file_id
        }
    }

    /// Domain-separated full Windows workspace identity. The volume serial and
    /// all 128 `FileId` bits are authority; this digest excludes the mutable path.
    #[must_use]
    pub fn windows_workspace_filesystem_identity_sha256(identity: PathIdentity) -> String {
        let mut digest = Sha256::new();
        digest.update(b"migration-stream/workspace-filesystem-identity/v1\0windows\0");
        digest.update(identity.volume_serial_number.to_be_bytes());
        digest.update(identity.file_id);
        lowercase_hex_32(&digest.finalize().into())
    }

    /// Read the full stable identity of one already-open Windows file handle.
    ///
    /// This deliberately avoids reopening a mutable path; the caller retains
    /// ownership of the handle for the comparison that follows.
    #[allow(clippy::missing_errors_doc)]
    pub fn file_identity_from_open_file(file: &std::fs::File) -> io::Result<PathIdentity> {
        use std::os::windows::io::AsRawHandle as _;

        file_identity(file.as_raw_handle())
    }

    /// Delete exactly one regular file through a retained no-reparse handle.
    ///
    /// The pathname is resolved once with `DELETE` access and no sharing. Its
    /// complete `FileId` must equal `expected`; only that opened handle receives
    /// the deletion disposition. A pathname replacement can therefore cause
    /// an error, but can never delete the replacement.
    #[allow(clippy::missing_errors_doc)]
    pub fn delete_file_exact(path: &Path, expected: PathIdentity) -> io::Result<()> {
        if !path.is_absolute() {
            return Err(invalid_input("exact deletion path must be absolute"));
        }
        let wide = wide_null(path.as_os_str())?;
        // SAFETY: `wide` is a retained NUL-terminated path. Zero sharing
        // denies concurrent writers/deleters after this exact handle opens;
        // OPEN_REPARSE_POINT lets the checks below reject a reparse object.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                DELETE | FILE_READ_ATTRIBUTES,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let result = (|| {
            let identity = file_identity(handle)?;
            let attributes = file_attributes(handle)?;
            let links = file_link_count(handle)?;
            if identity != expected
                || attributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY) != 0
                || links != 1
            {
                return Err(invalid_data(
                    "exact deletion target identity or regular-file authority changed",
                ));
            }
            mark_owned_file_for_delete(handle)
        })();
        // SAFETY: this owned handle is terminal after the disposition request;
        // CloseHandle commits a successful delete of this exact FileId.
        let close_result = if unsafe { CloseHandle(handle) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        };
        match (result, close_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Ok(()), Err(error)) | (Err(error), Ok(())) => Err(error),
            (Err(error), Err(cleanup)) => Err(with_cleanup(error, Err(cleanup))),
        }
    }

    /// One opened directory whose identity is independently captured.
    #[derive(Debug)]
    pub struct RealDirectory {
        path: PathBuf,
        process_path: PathBuf,
        identity: PathIdentity,
        components: Vec<LockedPathComponent>,
    }

    #[allow(clippy::missing_errors_doc)]
    impl RealDirectory {
        /// Capture an absolute directory only when it and every ancestor are
        /// non-reparse points and the opened handle has exactly one link.
        pub fn capture(path: &Path) -> io::Result<Self> {
            if !path.is_absolute() {
                return Err(invalid_input("directory must be an absolute path"));
            }
            let components = lock_path_components(path, true)?;
            let final_component = components
                .last()
                .ok_or_else(|| invalid_input("directory has no lockable component"))?;
            let process_path = process_compatible_path(path, final_component.handle)?;
            let directory = Self {
                path: final_component.normalized_path.clone(),
                process_path,
                identity: final_component.identity,
                components,
            };
            directory.verify()?;
            Ok(directory)
        }

        #[must_use]
        pub fn path(&self) -> &Path {
            &self.path
        }

        #[must_use]
        pub const fn identity(&self) -> PathIdentity {
            self.identity
        }

        fn process_path(&self) -> &Path {
            &self.process_path
        }

        /// Reopen and compare the exact filesystem object before a sensitive
        /// lifecycle action.  This detects replacement and junction changes.
        pub fn verify(&self) -> io::Result<()> {
            replay_locked_path(
                &self.path,
                true,
                &self.components,
                self.identity,
                "directory authority path changed identity",
            )?;
            replay_locked_path(
                &self.process_path,
                true,
                &self.components,
                self.identity,
                "directory process path changed identity",
            )
        }
    }

    impl Drop for RealDirectory {
        fn drop(&mut self) {
            // Components own their exact handles and close in reverse component order.
            self.components.reverse();
        }
    }

    /// Retained no-delete handles for every component of one verified executable
    /// path. This is intentionally distinct from a directory capability.
    #[derive(Debug)]
    pub struct RealFile {
        path: PathBuf,
        process_path: PathBuf,
        identity: PathIdentity,
        components: Vec<LockedPathComponent>,
    }

    #[allow(clippy::missing_errors_doc)]
    impl RealFile {
        pub fn capture(path: &Path) -> io::Result<Self> {
            if !path.is_absolute() {
                return Err(invalid_input("file must be an absolute path"));
            }
            let components = lock_path_components(path, false)?;
            let final_component = components
                .last()
                .ok_or_else(|| invalid_input("file has no lockable component"))?;
            let process_path = process_compatible_path(path, final_component.handle)?;
            let file = Self {
                path: final_component.normalized_path.clone(),
                process_path,
                identity: final_component.identity,
                components,
            };
            file.verify()?;
            Ok(file)
        }

        #[must_use]
        pub fn path(&self) -> &Path {
            &self.path
        }

        #[must_use]
        pub const fn identity(&self) -> PathIdentity {
            self.identity
        }

        fn process_path(&self) -> &Path {
            &self.process_path
        }

        pub fn verify(&self) -> io::Result<()> {
            replay_locked_path(
                &self.path,
                false,
                &self.components,
                self.identity,
                "file authority path changed identity",
            )?;
            replay_locked_path(
                &self.process_path,
                false,
                &self.components,
                self.identity,
                "file process path changed identity",
            )
        }
    }

    impl Drop for RealFile {
        fn drop(&mut self) {
            self.components.reverse();
        }
    }

    /// Process current-user SID retained only inside this native authority
    /// boundary. It is never surfaced in a product protocol or status record.
    #[derive(Debug)]
    pub struct CurrentUser {
        sid: AlignedSid,
    }

    #[allow(clippy::missing_errors_doc)]
    impl CurrentUser {
        pub fn capture() -> io::Result<Rc<Self>> {
            let mut token = std::ptr::null_mut();
            // SAFETY: pseudo current-process handle is valid; token output is
            // initialized by Windows and closed below on every path.
            if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
                return Err(io::Error::last_os_error());
            }
            let result = token_user_sid(token).map(|sid| Rc::new(Self { sid }));
            // SAFETY: OpenProcessToken returned this owned token handle.
            let _ = unsafe { CloseHandle(token) };
            result
        }

        fn sid(&self) -> PSID {
            self.sid.as_psid()
        }

        fn security_attributes(&self) -> io::Result<SecurityAttributes> {
            SecurityAttributes::for_current_user(self)
        }

        fn verify_file_security(&self, handle: HANDLE) -> io::Result<()> {
            verify_private_file_security(handle, self.sid())
        }
    }

    #[derive(Debug)]
    struct AlignedSid {
        words: Vec<usize>,
        byte_length: usize,
    }

    impl AlignedSid {
        fn as_psid(&self) -> PSID {
            self.words.as_ptr().cast_mut().cast()
        }

        fn bytes(&self) -> &[u8] {
            // SAFETY: `words` is owned aligned backing whose initialized prefix
            // is exactly `byte_length` copied from a validated Windows SID.
            unsafe { std::slice::from_raw_parts(self.words.as_ptr().cast(), self.byte_length) }
        }
    }

    /// Capture `LocalAppData` through the Windows Known Folder API, never an
    /// inherited environment variable. Callers must create product-private
    /// children through `PrivateRuntimeDirectory` before trusting state there.
    #[allow(clippy::missing_errors_doc)]
    pub fn local_app_data_directory() -> io::Result<Rc<RealDirectory>> {
        let mut path = std::ptr::null_mut();
        let known_folder = FOLDERID_LocalAppData;
        // SAFETY: Known Folder API allocates one CoTaskMem UTF-16 path on
        // success and the null token requests the current user location.
        let result = unsafe {
            SHGetKnownFolderPath(
                std::ptr::addr_of!(known_folder),
                0,
                std::ptr::null_mut(),
                &raw mut path,
            )
        };
        if result < 0 {
            // SAFETY: Known Folder documentation requires releasing any
            // returned allocation on failure too; freeing null is valid.
            unsafe { CoTaskMemFree(path.cast()) };
            return Err(io::Error::from_raw_os_error(result));
        }
        if path.is_null() {
            return Err(invalid_data("LocalAppData known-folder path is absent"));
        }
        // SAFETY: successful SHGetKnownFolderPath returns NUL-terminated data.
        let converted = unsafe {
            let mut length = 0_usize;
            while *path.add(length) != 0 {
                length += 1;
            }
            PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(
                path, length,
            )))
        };
        // SAFETY: Known Folder API allocated this through CoTaskMem.
        unsafe { CoTaskMemFree(path.cast()) };
        Ok(Rc::new(RealDirectory::capture(&converted)?))
    }

    /// System API spelling plus retained exact identity for `SystemRoot`.
    #[derive(Debug)]
    pub struct SystemRootDirectory {
        environment_path: PathBuf,
        authority: Rc<RealDirectory>,
    }

    impl SystemRootDirectory {
        /// Exact spelling returned by `GetSystemWindowsDirectoryW`, suitable
        /// for the conventional `SystemRoot` child environment value.
        #[must_use]
        pub fn environment_path(&self) -> &Path {
            &self.environment_path
        }

        /// Replay both the retained volume identity and the system API path.
        ///
        /// # Errors
        ///
        /// Returns an error if either spelling resolves differently or any
        /// retained component no longer passes exact identity verification.
        pub fn verify(&self) -> io::Result<()> {
            self.authority.verify()?;
            let spelled = RealDirectory::capture(&self.environment_path)?;
            if spelled.identity() != self.authority.identity() {
                return Err(invalid_data("SystemRoot spelling identity changed"));
            }
            Ok(())
        }
    }

    /// Capture the installed Windows directory through the system API rather
    /// than inherited `SystemRoot`. Authority is separately retained through
    /// its normalized volume-GUID path without changing the environment value.
    #[allow(clippy::missing_errors_doc)]
    pub fn system_root_directory() -> io::Result<SystemRootDirectory> {
        const MAXIMUM_WINDOWS_PATH_UNITS: usize = 32_767;
        let mut buffer = vec![0_u16; 260];
        loop {
            let capacity = u32::try_from(buffer.len())
                .map_err(|_| invalid_input("Windows directory buffer is too large"))?;
            // SAFETY: `buffer` is writable for exactly `capacity` UTF-16 units;
            // the system API writes one path and reports its required length.
            let written = unsafe { GetSystemWindowsDirectoryW(buffer.as_mut_ptr(), capacity) };
            if written == 0 {
                return Err(io::Error::last_os_error());
            }
            let written = usize::try_from(written)
                .map_err(|_| invalid_data("Windows directory length is invalid"))?;
            if written < buffer.len() {
                buffer.truncate(written);
                if buffer.is_empty() || buffer.contains(&0) {
                    return Err(invalid_data("Windows directory path is malformed"));
                }
                let path = PathBuf::from(OsString::from_wide(&buffer));
                let authority = Rc::new(RealDirectory::capture(&path)?);
                authority.verify()?;
                return Ok(SystemRootDirectory {
                    environment_path: path,
                    authority,
                });
            }
            if written > MAXIMUM_WINDOWS_PATH_UNITS {
                return Err(invalid_data("Windows directory path exceeds its bound"));
            }
            let capacity = written
                .checked_add(1)
                .ok_or_else(|| invalid_data("Windows directory length overflows"))?;
            buffer.resize(capacity, 0);
        }
    }

    /// A runtime directory that was created/opened with an exact current-user
    /// protected DACL and is retained through its no-reparse path handles.
    #[derive(Debug)]
    pub struct PrivateRuntimeDirectory {
        directory: Rc<RealDirectory>,
        user: Rc<CurrentUser>,
    }

    #[allow(clippy::missing_errors_doc, clippy::needless_pass_by_value)]
    impl PrivateRuntimeDirectory {
        /// Open or create exactly one private child of a previously captured
        /// trusted parent. Component names cannot introduce path parsing.
        pub fn open_or_create_child(
            parent: Rc<RealDirectory>,
            user: Rc<CurrentUser>,
            component: &str,
        ) -> io::Result<Self> {
            validate_private_component(component)?;
            parent.verify()?;
            let path = parent.path().join(component);
            let mut attributes = user.security_attributes()?;
            // SAFETY: `path` and self-relative security descriptor stay live
            // through creation; no ambient inheritance is requested.
            let created = unsafe {
                CreateDirectoryW(
                    wide_null(path.as_os_str())?.as_ptr(),
                    attributes.as_mut_ptr(),
                )
            };
            if created == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(183) {
                    return Err(error);
                }
            }
            parent.verify()?;
            let directory = Rc::new(RealDirectory::capture(&path)?);
            let handle = open_directory_security_handle(directory.path())?;
            let verification = user.verify_file_security(handle);
            // SAFETY: `handle` was opened above and has no other Rust owner.
            let _ = unsafe { CloseHandle(handle) };
            verification?;
            directory.verify()?;
            Ok(Self { directory, user })
        }

        #[must_use]
        pub fn directory(&self) -> Rc<RealDirectory> {
            Rc::clone(&self.directory)
        }

        /// Publish one state file only if its final component does not yet
        /// exist. The file is current-SID DACL protected, fully flushed, and
        /// identity/security replayed before this call returns. Replacement is
        /// intentionally not offered here: callers must make any ownership
        /// transition explicit rather than using `REPLACE_EXISTING`.
        pub fn create_state_file_new(&self, component: &str, bytes: &[u8]) -> io::Result<()> {
            validate_private_component(component)?;
            self.create_private_file_new(component, bytes)
        }

        #[cfg(test)]
        pub(crate) fn create_test_executable_file_new(
            &self,
            component: &str,
            bytes: &[u8],
        ) -> io::Result<()> {
            let stem = component
                .strip_suffix(".exe")
                .ok_or_else(|| invalid_input("test executable must use an explicit .exe suffix"))?;
            validate_private_component(stem)?;
            self.create_private_file_new(component, bytes)
        }

        fn create_private_file_new(&self, component: &str, bytes: &[u8]) -> io::Result<()> {
            self.directory.verify()?;
            let path = self.directory.path().join(component);
            let wide = wide_null(path.as_os_str())?;
            let mut attributes = self.user.security_attributes()?;
            // SAFETY: CREATE_NEW provides no-replace publication; the protected
            // descriptor and exact path are retained through the open call.
            let handle = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE | READ_CONTROL | DELETE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    attributes.as_mut_ptr(),
                    CREATE_NEW,
                    FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let result = (|| {
                write_file_all(handle, bytes)?;
                // SAFETY: all bytes were synchronously written to this owned
                // file handle; FlushFileBuffers establishes durable publication.
                if unsafe { FlushFileBuffers(handle) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                self.user.verify_file_security(handle)?;
                let _identity = file_identity(handle)?;
                let attributes = file_attributes(handle)?;
                let links = file_link_count(handle)?;
                if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 || links != 1 {
                    return Err(invalid_data(
                        "new private state file is a reparse point or has an alias",
                    ));
                }
                let _normalized_local_path = final_volume_path(handle)?;
                self.directory.verify()
            })();
            let cleanup = result
                .as_ref()
                .err()
                .map(|_| mark_owned_file_for_delete(handle));
            // SAFETY: this exact create-new handle has no other Rust owner.
            let _ = unsafe { CloseHandle(handle) };
            match (result, cleanup) {
                (Ok(()), _) => Ok(()),
                (Err(error), Some(Ok(())) | None) => Err(error),
                (Err(error), Some(Err(cleanup))) => Err(with_cleanup(error, Err(cleanup))),
            }
        }

        /// Open one existing state file through a no-reparse, no-sharing handle,
        /// replay its DACL and identity, then return at most `maximum_bytes`.
        pub fn read_state_file(
            &self,
            component: &str,
            maximum_bytes: usize,
        ) -> io::Result<Vec<u8>> {
            validate_private_component(component)?;
            self.directory.verify()?;
            let path = self.directory.path().join(component);
            let wide = wide_null(path.as_os_str())?;
            // SAFETY: no delete sharing prevents replacement while compatible
            // read/write sharing permits identity replay through a second handle.
            let handle = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    GENERIC_READ | READ_CONTROL,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let result = (|| {
                let file = RealFile::capture(&path)?;
                self.user.verify_file_security(handle)?;
                file.verify()?;
                let mut standard: FILE_STANDARD_INFO = unsafe { std::mem::zeroed() };
                // SAFETY: `standard` is writable POD output for this live file.
                if unsafe {
                    GetFileInformationByHandleEx(
                        handle,
                        FileStandardInfo,
                        std::ptr::addr_of_mut!(standard).cast(),
                        u32::try_from(std::mem::size_of_val(&standard))
                            .map_err(|_| invalid_input("state file information too large"))?,
                    )
                } == 0
                {
                    return Err(io::Error::last_os_error());
                }
                let size = usize::try_from(standard.EndOfFile)
                    .map_err(|_| invalid_data("state file has invalid length"))?;
                if size > maximum_bytes {
                    return Err(invalid_data("state file exceeds its read bound"));
                }
                let mut bytes = vec![0_u8; size];
                read_file_exact(handle, &mut bytes)?;
                self.directory.verify()?;
                file.verify()?;
                Ok(bytes)
            })();
            // SAFETY: this exact verified read handle is uniquely owned here.
            let _ = unsafe { CloseHandle(handle) };
            result
        }

        /// Delete one exact private state file only when its complete bytes
        /// still match the caller's previously validated snapshot. The exact
        /// no-reparse handle, not its path, receives the deletion disposition.
        pub fn delete_state_file_exact(
            &self,
            component: &str,
            expected_bytes: &[u8],
        ) -> io::Result<()> {
            const MAXIMUM_PRIVATE_STATE_BYTES: usize = 128 * 1024;
            validate_private_component(component)?;
            if expected_bytes.len() > MAXIMUM_PRIVATE_STATE_BYTES {
                return Err(invalid_input("expected state file exceeds its bound"));
            }
            self.directory.verify()?;
            let path = self.directory.path().join(component);
            let wide = wide_null(path.as_os_str())?;
            // SAFETY: this exact existing file is opened without write/delete
            // sharing, so its bytes and namespace cannot change during replay.
            let handle = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    GENERIC_READ | READ_CONTROL | DELETE,
                    FILE_SHARE_READ,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let result = (|| {
                self.user.verify_file_security(handle)?;
                let identity = file_identity(handle)?;
                let attributes = file_attributes(handle)?;
                let links = file_link_count(handle)?;
                if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 || links != 1 {
                    return Err(invalid_data(
                        "private state file is a reparse point or has an alias",
                    ));
                }
                let _normalized_local_path = final_volume_path(handle)?;
                let mut standard: FILE_STANDARD_INFO = unsafe { std::mem::zeroed() };
                // SAFETY: `standard` is writable POD output for this exact file.
                if unsafe {
                    GetFileInformationByHandleEx(
                        handle,
                        FileStandardInfo,
                        std::ptr::addr_of_mut!(standard).cast(),
                        u32::try_from(std::mem::size_of_val(&standard))
                            .map_err(|_| invalid_input("state file information too large"))?,
                    )
                } == 0
                {
                    return Err(io::Error::last_os_error());
                }
                let size = usize::try_from(standard.EndOfFile)
                    .map_err(|_| invalid_data("state file has invalid length"))?;
                if size != expected_bytes.len() {
                    return Err(invalid_data("private state file bytes changed"));
                }
                let mut actual = vec![0_u8; size];
                read_file_exact(handle, &mut actual)?;
                if actual != expected_bytes {
                    return Err(invalid_data("private state file bytes changed"));
                }
                self.directory.verify()?;
                self.user.verify_file_security(handle)?;
                if file_identity(handle)? != identity
                    || file_attributes(handle)? & FILE_ATTRIBUTE_REPARSE_POINT != 0
                    || file_link_count(handle)? != 1
                {
                    return Err(invalid_data("private state file authority changed"));
                }
                mark_owned_file_for_delete(handle)?;
                self.directory.verify()
            })();
            // SAFETY: the exact open/delete handle is uniquely owned here. A
            // successful disposition takes effect when this close completes.
            let close_result = if unsafe { CloseHandle(handle) } == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            };
            match (result, close_result) {
                (Ok(()), Ok(())) => Ok(()),
                (Ok(()), Err(error)) | (Err(error), Ok(())) => Err(error),
                (Err(error), Err(cleanup)) => Err(with_cleanup(error, Err(cleanup))),
            }
        }

        /// Create or recover one exact owned lock path, validate its DACL and
        /// identity, then take an immediate exclusive byte-range lock.
        pub fn open_and_lock(&self, component: &str) -> io::Result<PrivateLock> {
            validate_private_component(component)?;
            self.directory.verify()?;
            let path = self.directory.path().join(component);
            let mut attributes = self.user.security_attributes()?;
            let wide = wide_null(path.as_os_str())?;
            // SAFETY: security attributes and path storage remain valid; share
            // mode zero prohibits replacement/delete sharing for this handle.
            let handle = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    attributes.as_mut_ptr(),
                    OPEN_ALWAYS,
                    FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let file = match RealFile::capture(&path) {
                Ok(file) => Rc::new(file),
                Err(error) => {
                    // SAFETY: close the just-created/opened exact handle.
                    let _ = unsafe { CloseHandle(handle) };
                    return Err(error);
                }
            };
            if let Err(error) = self
                .user
                .verify_file_security(handle)
                .and_then(|()| file.verify())
            {
                // SAFETY: close the owned handle on validation failure.
                let _ = unsafe { CloseHandle(handle) };
                return Err(error);
            }
            // SAFETY: `handle` is exact and live; compatible read/write sharing
            // permits identity replay while omitted delete sharing pins its name.
            // zeroed OVERLAPPED locks its first byte without asynchronous use.
            let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
            if unsafe {
                LockFileEx(
                    handle,
                    LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                    0,
                    1,
                    0,
                    &raw mut overlapped,
                )
            } == 0
            {
                let error = io::Error::last_os_error();
                // SAFETY: lock was not acquired; close the owned handle.
                let _ = unsafe { CloseHandle(handle) };
                return Err(error);
            }
            let lock = PrivateLock {
                handle,
                file,
                directory: self.directory(),
                user: Rc::clone(&self.user),
            };
            lock.verify()?;
            Ok(lock)
        }
    }

    /// Exact current-user-owned file lock. Dropping closes the handle and
    /// releases the lock; a stale filename can therefore be reopened and
    /// verified for bounded abandoned-state recovery.
    #[derive(Debug)]
    pub struct PrivateLock {
        handle: HANDLE,
        file: Rc<RealFile>,
        directory: Rc<RealDirectory>,
        user: Rc<CurrentUser>,
    }

    #[allow(clippy::missing_errors_doc)]
    impl PrivateLock {
        pub fn verify(&self) -> io::Result<()> {
            self.directory.verify()?;
            self.user.verify_file_security(self.handle)?;
            self.file.verify()
        }
    }

    impl Drop for PrivateLock {
        fn drop(&mut self) {
            // SAFETY: this lock owns its one CreateFile handle.
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }

    /// Classify the expected immediate `LockFileEx` contention result without
    /// exposing a numeric Win32 error dependency to product protocol code.
    #[must_use]
    pub fn private_lock_is_contended(error: &io::Error) -> bool {
        error.kind() == io::ErrorKind::WouldBlock
            || error.raw_os_error() == Some(ERROR_LOCK_VIOLATION.cast_signed())
    }

    const MAXIMUM_PIPE_PAYLOAD_BYTES: usize = 128 * 1024;
    const RESPONSE_ACK_BYTES: usize = 32;
    const SID_FIXED_HEADER_BYTES: usize = 8;

    /// CSPRNG-generated capability for one local control session. The value is
    /// intentionally only exposed as fixed lowercase hex for a canonical JSON
    /// field; it is never derived from a path or public pipe name.
    #[derive(Clone)]
    pub struct CapabilityToken([u8; 32]);

    impl std::fmt::Debug for CapabilityToken {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("CapabilityToken([REDACTED])")
        }
    }

    #[allow(clippy::missing_errors_doc)]
    impl CapabilityToken {
        pub fn generate() -> io::Result<Self> {
            Ok(Self(random_bytes_32()?))
        }

        /// Import the exact representation stored only in a verified private
        /// launch/session record. Uppercase, whitespace, and variable length
        /// encodings are rejected to preserve one canonical protocol value.
        pub fn parse_lowercase_hex(value: &str) -> io::Result<Self> {
            Ok(Self(decode_lowercase_hex_32(value, "capability")?))
        }

        #[must_use]
        pub fn lowercase_hex(&self) -> String {
            lowercase_hex_32(&self.0)
        }
    }

    /// One CSPRNG launch nonce binding a readiness response to the exact
    /// supervisor process being adopted. It is not an authority by itself.
    #[derive(Clone)]
    pub struct SessionNonce([u8; 32]);

    impl std::fmt::Debug for SessionNonce {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("SessionNonce([REDACTED])")
        }
    }

    #[allow(clippy::missing_errors_doc)]
    impl SessionNonce {
        pub fn generate() -> io::Result<Self> {
            Ok(Self(random_bytes_32()?))
        }

        pub fn parse_lowercase_hex(value: &str) -> io::Result<Self> {
            Ok(Self(decode_lowercase_hex_32(value, "session nonce")?))
        }

        #[must_use]
        pub fn lowercase_hex(&self) -> String {
            lowercase_hex_32(&self.0)
        }
    }

    fn random_bytes_32() -> io::Result<[u8; 32]> {
        let mut bytes = [0_u8; 32];
        // SAFETY: null algorithm plus system-preferred flag asks BCrypt for
        // the documented OS CSPRNG and writes exactly `bytes.len()` bytes.
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                bytes.as_mut_ptr(),
                u32::try_from(bytes.len()).map_err(|_| invalid_input("random value too large"))?,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status < 0 {
            return Err(io::Error::from_raw_os_error(status));
        }
        Ok(bytes)
    }

    fn decode_lowercase_hex_32(value: &str, label: &str) -> io::Result<[u8; 32]> {
        if value.len() != 64 || !value.is_ascii() {
            return Err(invalid_input(&format!("{label} is not 64 lowercase hex")));
        }
        let mut decoded = [0_u8; 32];
        for (index, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
            let high = decode_lowercase_hex_nibble(pair[0])?;
            let low = decode_lowercase_hex_nibble(pair[1])?;
            decoded[index] = high * 16 + low;
        }
        Ok(decoded)
    }

    fn decode_lowercase_hex_nibble(value: u8) -> io::Result<u8> {
        match value {
            b'0'..=b'9' => Ok(value - b'0'),
            b'a'..=b'f' => Ok(value - b'a' + 10),
            _ => Err(invalid_input("value is not lowercase hex")),
        }
    }

    fn lowercase_hex_32(bytes: &[u8; 32]) -> String {
        let mut rendered = String::with_capacity(64);
        for byte in bytes {
            write!(&mut rendered, "{byte:02x}").expect("writing to String cannot fail");
        }
        rendered
    }

    /// Domain-separated acknowledgement for one complete response frame. The
    /// client sends these bytes only after it has read the response header and
    /// payload, preventing a server disconnect from silently discarding an
    /// unread response.
    #[must_use]
    pub fn response_ack(payload: &[u8]) -> [u8; RESPONSE_ACK_BYTES] {
        let mut digest = Sha256::new();
        digest.update(b"migration-stream/named-pipe-response-ack/v1\0");
        digest.update((payload.len() as u64).to_be_bytes());
        digest.update(payload);
        digest.finalize().into()
    }

    #[derive(Clone)]
    pub struct AuthenticatedControlBinding([u8; 32]);

    impl std::fmt::Debug for AuthenticatedControlBinding {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("AuthenticatedControlBinding([REDACTED])")
        }
    }

    #[allow(clippy::missing_errors_doc)]
    pub fn authenticated_control_binding(
        name_component: &str,
        capability: &CapabilityToken,
    ) -> io::Result<AuthenticatedControlBinding> {
        validate_private_component(name_component)?;
        let mut digest = Sha256::new();
        digest.update(b"migration-stream/authenticated-control-binding/v1\0");
        digest.update(name_component.as_bytes());
        digest.update([0]);
        digest.update(capability.0);
        Ok(AuthenticatedControlBinding(digest.finalize().into()))
    }

    /// Current-SID DACL-protected local byte-mode pipe. It serializes one
    /// request at a time and rejects remote clients at the kernel boundary.
    pub struct AuthenticatedPipeServer {
        handle: Option<HANDLE>,
        user: Rc<CurrentUser>,
        capability: CapabilityToken,
        poisoned: bool,
    }

    impl std::fmt::Debug for AuthenticatedPipeServer {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("AuthenticatedPipeServer")
                .field("pipe_handle_present", &self.handle.is_some())
                .field("poisoned", &self.poisoned)
                .finish_non_exhaustive()
        }
    }

    #[allow(clippy::missing_errors_doc)]
    impl AuthenticatedPipeServer {
        pub fn create(
            user: Rc<CurrentUser>,
            name_component: &str,
            capability: CapabilityToken,
        ) -> io::Result<Self> {
            validate_private_component(name_component)?;
            let name = format!(r"\\.\pipe\MigrationStream-{name_component}");
            let wide = wide_null(OsStr::new(&name))?;
            let mut attributes = user.security_attributes()?;
            // SAFETY: pipe name and protected SID descriptor remain live through
            // creation; FIRST_PIPE_INSTANCE refuses duplicate supervisors.
            let handle = unsafe {
                CreateNamedPipeW(
                    wide.as_ptr(),
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    1,
                    u32::try_from(MAXIMUM_PIPE_PAYLOAD_BYTES + 68)
                        .map_err(|_| invalid_input("pipe buffer too large"))?,
                    u32::try_from(MAXIMUM_PIPE_PAYLOAD_BYTES + 68)
                        .map_err(|_| invalid_input("pipe buffer too large"))?,
                    3_000,
                    attributes.as_mut_ptr(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            if let Err(error) = user.verify_file_security(handle) {
                // SAFETY: close the just-created pipe on failed verification.
                let _ = unsafe { CloseHandle(handle) };
                return Err(error);
            }
            Ok(Self {
                handle: Some(handle),
                user,
                capability,
                poisoned: false,
            })
        }

        /// Accept exactly one local current-SID request by `deadline`. The
        /// returned guard owns the connection and therefore cannot be forgotten
        /// by a caller that rejects its payload or fails before replying.
        pub fn accept(&mut self, deadline: Instant) -> io::Result<AcceptedPipeTransaction<'_>> {
            if self.poisoned {
                return Err(invalid_data(
                    "named-pipe server is poisoned after impersonation failure",
                ));
            }
            if let Err(error) = self.connect(deadline) {
                // A cancelled ConnectNamedPipe can race with a successful
                // connection. Reset this exact instance before reporting the
                // error so no client remains attached without an owning guard.
                return Err(self.disconnect_or_poison(error));
            }
            let result = (|| {
                let mut length = [0_u8; 4];
                self.read_exact(deadline, &mut length)?;
                let length = validate_pipe_payload_length(u32::from_be_bytes(length))?;
                let mut presented = [0_u8; 64];
                self.read_exact(deadline, &mut presented)?;
                let mut payload = vec![0_u8; length];
                self.read_exact(deadline, &mut payload)?;

                // Windows derives the impersonation context from pipe data read
                // by this server thread. Stage one complete bounded frame first,
                // but authenticate its SID before comparing the capability or
                // exposing any application payload to the caller.
                self.verify_impersonated_current_user()?;
                let expected = self.capability.lowercase_hex();
                if !constant_time_equal(&presented, expected.as_bytes()) {
                    return Err(invalid_data("pipe capability does not match"));
                }
                Ok(payload)
            })();
            match result {
                Ok(payload) => Ok(AcceptedPipeTransaction {
                    server: self,
                    payload,
                    deadline,
                    finished: false,
                }),
                Err(error) => Err(self.disconnect_or_poison(error)),
            }
        }

        fn handle(&self) -> io::Result<HANDLE> {
            self.handle
                .ok_or_else(|| invalid_data("named-pipe instance was closed after uncertain I/O"))
        }

        fn connect(&mut self, deadline: Instant) -> io::Result<()> {
            loop {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "named-pipe accept deadline expired",
                    ));
                }
                let handle = self.handle()?;
                let mut pending = PendingOperation::new()?;
                // SAFETY: `handle` is this server's overlapped pipe instance and
                // `pending` retains its event and OVERLAPPED until completion.
                let connected = unsafe { ConnectNamedPipe(handle, pending.as_mut_ptr()) };
                if connected != 0 {
                    return Ok(());
                }
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_PIPE_CONNECTED.cast_signed()) {
                    return Ok(());
                }
                if error.raw_os_error()
                    == Some(windows_sys::Win32::Foundation::ERROR_NO_DATA.cast_signed())
                {
                    // A client connected and vanished before the server issued
                    // ConnectNamedPipe. Reset only our instance, then continue
                    // accepting under the same caller-owned deadline.
                    self.disconnect_connection()?;
                    continue;
                }
                if error.raw_os_error() != Some(ERROR_IO_PENDING.cast_signed()) {
                    return Err(normalize_pipe_error(error));
                }
                return self.wait_for_pending(&mut pending, deadline).map(|_| ());
            }
        }

        fn read_exact(&mut self, deadline: Instant, bytes: &mut [u8]) -> io::Result<()> {
            let mut offset = 0_usize;
            while offset < bytes.len() {
                let read = self.read_once(deadline, &mut bytes[offset..])?;
                if read == 0 {
                    return Err(invalid_data("pipe request was truncated"));
                }
                offset = offset
                    .checked_add(read)
                    .ok_or_else(|| invalid_data("pipe read offset overflow"))?;
            }
            Ok(())
        }

        fn read_once(&mut self, deadline: Instant, bytes: &mut [u8]) -> io::Result<usize> {
            let handle = self.handle()?;
            let count =
                u32::try_from(bytes.len()).map_err(|_| invalid_input("pipe read too large"))?;
            let mut pending = PendingOperation::new()?;
            let mut read = 0_u32;
            // SAFETY: the buffer and `pending` are retained until terminal
            // completion is observed; ambiguous cancellation fail-stops instead
            // of allowing either value to be dropped.
            let issued = unsafe {
                ReadFile(
                    handle,
                    bytes.as_mut_ptr(),
                    count,
                    &raw mut read,
                    pending.as_mut_ptr(),
                )
            };
            if issued != 0 {
                return usize::try_from(read).map_err(|_| invalid_data("pipe read length invalid"));
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_IO_PENDING.cast_signed()) {
                return Err(normalize_pipe_error(error));
            }
            self.wait_for_pending(&mut pending, deadline)
                .and_then(|value| {
                    usize::try_from(value).map_err(|_| invalid_data("pipe read length invalid"))
                })
        }

        fn write_all(&mut self, deadline: Instant, bytes: &[u8]) -> io::Result<()> {
            let mut offset = 0_usize;
            while offset < bytes.len() {
                let written = self.write_once(deadline, &bytes[offset..])?;
                if written == 0 {
                    return Err(invalid_data("pipe response was truncated"));
                }
                offset = offset
                    .checked_add(written)
                    .ok_or_else(|| invalid_data("pipe write offset overflow"))?;
            }
            Ok(())
        }

        fn write_once(&mut self, deadline: Instant, bytes: &[u8]) -> io::Result<usize> {
            let handle = self.handle()?;
            let count =
                u32::try_from(bytes.len()).map_err(|_| invalid_input("pipe write too large"))?;
            let mut pending = PendingOperation::new()?;
            let mut written = 0_u32;
            // SAFETY: the immutable buffer and `pending` remain live until
            // terminal completion is observed; ambiguous cancellation
            // fail-stops instead of allowing either value to be dropped.
            let issued = unsafe {
                WriteFile(
                    handle,
                    bytes.as_ptr(),
                    count,
                    &raw mut written,
                    pending.as_mut_ptr(),
                )
            };
            if issued != 0 {
                return usize::try_from(written)
                    .map_err(|_| invalid_data("pipe write length invalid"));
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_IO_PENDING.cast_signed()) {
                return Err(normalize_pipe_error(error));
            }
            self.wait_for_pending(&mut pending, deadline)
                .and_then(|value| {
                    usize::try_from(value).map_err(|_| invalid_data("pipe write length invalid"))
                })
        }

        fn wait_for_pending(
            &mut self,
            pending: &mut PendingOperation,
            deadline: Instant,
        ) -> io::Result<u32> {
            let timeout = remaining_wait_millis(deadline);
            // SAFETY: `pending.event` is an owned event associated with this
            // exact OVERLAPPED operation.
            let waited = unsafe { WaitForSingleObject(pending.event, timeout) };
            if waited == WAIT_OBJECT_0 {
                return pending.completion(self.handle()?);
            }
            let original = if waited == WAIT_TIMEOUT {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "named-pipe operation deadline expired",
                )
            } else if waited == WAIT_FAILED {
                io::Error::last_os_error()
            } else {
                invalid_data("unexpected named-pipe completion wait result")
            };
            let cleanup = pending.cancel_and_reap(self.handle()?);
            if cleanup.is_err() {
                self.close_exact_handle();
                // The kernel may still hold `pending.overlapped`, its event, or
                // a caller's read/write buffer. Returning would drop them and
                // create a use-after-free, so this disposable supervisor must
                // fail-stop rather than rely on CloseHandle cancellation.
                std::process::abort();
            }
            Err(original)
        }

        fn disconnect_or_poison(&mut self, original: io::Error) -> io::Error {
            match self.disconnect_connection() {
                Ok(()) => original,
                Err(cleanup) => with_pipe_cleanup(original, Err(cleanup)),
            }
        }

        fn disconnect_connection(&mut self) -> io::Result<()> {
            let Some(handle) = self.handle else {
                return Err(invalid_data(
                    "named-pipe instance was closed after uncertain I/O",
                ));
            };
            // SAFETY: this is the one server-owned connected pipe instance.
            let disconnected = unsafe { DisconnectNamedPipe(handle) };
            if disconnected != 0 {
                return Ok(());
            }
            let cleanup = io::Error::last_os_error();
            if cleanup.raw_os_error() == Some(ERROR_PIPE_NOT_CONNECTED.cast_signed()) {
                return Ok(());
            }
            self.close_exact_handle();
            Err(cleanup)
        }

        fn close_exact_handle(&mut self) {
            if let Some(handle) = self.handle.take() {
                // SAFETY: this closes exactly the handle whose outstanding I/O
                // was cancelled; it cannot affect a foreign first instance.
                let _ = unsafe { CloseHandle(handle) };
            }
            self.poisoned = true;
        }

        fn verify_impersonated_current_user(&mut self) -> io::Result<()> {
            // SAFETY: this is a connected server pipe; impersonation is reverted
            // on every branch below before any application payload is accepted.
            if unsafe { ImpersonateNamedPipeClient(self.handle()?) } == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut token = std::ptr::null_mut();
            // SAFETY: current thread is impersonating the connected client.
            let opened =
                unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 0, &raw mut token) };
            let identity = if opened == 0 {
                Err(io::Error::last_os_error())
            } else {
                let result = token_user_sid(token).map(|sid| sid.bytes() == self.user.sid.bytes());
                // SAFETY: OpenThreadToken returned this owned token handle.
                let _ = unsafe { CloseHandle(token) };
                result.and_then(|same| {
                    same.then_some(())
                        .ok_or_else(|| invalid_data("pipe client SID differs"))
                })
            };
            // SAFETY: always revert the thread after every successful impersonation.
            let reverted = unsafe { RevertToSelf() } != 0;
            if !reverted {
                self.poisoned = true;
                // Continuing could run controller code under an untrusted client
                // token. This process is intentionally disposable on this path.
                std::process::abort();
            }
            identity
        }
    }

    impl Drop for AuthenticatedPipeServer {
        fn drop(&mut self) {
            self.close_exact_handle();
        }
    }

    /// An authenticated request whose connection is owned until it is finished
    /// or dropped. Payload validation and JSON parsing deliberately remain in
    /// the product layer; this boundary only authenticates and frames bytes.
    pub struct AcceptedPipeTransaction<'server> {
        server: &'server mut AuthenticatedPipeServer,
        payload: Vec<u8>,
        deadline: Instant,
        finished: bool,
    }

    impl std::fmt::Debug for AcceptedPipeTransaction<'_> {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("AcceptedPipeTransaction")
                .field("payload_length", &self.payload.len())
                .field("finished", &self.finished)
                .finish_non_exhaustive()
        }
    }

    #[allow(clippy::missing_errors_doc)]
    impl AcceptedPipeTransaction<'_> {
        #[must_use]
        pub fn payload(&self) -> &[u8] {
            &self.payload
        }

        /// Write one bounded response under the original overall deadline and
        /// always release this connection before returning.
        pub fn finish(mut self, payload: &[u8]) -> io::Result<()> {
            let result = (|| {
                if payload.is_empty() || payload.len() > MAXIMUM_PIPE_PAYLOAD_BYTES {
                    return Err(invalid_input("pipe response exceeds its bound"));
                }
                let length = u32::try_from(payload.len())
                    .map_err(|_| invalid_input("pipe response length invalid"))?
                    .to_be_bytes();
                self.server.write_all(self.deadline, &length)?;
                self.server.write_all(self.deadline, payload)?;
                let mut received_ack = [0_u8; RESPONSE_ACK_BYTES];
                self.server.read_exact(self.deadline, &mut received_ack)?;
                let expected_ack = response_ack(payload);
                if !constant_time_equal(&received_ack, &expected_ack) {
                    return Err(invalid_data(
                        "named-pipe response acknowledgement does not match",
                    ));
                }
                Ok(())
            })();
            self.finished = true;
            let disconnected = self.server.disconnect_connection();
            match (result, disconnected) {
                (Ok(()), Ok(())) => Ok(()),
                (Ok(()), Err(error)) | (Err(error), Ok(())) => Err(error),
                (Err(error), Err(cleanup)) => Err(with_pipe_cleanup(error, Err(cleanup))),
            }
        }
    }

    impl Drop for AcceptedPipeTransaction<'_> {
        fn drop(&mut self) {
            if !self.finished {
                let _ = self
                    .server
                    .disconnect_or_poison(invalid_data("accepted transaction dropped"));
            }
        }
    }

    /// One authenticated client-side control exchange. Its exact pipe handle
    /// is retained until the framed response has been acknowledged or dropped.
    #[derive(Debug)]
    pub struct AuthenticatedPipeClient {
        handle: HANDLE,
        deadline: Instant,
        capability: CapabilityToken,
        binding: AuthenticatedControlBinding,
        user: Rc<CurrentUser>,
    }

    #[allow(clippy::missing_errors_doc)]
    impl AuthenticatedPipeClient {
        /// Connect to one validated private pipe component before `deadline`.
        /// Windows exposes client connection through `CreateFileW`; all framed
        /// reads and writes use distinct overlapped operations below. The DACL
        /// excludes other SIDs; a hostile process already running as this exact
        /// user is outside this local same-user authority boundary.
        pub fn connect(
            user: Rc<CurrentUser>,
            name_component: &str,
            capability: CapabilityToken,
            deadline: Instant,
        ) -> io::Result<Self> {
            validate_private_component(name_component)?;
            let binding = authenticated_control_binding(name_component, &capability)?;
            let name = format!(r"\\.\pipe\MigrationStream-{name_component}");
            let wide = wide_null(OsStr::new(&name))?;
            loop {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "named-pipe client connection deadline expired",
                    ));
                }
                // SAFETY: `wide` is a retained NUL-terminated local pipe name;
                // an overlapped client handle is returned only on success.
                let handle = unsafe {
                    CreateFileW(
                        wide.as_ptr(),
                        GENERIC_READ | GENERIC_WRITE | READ_CONTROL,
                        0,
                        std::ptr::null(),
                        OPEN_EXISTING,
                        FILE_FLAG_OVERLAPPED,
                        std::ptr::null_mut(),
                    )
                };
                if handle != INVALID_HANDLE_VALUE {
                    if let Err(error) = user.verify_file_security(handle) {
                        // SAFETY: reject this exact untrusted pipe handle before
                        // any capability or request bytes are sent.
                        let _ = unsafe { CloseHandle(handle) };
                        return Err(error);
                    }
                    return Ok(Self {
                        handle,
                        deadline,
                        capability,
                        binding,
                        user,
                    });
                }
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND.cast_signed()) {
                    if !sleep_for_pipe_creation_retry(deadline) {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "named-pipe client connection deadline expired",
                        ));
                    }
                    continue;
                }
                if error.raw_os_error() != Some(ERROR_PIPE_BUSY.cast_signed()) {
                    return Err(normalize_pipe_error(error));
                }
                // SAFETY: WaitNamedPipeW receives the same retained local name
                // and a finite remaining duration, never INFINITE.
                let wait_timeout = pipe_client_wait_millis(deadline).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "named-pipe client connection deadline expired",
                    )
                })?;
                if unsafe { WaitNamedPipeW(wide.as_ptr(), wait_timeout) } == 0 {
                    let waited = io::Error::last_os_error();
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "named-pipe client connection deadline expired",
                        ));
                    }
                    if retryable_pipe_wait_error(waited.raw_os_error()) {
                        continue;
                    }
                    return Err(normalize_pipe_error(waited));
                }
            }
        }

        /// Send one bounded request and return the acknowledged response bytes.
        /// Canonical JSON validation remains with the product protocol layer.
        pub fn transact(self, payload: &[u8]) -> io::Result<AuthenticatedPipeResponse> {
            (|| {
                if payload.is_empty() || payload.len() > MAXIMUM_PIPE_PAYLOAD_BYTES {
                    return Err(invalid_input("pipe request exceeds its bound"));
                }
                let length = u32::try_from(payload.len())
                    .map_err(|_| invalid_input("pipe request length invalid"))?
                    .to_be_bytes();
                endpoint_write_all(self.handle, self.deadline, &length)?;
                let capability = self.capability.lowercase_hex();
                endpoint_write_all(self.handle, self.deadline, capability.as_bytes())?;
                endpoint_write_all(self.handle, self.deadline, payload)?;
                let mut response_length = [0_u8; 4];
                endpoint_read_exact(self.handle, self.deadline, &mut response_length)?;
                let response_length =
                    validate_pipe_payload_length(u32::from_be_bytes(response_length))?;
                let mut response = vec![0_u8; response_length];
                endpoint_read_exact(self.handle, self.deadline, &mut response)?;
                endpoint_write_all(self.handle, self.deadline, &response_ack(&response))?;
                Ok(AuthenticatedPipeResponse {
                    binding: self.binding.clone(),
                    payload: response,
                    user: Rc::clone(&self.user),
                })
            })()
        }
    }

    /// Response bytes that were read and acknowledged on an authenticated pipe.
    pub struct AuthenticatedPipeResponse {
        binding: AuthenticatedControlBinding,
        payload: Vec<u8>,
        user: Rc<CurrentUser>,
    }

    impl std::fmt::Debug for AuthenticatedPipeResponse {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("AuthenticatedPipeResponse")
                .field("payload_length", &self.payload.len())
                .finish_non_exhaustive()
        }
    }

    impl AuthenticatedPipeResponse {
        #[must_use]
        pub fn payload(&self) -> &[u8] {
            &self.payload
        }
        /// Run the caller's strict readiness validator before consuming the
        /// response into the unforgeable permit required for detachment.
        ///
        /// # Errors
        ///
        /// Returns the validator's error without creating a detach permit.
        pub fn validate_readiness(
            self,
            expected_process_id: u32,
            expected_nonce: SessionNonce,
            validator: impl FnOnce(&[u8]) -> io::Result<()>,
        ) -> io::Result<AuthenticatedReadinessPermit> {
            validator(&self.payload)?;
            Ok(AuthenticatedReadinessPermit {
                binding: self.binding,
                _user: self.user,
                process_id: expected_process_id,
                nonce: expected_nonce,
            })
        }
    }

    /// Unforgeable evidence of a completed authenticated response exchange.
    #[derive(Debug)]
    pub struct AuthenticatedReadinessPermit {
        binding: AuthenticatedControlBinding,
        _user: Rc<CurrentUser>,
        process_id: u32,
        nonce: SessionNonce,
    }

    impl Drop for AuthenticatedPipeClient {
        fn drop(&mut self) {
            // SAFETY: any pending endpoint I/O either proved terminal completion
            // or fail-stopped, so this owned handle can now be closed safely.
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }

    struct SecurityAttributes {
        descriptor: *mut core::ffi::c_void,
        attributes: SECURITY_ATTRIBUTES,
    }

    impl SecurityAttributes {
        fn for_current_user(user: &CurrentUser) -> io::Result<Self> {
            let sid = sid_string(user.sid())?;
            let sddl = format!("O:{sid}D:P(A;;FA;;;{sid})");
            let wide = wide_null(OsStr::new(&sddl))?;
            let mut descriptor = std::ptr::null_mut();
            // SAFETY: `wide` is a valid NUL-terminated SDDL string; Windows
            // allocates one self-relative descriptor returned in `descriptor`.
            if unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    wide.as_ptr(),
                    1,
                    &raw mut descriptor,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            let attributes = SECURITY_ATTRIBUTES {
                nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>())
                    .map_err(|_| invalid_input("security attributes are too large"))?,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            };
            Ok(Self {
                descriptor,
                attributes,
            })
        }

        fn as_mut_ptr(&mut self) -> *const SECURITY_ATTRIBUTES {
            std::ptr::addr_of!(self.attributes)
        }
    }

    impl Drop for SecurityAttributes {
        fn drop(&mut self) {
            if !self.descriptor.is_null() {
                // SAFETY: descriptor was allocated by the documented SDDL API.
                let _ = unsafe { LocalFree(self.descriptor.cast()) };
            }
        }
    }

    fn token_user_sid(token: HANDLE) -> io::Result<AlignedSid> {
        let mut required = 0_u32;
        // SAFETY: query-only sizing call uses null buffer as documented.
        let _ = unsafe {
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &raw mut required)
        };
        if required == 0 {
            return Err(io::Error::last_os_error());
        }
        let buffer_length =
            usize::try_from(required).map_err(|_| invalid_input("token data too large"))?;
        let mut bytes = vec![0_usize; buffer_length.div_ceil(std::mem::size_of::<usize>())];
        let mut returned = 0_u32;
        // SAFETY: `bytes` is exactly the documented token information buffer.
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                bytes.as_mut_ptr().cast(),
                u32::try_from(buffer_length).map_err(|_| invalid_input("token data too large"))?,
                &raw mut returned,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful TokenUser query initializes this C structure at
        // the beginning of `bytes` for the buffer's full lifetime.
        let token_user = unsafe { &*bytes.as_ptr().cast::<TOKEN_USER>() };
        // SAFETY: TokenUser contains one valid SID owned by `bytes`.
        let sid_length = unsafe { GetLengthSid(token_user.User.Sid) };
        if sid_length == 0 {
            return Err(io::Error::last_os_error());
        }
        let byte_length =
            usize::try_from(sid_length).map_err(|_| invalid_input("SID too large"))?;
        let mut words = vec![0_usize; byte_length.div_ceil(std::mem::size_of::<usize>())];
        // SAFETY: source is the validated SID and destination has exact size.
        unsafe {
            std::ptr::copy_nonoverlapping(
                token_user.User.Sid.cast::<u8>(),
                words.as_mut_ptr().cast(),
                byte_length,
            );
        }
        Ok(AlignedSid { words, byte_length })
    }

    fn sid_string(sid: PSID) -> io::Result<String> {
        let mut rendered = std::ptr::null_mut();
        // SAFETY: `sid` is a retained validated SID; Windows LocalAlloc's the
        // NUL-terminated UTF-16 result and we free it below.
        if unsafe { ConvertSidToStringSidW(sid, &raw mut rendered) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful API output is a NUL-terminated UTF-16 string.
        let result = unsafe {
            let mut length = 0_usize;
            while *rendered.add(length) != 0 {
                length += 1;
            }
            String::from_utf16(std::slice::from_raw_parts(rendered, length))
                .map_err(|_| invalid_data("current-user SID is not UTF-16"))
        };
        // SAFETY: ConvertSidToStringSidW allocated this LocalAlloc buffer.
        let _ = unsafe { LocalFree(rendered.cast()) };
        result
    }

    #[allow(clippy::too_many_lines)]
    fn verify_private_file_security(handle: HANDLE, expected_owner: PSID) -> io::Result<()> {
        let mut owner = std::ptr::null_mut();
        let mut dacl = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: handle is live, outputs are initialized only by Windows; the
        // returned descriptor is released with LocalFree below.
        let status = unsafe {
            GetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &raw mut owner,
                std::ptr::null_mut(),
                &raw mut dacl,
                std::ptr::null_mut(),
                &raw mut descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status.cast_signed()));
        }
        let result = (|| {
            if descriptor.is_null() || owner.is_null() {
                return Err(invalid_data(
                    "private object security descriptor or owner is absent",
                ));
            }
            // SAFETY: owner was returned with `descriptor` and remains live.
            if unsafe { EqualSid(owner, expected_owner) } == 0 {
                return Err(invalid_data("private object owner SID differs"));
            }
            let mut control = 0_u16;
            let mut revision = 0_u32;
            // SAFETY: descriptor is the valid object descriptor returned above.
            if unsafe {
                GetSecurityDescriptorControl(descriptor, &raw mut control, &raw mut revision)
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            if control & SE_DACL_PROTECTED == 0 || dacl.is_null() {
                return Err(invalid_data("private object DACL is absent or inherited"));
            }
            // SAFETY: zeroed is valid output storage for ACL_SIZE_INFORMATION.
            let mut size: ACL_SIZE_INFORMATION = unsafe { std::mem::zeroed() };
            if unsafe {
                GetAclInformation(
                    dacl,
                    std::ptr::addr_of_mut!(size).cast(),
                    u32::try_from(std::mem::size_of::<ACL_SIZE_INFORMATION>())
                        .map_err(|_| invalid_input("ACL size overflow"))?,
                    AclSizeInformation,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            if size.AceCount != 1 {
                return Err(invalid_data(
                    "private object DACL must contain exactly one ACE",
                ));
            }
            let mut ace = std::ptr::null_mut();
            // SAFETY: validated one-entry ACL; Windows returns the first ACE.
            if unsafe { GetAce(dacl, 0, &raw mut ace) } == 0 {
                return Err(io::Error::last_os_error());
            }
            if ace.is_null() {
                return Err(invalid_data("private object ACE is absent"));
            }
            // SAFETY: descriptor is valid and returned by GetSecurityInfo.
            let descriptor_length = unsafe { GetSecurityDescriptorLength(descriptor) } as usize;
            let descriptor_start = descriptor.cast::<u8>() as usize;
            let descriptor_end = descriptor_start
                .checked_add(descriptor_length)
                .ok_or_else(|| invalid_data("private security descriptor overflows"))?;
            let ace_start = ace.cast::<u8>() as usize;
            let header_size = std::mem::size_of::<ACE_HEADER>();
            if ace_start < descriptor_start
                || ace_start
                    .checked_add(header_size)
                    .is_none_or(|end| end > descriptor_end)
            {
                return Err(invalid_data(
                    "private object ACE header is outside descriptor",
                ));
            }
            // SAFETY: header bounds were proven against the returned descriptor.
            let header = unsafe { &*ace.cast::<ACE_HEADER>() };
            let ace_size = usize::from(header.AceSize);
            let ace_end = ace_start.checked_add(ace_size).ok_or_else(|| {
                invalid_data("private object ACE size overflows descriptor bounds")
            })?;
            if ace_size < std::mem::size_of::<ACCESS_ALLOWED_ACE>() || ace_end > descriptor_end {
                return Err(invalid_data("private object ACE size is invalid"));
            }
            if !ace_start.is_multiple_of(std::mem::align_of::<ACCESS_ALLOWED_ACE>()) {
                return Err(invalid_data("private object ACE alignment is invalid"));
            }
            // SAFETY: type, alignment, and complete ACE bounds are checked first.
            let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
            if allowed.Header.AceType != ACCESS_ALLOWED_ACE_TYPE
                || allowed.Header.AceFlags != 0
                || allowed.Mask != FILE_ALL_ACCESS
            {
                return Err(invalid_data(
                    "private object DACL is not exact owner-only full access",
                ));
            }
            let ace_sid: PSID = std::ptr::addr_of!(allowed.SidStart).cast_mut().cast();
            let sid_start = ace_sid.cast::<u8>() as usize;
            let sid_fixed_end = sid_start
                .checked_add(SID_FIXED_HEADER_BYTES)
                .ok_or_else(|| invalid_data("private object ACE SID header overflows"))?;
            if sid_start < ace_start || sid_fixed_end > ace_end {
                return Err(invalid_data(
                    "private object ACE SID fixed header exceeds ACE bounds",
                ));
            }
            // SAFETY: the fixed SID header is fully contained within the exact
            // ACE extent, so its second byte is the bounded subauthority count.
            let subauthority_count = usize::from(unsafe { *ace_sid.cast::<u8>().add(1) });
            let (sid_length, _) =
                exact_sid_extent(ace_start, ace_end, sid_start, subauthority_count)?;
            // SAFETY: the complete SID extent was computed from its fixed header
            // and proven to equal the complete ACE extent before validation.
            if unsafe { IsValidSid(ace_sid) } == 0 {
                return Err(invalid_data("private object ACE SID is invalid"));
            }
            let reported_sid_length = unsafe { GetLengthSid(ace_sid) } as usize;
            if reported_sid_length != sid_length {
                return Err(invalid_data(
                    "private object ACE SID length differs from its bounded encoding",
                ));
            }
            if unsafe { EqualSid(ace_sid, expected_owner) } == 0 {
                return Err(invalid_data("private object DACL ACE SID differs"));
            }
            Ok(())
        })();
        // SAFETY: GetSecurityInfo allocated this descriptor with LocalAlloc.
        let _ = unsafe { LocalFree(descriptor.cast()) };
        result
    }

    fn open_directory_security_handle(path: &Path) -> io::Result<HANDLE> {
        let wide = wide_null(path.as_os_str())?;
        // SAFETY: path storage is live and NUL terminated; no delete sharing or
        // reparse following is allowed while the descriptor is inspected.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_READ_ATTRIBUTES | READ_CONTROL,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(handle)
        }
    }

    fn validate_private_component(component: &str) -> io::Result<()> {
        if component.is_empty()
            || component.len() > 80
            || !component
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(invalid_input("private runtime component is unsafe"));
        }
        let uppercase = component.to_ascii_uppercase();
        if matches!(uppercase.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (uppercase.len() == 4
                && (uppercase.starts_with("COM") || uppercase.starts_with("LPT"))
                && matches!(uppercase.as_bytes()[3], b'1'..=b'9'))
        {
            return Err(invalid_input(
                "private runtime component is a Windows device alias",
            ));
        }
        Ok(())
    }

    /// One event and OVERLAPPED record for one pending kernel request. The pair
    /// is never reused, so a late completion cannot be confused with a later
    /// frame operation on the same pipe instance.
    struct PendingOperation {
        event: HANDLE,
        overlapped: OVERLAPPED,
    }

    impl PendingOperation {
        fn new() -> io::Result<Self> {
            // SAFETY: unnamed manual-reset event starts nonsignaled and is owned
            // by this operation until its completion has been reaped.
            let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
            if event.is_null() {
                return Err(io::Error::last_os_error());
            }
            let overlapped = OVERLAPPED {
                hEvent: event,
                ..OVERLAPPED::default()
            };
            Ok(Self { event, overlapped })
        }

        fn as_mut_ptr(&mut self) -> *mut OVERLAPPED {
            &raw mut self.overlapped
        }

        fn completion(&mut self, handle: HANDLE) -> io::Result<u32> {
            let mut transferred = 0_u32;
            // SAFETY: the event has signaled for this exact retained OVERLAPPED;
            // bWait=false cannot block and reaps its terminal result.
            if unsafe { GetOverlappedResult(handle, self.as_mut_ptr(), &raw mut transferred, 0) }
                == 0
            {
                return Err(normalize_pipe_error(io::Error::last_os_error()));
            }
            Ok(transferred)
        }

        /// Cancel this one request then prove that completion was observed. Any
        /// returned error is an ambiguity that the caller must fail-stop before
        /// this operation, its event, or its borrowed buffer can be dropped.
        fn cancel_and_reap(&mut self, handle: HANDLE) -> io::Result<()> {
            // SAFETY: this targets only the exact OVERLAPPED operation owned by
            // this value, never unrelated I/O on the same server handle.
            let cancelled = unsafe { CancelIoEx(handle, self.as_mut_ptr()) };
            if cancelled == 0 {
                let cancellation = io::Error::last_os_error();
                if cancellation.raw_os_error() != Some(ERROR_NOT_FOUND.cast_signed()) {
                    return Err(cancellation);
                }
            }
            // SAFETY: wait only on our owned completion event for a short
            // bounded cleanup period; this does not extend the request deadline.
            let waited = unsafe { WaitForSingleObject(self.event, 1_000) };
            if waited == WAIT_OBJECT_0 {
                let mut transferred = 0_u32;
                // SAFETY: event completion makes this a nonblocking terminal
                // reap. Cancellation normally reports OPERATION_ABORTED, which
                // is still a successfully reaped operation.
                let completed = unsafe {
                    GetOverlappedResult(handle, self.as_mut_ptr(), &raw mut transferred, 0)
                };
                if completed == 0
                    && io::Error::last_os_error().raw_os_error()
                        == Some(ERROR_IO_INCOMPLETE.cast_signed())
                {
                    return Err(invalid_data(
                        "named-pipe event signaled without a terminal I/O result",
                    ));
                }
                return Ok(());
            }
            if waited == WAIT_TIMEOUT {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "named-pipe cancellation did not complete within its cleanup bound",
                ));
            }
            if waited == WAIT_FAILED {
                return Err(io::Error::last_os_error());
            }
            Err(invalid_data(
                "unexpected named-pipe cancellation wait result",
            ))
        }
    }

    impl Drop for PendingOperation {
        fn drop(&mut self) {
            // SAFETY: `event` is one uniquely owned CreateEvent handle.
            let _ = unsafe { CloseHandle(self.event) };
        }
    }

    fn endpoint_read_exact(handle: HANDLE, deadline: Instant, bytes: &mut [u8]) -> io::Result<()> {
        let mut offset = 0_usize;
        while offset < bytes.len() {
            let read = endpoint_read_once(handle, deadline, &mut bytes[offset..])?;
            if read == 0 {
                return Err(invalid_data("named-pipe response was truncated"));
            }
            offset = offset
                .checked_add(read)
                .ok_or_else(|| invalid_data("named-pipe response read offset overflow"))?;
        }
        Ok(())
    }

    fn read_file_exact(handle: HANDLE, bytes: &mut [u8]) -> io::Result<()> {
        let mut offset = 0_usize;
        while offset < bytes.len() {
            let mut read = 0_u32;
            // SAFETY: synchronous exact file handle and writable remaining bytes.
            if unsafe {
                ReadFile(
                    handle,
                    bytes[offset..].as_mut_ptr(),
                    u32::try_from(bytes.len() - offset)
                        .map_err(|_| invalid_input("state file read too large"))?,
                    &raw mut read,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            if read == 0 {
                return Err(invalid_data("state file was truncated while read"));
            }
            offset += usize::try_from(read).map_err(|_| invalid_data("state file read length"))?;
        }
        Ok(())
    }

    fn mark_owned_file_for_delete(handle: HANDLE) -> io::Result<()> {
        let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
        // SAFETY: this is the exact CREATE_NEW handle retained by the caller;
        // deletion is marked by handle, never by a later pathname lookup.
        if unsafe {
            SetFileInformationByHandle(
                handle,
                FileDispositionInfo,
                std::ptr::addr_of!(disposition).cast(),
                u32::try_from(std::mem::size_of_val(&disposition))
                    .map_err(|_| invalid_input("file disposition structure too large"))?,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn write_file_all(handle: HANDLE, bytes: &[u8]) -> io::Result<()> {
        let mut offset = 0_usize;
        while offset < bytes.len() {
            let mut written = 0_u32;
            // SAFETY: synchronous exact file handle and retained source bytes.
            if unsafe {
                WriteFile(
                    handle,
                    bytes[offset..].as_ptr(),
                    u32::try_from(bytes.len() - offset)
                        .map_err(|_| invalid_input("state file write too large"))?,
                    &raw mut written,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            if written == 0 {
                return Err(invalid_data("state file write was truncated"));
            }
            offset +=
                usize::try_from(written).map_err(|_| invalid_data("state file write length"))?;
        }
        Ok(())
    }

    fn endpoint_read_once(
        handle: HANDLE,
        deadline: Instant,
        bytes: &mut [u8],
    ) -> io::Result<usize> {
        let count = u32::try_from(bytes.len()).map_err(|_| invalid_input("pipe read too large"))?;
        let mut pending = PendingOperation::new()?;
        let mut read = 0_u32;
        // SAFETY: `bytes` and `pending` stay live until terminal completion is
        // observed; an ambiguous cancellation aborts the process instead.
        let issued = unsafe {
            ReadFile(
                handle,
                bytes.as_mut_ptr(),
                count,
                &raw mut read,
                pending.as_mut_ptr(),
            )
        };
        if issued != 0 {
            return usize::try_from(read).map_err(|_| invalid_data("pipe read length invalid"));
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_IO_PENDING.cast_signed()) {
            return Err(normalize_pipe_error(error));
        }
        endpoint_wait_for_pending(handle, &mut pending, deadline).and_then(|value| {
            usize::try_from(value).map_err(|_| invalid_data("pipe read length invalid"))
        })
    }

    fn endpoint_write_all(handle: HANDLE, deadline: Instant, bytes: &[u8]) -> io::Result<()> {
        let mut offset = 0_usize;
        while offset < bytes.len() {
            let count = u32::try_from(bytes.len() - offset)
                .map_err(|_| invalid_input("pipe write too large"))?;
            let mut pending = PendingOperation::new()?;
            let mut written = 0_u32;
            // SAFETY: source storage and operation state remain live until a
            // terminal completion is observed or the process fail-stops.
            let issued = unsafe {
                WriteFile(
                    handle,
                    bytes[offset..].as_ptr(),
                    count,
                    &raw mut written,
                    pending.as_mut_ptr(),
                )
            };
            let written = if issued != 0 {
                written
            } else {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_IO_PENDING.cast_signed()) {
                    return Err(normalize_pipe_error(error));
                }
                endpoint_wait_for_pending(handle, &mut pending, deadline)?
            };
            if written == 0 {
                return Err(invalid_data("named-pipe request write was truncated"));
            }
            offset = offset
                .checked_add(
                    usize::try_from(written)
                        .map_err(|_| invalid_data("pipe write length invalid"))?,
                )
                .ok_or_else(|| invalid_data("named-pipe request write offset overflow"))?;
        }
        Ok(())
    }

    fn endpoint_wait_for_pending(
        handle: HANDLE,
        pending: &mut PendingOperation,
        deadline: Instant,
    ) -> io::Result<u32> {
        // SAFETY: this event belongs to `pending` and no other operation.
        let waited = unsafe { WaitForSingleObject(pending.event, remaining_wait_millis(deadline)) };
        if waited == WAIT_OBJECT_0 {
            return pending.completion(handle);
        }
        let original = if waited == WAIT_TIMEOUT {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "named-pipe operation deadline expired",
            )
        } else if waited == WAIT_FAILED {
            io::Error::last_os_error()
        } else {
            invalid_data("unexpected named-pipe completion wait result")
        };
        if pending.cancel_and_reap(handle).is_err() {
            // Neither the caller buffer nor this OVERLAPPED can be dropped
            // without a terminal completion proof.
            std::process::abort();
        }
        Err(original)
    }

    fn remaining_wait_millis(deadline: Instant) -> u32 {
        finite_wait_millis(deadline.saturating_duration_since(Instant::now()))
    }

    fn pipe_client_wait_millis(deadline: Instant) -> Option<u32> {
        let remaining = deadline.checked_duration_since(Instant::now())?;
        (!remaining.is_zero()).then(|| pipe_client_wait_for_duration(remaining))
    }

    fn pipe_client_wait_for_duration(remaining: std::time::Duration) -> u32 {
        finite_wait_millis(remaining).max(1)
    }

    fn retryable_pipe_wait_error(raw_error: Option<i32>) -> bool {
        matches!(
            raw_error,
            Some(code)
                if code == ERROR_SEM_TIMEOUT.cast_signed()
                    || code == ERROR_FILE_NOT_FOUND.cast_signed()
                    || code == ERROR_PIPE_BUSY.cast_signed()
        )
    }

    fn sleep_for_pipe_creation_retry(deadline: Instant) -> bool {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return false;
        };
        if remaining.is_zero() {
            return false;
        }
        std::thread::sleep(remaining.min(std::time::Duration::from_millis(1)));
        true
    }

    fn finite_wait_millis(remaining: std::time::Duration) -> u32 {
        let maximum_finite_wait = u128::from(u32::MAX - 1);
        u32::try_from(remaining.as_millis().min(maximum_finite_wait))
            .expect("finite Windows wait value fits u32")
    }

    fn exact_sid_extent(
        ace_start: usize,
        ace_end: usize,
        sid_start: usize,
        subauthority_count: usize,
    ) -> io::Result<(usize, usize)> {
        if sid_start < ace_start {
            return Err(invalid_data("private object ACE SID begins before its ACE"));
        }
        let sid_length = SID_FIXED_HEADER_BYTES
            .checked_add(
                subauthority_count
                    .checked_mul(std::mem::size_of::<u32>())
                    .ok_or_else(|| invalid_data("private object ACE SID count overflows"))?,
            )
            .ok_or_else(|| invalid_data("private object ACE SID length overflows"))?;
        let sid_end = sid_start
            .checked_add(sid_length)
            .ok_or_else(|| invalid_data("private object ACE SID length overflows"))?;
        if sid_end != ace_end {
            return Err(invalid_data(
                "private object ACE must contain exactly one bounded SID",
            ));
        }
        Ok((sid_length, sid_end))
    }

    fn validate_pipe_payload_length(length: u32) -> io::Result<usize> {
        let length =
            usize::try_from(length).map_err(|_| invalid_input("pipe payload length invalid"))?;
        if length == 0 || length > MAXIMUM_PIPE_PAYLOAD_BYTES {
            return Err(invalid_data("pipe payload exceeds its bound"));
        }
        Ok(length)
    }

    fn normalize_pipe_error(error: io::Error) -> io::Error {
        if matches!(
            error.raw_os_error(),
            Some(code)
                if code == windows_sys::Win32::Foundation::ERROR_BROKEN_PIPE.cast_signed()
                    || code == windows_sys::Win32::Foundation::ERROR_NO_DATA.cast_signed()
        ) {
            invalid_data("named-pipe peer disconnected during a framed transaction")
        } else {
            error
        }
    }

    fn with_pipe_cleanup(original: io::Error, cleanup: io::Result<()>) -> io::Error {
        match cleanup {
            Ok(()) => original,
            Err(cleanup) => io::Error::new(
                original.kind(),
                format!("{original}; named-pipe cleanup was not proven: {cleanup}"),
            ),
        }
    }

    fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
        if left.len() != right.len() {
            return false;
        }
        let mut difference = 0_u8;
        for (left, right) in left.iter().zip(right) {
            difference |= left ^ right;
        }
        difference == 0
    }

    #[derive(Debug)]
    struct LockedPathComponent {
        identity: PathIdentity,
        normalized_path: PathBuf,
        handle: HANDLE,
    }

    impl Drop for LockedPathComponent {
        fn drop(&mut self) {
            // SAFETY: this is one exclusively owned no-follow handle.
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }

    /// An owned Windows Job configured to kill every assigned process when its
    /// final handle closes.  A caller must assign a child before resuming it.
    #[derive(Debug)]
    pub struct KillOnCloseJob {
        handle: HANDLE,
    }

    #[allow(clippy::missing_errors_doc)]
    impl KillOnCloseJob {
        pub fn new() -> io::Result<Rc<Self>> {
            // SAFETY: null security/name create one unnamed job owned by this process.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Self { handle };
            // SAFETY: zeroed is valid for this documented POD Windows structure;
            // only the LimitFlags field is enabled before the OS reads it.
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let byte_count = u32::try_from(std::mem::size_of_val(&limits))
                .map_err(|_| invalid_input("job limits are too large"))?;
            // SAFETY: `job.handle` and the pointer are valid for this call and the
            // exact byte count describes the initialized structure above.
            let configured = unsafe {
                SetInformationJobObject(
                    job.handle,
                    JobObjectExtendedLimitInformation,
                    std::ptr::addr_of!(limits).cast(),
                    byte_count,
                )
            };
            if configured == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Rc::new(job))
        }

        /// Terminate only processes already proven to belong to this Job.
        pub fn terminate(&self, exit_code: u32) -> io::Result<()> {
            // SAFETY: `self.handle` is owned and remains live for this call.
            let terminated = unsafe { TerminateJobObject(self.handle, exit_code) };
            if terminated == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        /// Create an exact executable suspended, place it in this Job before it
        /// can execute user code, then resume its primary thread. The caller
        /// supplies the deliberately minimal environment as a bounded list;
        /// no shell parser or ambient `PATH` lookup participates.
        pub fn spawn_and_assign(
            self: &Rc<Self>,
            executable: Rc<RealFile>,
            arguments: &[OsString],
            environment: &[(OsString, OsString)],
            current_directory: Rc<RealDirectory>,
        ) -> io::Result<ManagedProcess> {
            current_directory.verify()?;
            executable.verify()?;
            let executable_wide = wide_null(executable.process_path().as_os_str())?;
            let mut command_line = command_line(executable.process_path().as_os_str(), arguments)?;
            let environment = unicode_environment_block(environment)?;
            let mut process = SuspendedProcess::create(
                &executable_wide,
                &mut command_line,
                &environment,
                &current_directory,
            )?;
            if let Err(error) = current_directory
                .verify()
                .and_then(|()| executable.verify())
            {
                return Err(with_cleanup(error, process.terminate_and_reap()));
            }
            let child_image = match query_process_image_path(process.process_handle())
                .and_then(|path| RealFile::capture(&path))
            {
                Ok(image) if image.identity() == executable.identity() => image,
                Ok(_) => {
                    return Err(with_cleanup(
                        invalid_data(
                            "suspended child image identity differs from bundled executable",
                        ),
                        process.terminate_and_reap(),
                    ));
                }
                Err(error) => return Err(with_cleanup(error, process.terminate_and_reap())),
            };
            if let Err(error) = child_image.verify() {
                return Err(with_cleanup(error, process.terminate_and_reap()));
            }
            // SAFETY: the returned process handle is live, suspended, and owned by
            // `process`; this Job handle remains live for the assignment.
            let assigned =
                unsafe { AssignProcessToJobObject(self.handle, process.process_handle()) };
            if assigned == 0 {
                let error = io::Error::last_os_error();
                return Err(with_cleanup(error, process.terminate_and_reap()));
            }
            if let Err(error) = process.resume() {
                return Err(with_cleanup(error, process.terminate_and_reap()));
            }
            if let Err(error) = current_directory
                .verify()
                .and_then(|()| executable.verify())
            {
                return Err(with_cleanup(error, process.terminate_and_reap()));
            }
            Ok(process.into_managed(Rc::clone(self), executable, current_directory))
        }
    }

    impl Drop for KillOnCloseJob {
        fn drop(&mut self) {
            // SAFETY: this is the one owned handle created by `new`; CloseHandle
            // consumes no Rust references and Windows applies kill-on-close.
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }

    /// Retained native process and primary-thread handles. It is intentionally
    /// not a PID wrapper: every wait or termination acts on this exact handle.
    #[derive(Debug)]
    pub struct ManagedProcess {
        process: HANDLE,
        primary_thread: HANDLE,
        process_id: u32,
        // Keep kill-on-close authority alive until exact child handles close.
        _job: Rc<KillOnCloseJob>,
        // Hold every locked path component for the lifetime of this child.
        _executable: Rc<RealFile>,
        _current_directory: Rc<RealDirectory>,
    }

    /// Private typestate: only this module creates it and only the Job-backed
    /// transition can turn it into a running `ManagedProcess`.
    struct SuspendedProcess {
        process: Option<HANDLE>,
        primary_thread: Option<HANDLE>,
        process_id: u32,
    }

    /// Explicit standard-handle policy for every persistent native child.
    /// The one inheritable `NUL` handle is also the complete handle allow-list,
    /// so enabling inheritance cannot leak ambient controller pipe endpoints.
    struct ChildStandardHandles {
        handles: Box<[HANDLE; 1]>,
        attribute_storage: Vec<usize>,
        attribute_list_initialized: bool,
    }

    impl ChildStandardHandles {
        fn create() -> io::Result<Self> {
            let null_path = wide_null(OsStr::new(r"\\.\NUL"))?;
            let security = SECURITY_ATTRIBUTES {
                nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>()).map_err(
                    |_| invalid_input("standard-handle security attributes are too large"),
                )?,
                lpSecurityDescriptor: std::ptr::null_mut(),
                bInheritHandle: 1,
            };
            // SAFETY: the retained device path is NUL terminated and `security`
            // explicitly creates the returned `NUL` handle as inheritable.
            let null_handle = unsafe {
                CreateFileW(
                    null_path.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::addr_of!(security),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    std::ptr::null_mut(),
                )
            };
            if null_handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let mut boundary = Self {
                handles: Box::new([null_handle]),
                attribute_storage: Vec::new(),
                attribute_list_initialized: false,
            };
            let mut handle_flags = 0_u32;
            // SAFETY: the returned `NUL` handle is retained and the flags output
            // points to initialized writable storage.
            if unsafe {
                GetHandleInformation(
                    boundary.standard_handle(),
                    std::ptr::addr_of_mut!(handle_flags),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            if handle_flags & HANDLE_FLAG_INHERIT == 0 {
                return Err(invalid_data(
                    "child standard handle is not explicitly inheritable",
                ));
            }
            let mut byte_count = 0_usize;
            // SAFETY: this is the documented sizing call for one attribute.
            let sized = unsafe {
                InitializeProcThreadAttributeList(
                    std::ptr::null_mut(),
                    1,
                    0,
                    std::ptr::addr_of_mut!(byte_count),
                )
            };
            let sizing_error = io::Error::last_os_error();
            if sized != 0 || byte_count == 0 {
                return Err(invalid_data(
                    "process handle allow-list sizing did not fail with a required size",
                ));
            }
            if sizing_error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER.cast_signed()) {
                return Err(sizing_error);
            }
            boundary.attribute_storage =
                vec![0_usize; byte_count.div_ceil(std::mem::size_of::<usize>())];
            // SAFETY: `attribute_storage` is aligned writable storage of the
            // exact size requested by the sizing call and remains stable.
            if unsafe {
                InitializeProcThreadAttributeList(
                    boundary.attribute_list(),
                    1,
                    0,
                    std::ptr::addr_of_mut!(byte_count),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            boundary.attribute_list_initialized = true;
            // SAFETY: the boxed handle array and initialized attribute list both
            // remain stable until `DeleteProcThreadAttributeList` in `Drop`.
            if unsafe {
                UpdateProcThreadAttribute(
                    boundary.attribute_list(),
                    0,
                    usize::try_from(PROC_THREAD_ATTRIBUTE_HANDLE_LIST)
                        .map_err(|_| invalid_input("process handle-list attribute is invalid"))?,
                    boundary.handles.as_ptr().cast(),
                    std::mem::size_of::<HANDLE>(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(boundary)
        }

        fn standard_handle(&self) -> HANDLE {
            self.handles[0]
        }

        fn attribute_list(&mut self) -> *mut core::ffi::c_void {
            self.attribute_storage.as_mut_ptr().cast()
        }
    }

    impl Drop for ChildStandardHandles {
        fn drop(&mut self) {
            if self.attribute_list_initialized {
                // SAFETY: this exact list was initialized successfully and its
                // backing allocation is retained until after this call.
                unsafe { DeleteProcThreadAttributeList(self.attribute_list()) };
            }
            // SAFETY: this is the one owned handle returned by `CreateFileW`.
            let _ = unsafe { CloseHandle(self.handles[0]) };
        }
    }

    impl SuspendedProcess {
        fn create(
            executable: &[u16],
            command_line: &mut [u16],
            environment: &[u16],
            current_directory: &RealDirectory,
        ) -> io::Result<Self> {
            let mut standard_handles = ChildStandardHandles::create()?;
            // SAFETY: zeroed is valid for the documented POD startup structures;
            // every field selected by `dwFlags` is set before Windows reads it.
            let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
            startup.StartupInfo.cb = u32::try_from(std::mem::size_of::<STARTUPINFOEXW>())
                .map_err(|_| invalid_input("startup information is too large"))?;
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.StartupInfo.hStdInput = standard_handles.standard_handle();
            startup.StartupInfo.hStdOutput = standard_handles.standard_handle();
            startup.StartupInfo.hStdError = standard_handles.standard_handle();
            startup.lpAttributeList = standard_handles.attribute_list();
            // SAFETY: zeroed is valid output storage for PROCESS_INFORMATION.
            let mut process_information: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
            let current_directory_wide = wide_null(current_directory.process_path().as_os_str())?;
            // SAFETY: all UTF-16 buffers are NUL terminated and retained through the
            // call; Windows initializes only `process_information`; handles are
            // retained in this private suspended typestate on success.
            let created = unsafe {
                CreateProcessW(
                    executable.as_ptr(),
                    command_line.as_mut_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    1,
                    CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
                    environment.as_ptr().cast(),
                    current_directory_wide.as_ptr(),
                    std::ptr::addr_of!(startup.StartupInfo),
                    std::ptr::addr_of_mut!(process_information),
                )
            };
            if created == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut process = Self {
                process: Some(process_information.hProcess),
                primary_thread: Some(process_information.hThread),
                process_id: process_information.dwProcessId,
            };
            if process.process_id == 0 {
                return Err(with_cleanup(
                    invalid_data("created process lacks a stable process ID"),
                    process.terminate_and_reap(),
                ));
            }
            Ok(process)
        }

        fn process_handle(&self) -> HANDLE {
            self.process.expect("suspended process handle is retained")
        }

        fn primary_thread_handle(&self) -> HANDLE {
            self.primary_thread
                .expect("suspended primary-thread handle is retained")
        }

        fn resume(&mut self) -> io::Result<()> {
            // SAFETY: `primary_thread` is owned, live, and created suspended.
            if unsafe { ResumeThread(self.primary_thread_handle()) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        fn terminate_and_reap(&mut self) -> io::Result<()> {
            // SAFETY: `process` is an owned live process handle. A suspended
            // process has not run arbitrary application code, but is still a
            // process that must be terminated and waited before handle release.
            let termination_error = (unsafe { TerminateProcess(self.process_handle(), 1) } == 0)
                .then(io::Error::last_os_error);
            // SAFETY: `process` is retained until this bounded wait returns.
            let waited = unsafe { WaitForSingleObject(self.process_handle(), 5_000) };
            if waited == WAIT_TIMEOUT {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "failed to reap suspended process within five seconds",
                ));
            }
            if waited == WAIT_FAILED {
                return Err(io::Error::last_os_error());
            }
            if waited != WAIT_OBJECT_0 {
                return Err(io::Error::other("unexpected suspended-process wait result"));
            }
            termination_error.map_or(Ok(()), Err)
        }

        fn into_managed(
            mut self,
            job: Rc<KillOnCloseJob>,
            executable: Rc<RealFile>,
            current_directory: Rc<RealDirectory>,
        ) -> ManagedProcess {
            ManagedProcess {
                process: self
                    .process
                    .take()
                    .expect("suspended process handle is retained"),
                primary_thread: self
                    .primary_thread
                    .take()
                    .expect("suspended primary-thread handle is retained"),
                process_id: self.process_id,
                _job: job,
                _executable: executable,
                _current_directory: current_directory,
            }
        }
    }

    impl Drop for SuspendedProcess {
        fn drop(&mut self) {
            // Every error path retains exact process authority. If Windows
            // cannot prove terminal completion, continuing could orphan a
            // suspended or briefly resumed child, so this boundary fail-stops.
            if let Some(process) = self.process.take() {
                // SAFETY: this exact process handle remains live and owned.
                let observed = unsafe { WaitForSingleObject(process, 0) };
                if observed == WAIT_FAILED {
                    std::process::abort();
                }
                if observed != WAIT_OBJECT_0 {
                    // SAFETY: terminate the exact retained child before any
                    // application authority can escape this typestate.
                    if unsafe { TerminateProcess(process, 1) } == 0 {
                        // A concurrent exit may race TerminateProcess. Only a
                        // signaled exact handle is accepted as cleanup proof.
                        if unsafe { WaitForSingleObject(process, 0) } != WAIT_OBJECT_0 {
                            std::process::abort();
                        }
                    } else if unsafe { WaitForSingleObject(process, 5_000) } != WAIT_OBJECT_0 {
                        std::process::abort();
                    }
                }
                // SAFETY: terminal completion was proven above.
                let _ = unsafe { CloseHandle(process) };
            }
            if let Some(thread) = self.primary_thread.take() {
                let _ = unsafe { CloseHandle(thread) };
            }
        }
    }

    #[allow(clippy::missing_errors_doc)]
    impl ManagedProcess {
        /// PID captured atomically in `PROCESS_INFORMATION` before the child
        /// was assigned or resumed. It is diagnostic; all authority remains on
        /// the retained exact process and Job handles.
        #[must_use]
        pub const fn process_id(&self) -> u32 {
            self.process_id
        }

        pub fn try_wait(&self) -> io::Result<Option<u32>> {
            // SAFETY: `process` is a live process handle owned by this instance.
            let waited = unsafe { WaitForSingleObject(self.process, 0) };
            if waited == WAIT_TIMEOUT {
                return Ok(None);
            }
            if waited != WAIT_OBJECT_0 {
                return Err(io::Error::last_os_error());
            }
            let mut exit_code = 0_u32;
            // SAFETY: `process` is live and `exit_code` is writable storage.
            if unsafe { GetExitCodeProcess(self.process, std::ptr::addr_of_mut!(exit_code)) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Some(exit_code))
        }

        pub fn terminate(&self, exit_code: u32) -> io::Result<()> {
            // SAFETY: `process` is a live process handle owned by this instance.
            if unsafe { TerminateProcess(self.process, exit_code) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    impl Drop for ManagedProcess {
        fn drop(&mut self) {
            // SAFETY: these are the two handles returned by CreateProcessW and
            // exclusively owned by this value; neither call invalidates Rust data.
            let _ = unsafe { CloseHandle(self.primary_thread) };
            let _ = unsafe { CloseHandle(self.process) };
        }
    }

    /// A verified supervisor process deliberately not assigned to a Job. It is
    /// retained only until authenticated readiness permits explicit detachment.
    #[derive(Debug)]
    pub struct OwnedProcess {
        process: Option<HANDLE>,
        primary_thread: Option<HANDLE>,
        process_id: u32,
        _executable: Rc<RealFile>,
        _current_directory: Rc<RealDirectory>,
        control_binding: AuthenticatedControlBinding,
        session_nonce: SessionNonce,
    }

    #[allow(clippy::missing_errors_doc, clippy::needless_pass_by_value)]
    impl OwnedProcess {
        pub fn launch(
            executable: Rc<RealFile>,
            arguments: &[OsString],
            environment: &[(OsString, OsString)],
            current_directory: Rc<RealDirectory>,
            control_binding: AuthenticatedControlBinding,
            session_nonce: SessionNonce,
        ) -> io::Result<Self> {
            executable.verify()?;
            current_directory.verify()?;
            let executable_wide = wide_null(executable.process_path().as_os_str())?;
            let mut command_line = command_line(executable.process_path().as_os_str(), arguments)?;
            let environment = unicode_environment_block(environment)?;
            let mut suspended = SuspendedProcess::create(
                &executable_wide,
                &mut command_line,
                &environment,
                &current_directory,
            )?;
            let image = query_process_image_path(suspended.process_handle())
                .and_then(|path| RealFile::capture(&path));
            if image
                .as_ref()
                .map_or(true, |image| image.identity() != executable.identity())
            {
                return Err(with_cleanup(
                    invalid_data("suspended supervisor image identity differs"),
                    suspended.terminate_and_reap(),
                ));
            }
            current_directory.verify()?;
            executable.verify()?;
            if let Err(error) = suspended.resume() {
                return Err(with_cleanup(error, suspended.terminate_and_reap()));
            }
            Ok(Self {
                process: suspended.process.take(),
                primary_thread: suspended.primary_thread.take(),
                process_id: suspended.process_id,
                _executable: executable,
                _current_directory: current_directory,
                control_binding,
                session_nonce,
            })
        }

        pub fn process_id(&self) -> io::Result<u32> {
            self.process
                .as_ref()
                .map(|_| self.process_id)
                .ok_or_else(|| invalid_data("supervisor was detached"))
        }

        pub fn try_wait(&self) -> io::Result<Option<u32>> {
            let handle = self
                .process
                .ok_or_else(|| invalid_data("supervisor was detached"))?;
            let waited = unsafe { WaitForSingleObject(handle, 0) };
            if waited == WAIT_TIMEOUT {
                return Ok(None);
            }
            if waited != WAIT_OBJECT_0 {
                return Err(io::Error::last_os_error());
            }
            let mut exit_code = 0_u32;
            if unsafe { GetExitCodeProcess(handle, &raw mut exit_code) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Some(exit_code))
        }

        pub fn terminate_and_reap(&mut self, exit_code: u32, deadline: Instant) -> io::Result<()> {
            let handle = self
                .process
                .ok_or_else(|| invalid_data("supervisor was detached"))?;
            // SAFETY: poll the retained exact process handle before requesting
            // termination; an already-signaled supervisor is terminal already.
            let initial = unsafe { WaitForSingleObject(handle, 0) };
            if initial == WAIT_FAILED {
                return Err(io::Error::last_os_error());
            }
            if initial != WAIT_OBJECT_0 && unsafe { TerminateProcess(handle, exit_code) } == 0 {
                // A concurrent natural exit may race termination; re-poll the
                // same exact handle before treating the failure as an error.
                if unsafe { WaitForSingleObject(handle, 0) } != WAIT_OBJECT_0 {
                    return Err(io::Error::last_os_error());
                }
            }
            let waited = unsafe { WaitForSingleObject(handle, remaining_wait_millis(deadline)) };
            if waited == WAIT_OBJECT_0 {
                return Ok(());
            }
            if waited == WAIT_TIMEOUT {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "supervisor reap deadline expired",
                ));
            }
            Err(io::Error::last_os_error())
        }

        pub fn detach_after_authenticated_readiness(
            &mut self,
            permit: AuthenticatedReadinessPermit,
        ) -> io::Result<()> {
            if permit.process_id != self.process_id()?
                || !constant_time_equal(&permit.nonce.0, &self.session_nonce.0)
                || !constant_time_equal(&permit.binding.0, &self.control_binding.0)
            {
                return Err(invalid_data("authenticated readiness binding differs"));
            }
            if self.try_wait()?.is_some() {
                return Err(invalid_data(
                    "authenticated supervisor exited before handle detachment",
                ));
            }
            if let Some(thread) = self.primary_thread.take() {
                let _ = unsafe { CloseHandle(thread) };
            }
            if let Some(process) = self.process.take() {
                let _ = unsafe { CloseHandle(process) };
            }
            Ok(())
        }
    }

    impl Drop for OwnedProcess {
        fn drop(&mut self) {
            if let Some(process) = self.process.take() {
                // SAFETY: implicit ownership remains responsible for this exact
                // supervisor. First distinguish a naturally exited process so
                // normal cleanup never treats ACCESS_DENIED as an orphan.
                let observed = unsafe { WaitForSingleObject(process, 0) };
                if observed == WAIT_FAILED {
                    std::process::abort();
                }
                if observed != WAIT_OBJECT_0 {
                    // SAFETY: this exact retained process handle is still live.
                    if unsafe { TerminateProcess(process, 1) } == 0 {
                        // A concurrent natural exit can make TerminateProcess
                        // fail; one exact-handle replay distinguishes that case.
                        if unsafe { WaitForSingleObject(process, 0) } != WAIT_OBJECT_0 {
                            std::process::abort();
                        }
                    } else if unsafe { WaitForSingleObject(process, 5_000) } != WAIT_OBJECT_0 {
                        std::process::abort();
                    }
                }
                // SAFETY: terminal completion is proven for this exact handle.
                let _ = unsafe { CloseHandle(process) };
            }
            if let Some(thread) = self.primary_thread.take() {
                let _ = unsafe { CloseHandle(thread) };
            }
        }
    }

    fn process_compatible_path(path: &Path, handle: HANDLE) -> io::Result<PathBuf> {
        let process_path = if local_dos_path(path) {
            path.to_path_buf()
        } else {
            final_dos_path(handle)?
        };
        if !process_path.is_absolute() || !local_dos_path(&process_path) {
            return Err(invalid_data(
                "process path is not one explicit local DOS-volume spelling",
            ));
        }
        Ok(process_path)
    }

    fn local_dos_path(path: &Path) -> bool {
        matches!(
            path.components().next(),
            Some(std::path::Component::Prefix(prefix))
                if matches!(
                    prefix.kind(),
                    std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)
                )
        )
    }

    fn replay_locked_path(
        path: &Path,
        final_is_directory: bool,
        retained: &[LockedPathComponent],
        expected_identity: PathIdentity,
        mismatch: &str,
    ) -> io::Result<()> {
        let replayed = lock_path_components(path, final_is_directory)?;
        let exact_components = replayed.len() == retained.len()
            && replayed.iter().zip(retained).all(|(replayed, retained)| {
                replayed.identity == retained.identity
                    && replayed.normalized_path == retained.normalized_path
            });
        if !exact_components
            || replayed
                .last()
                .is_none_or(|component| component.identity != expected_identity)
        {
            return Err(invalid_data(mismatch));
        }
        Ok(())
    }

    fn lock_path_components(
        path: &Path,
        final_is_directory: bool,
    ) -> io::Result<Vec<LockedPathComponent>> {
        let mut ancestors = path.ancestors().collect::<Vec<_>>();
        ancestors.reverse();
        let mut components = Vec::with_capacity(ancestors.len());
        for ancestor in ancestors {
            if ancestor.as_os_str().is_empty() {
                continue;
            }
            let directory = ancestor != path || final_is_directory;
            let (handle, identity, attributes, links) = open_path(ancestor, directory)?;
            if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                // SAFETY: the just-opened no-follow handle is uniquely owned here.
                let _ = unsafe { CloseHandle(handle) };
                return Err(invalid_data("path component is a reparse point"));
            }
            if links != 1 {
                // SAFETY: the just-opened no-follow handle is uniquely owned here.
                let _ = unsafe { CloseHandle(handle) };
                return Err(invalid_data("path component has a hard-link alias"));
            }
            let normalized_path = match final_volume_path(handle) {
                Ok(path) => path,
                Err(error) => {
                    // SAFETY: the just-opened no-follow handle is uniquely owned here.
                    let _ = unsafe { CloseHandle(handle) };
                    return Err(error);
                }
            };
            components.push(LockedPathComponent {
                identity,
                normalized_path,
                handle,
            });
        }
        Ok(components)
    }

    fn open_path(path: &Path, directory: bool) -> io::Result<(HANDLE, PathIdentity, u32, u32)> {
        let encoded = wide_null(path.as_os_str())?;
        let mut flags = FILE_FLAG_OPEN_REPARSE_POINT;
        if directory {
            flags |= FILE_FLAG_BACKUP_SEMANTICS;
        }
        // SAFETY: the path is NUL-terminated and all optional pointers are null;
        // the returned handle is closed on every path below.
        let handle = unsafe {
            CreateFileW(
                encoded.as_ptr(),
                FILE_READ_ATTRIBUTES,
                // Do not grant delete sharing: the retained authority handle
                // prevents final-component replacement through this transition.
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                flags,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let identity = match file_identity(handle) {
            Ok(identity) => identity,
            Err(error) => return close_open_error(handle, error),
        };
        let attributes = match file_attributes(handle) {
            Ok(attributes) => attributes,
            Err(error) => return close_open_error(handle, error),
        };
        let links = match file_link_count(handle) {
            Ok(links) => links,
            Err(error) => return close_open_error(handle, error),
        };
        if directory && attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            // SAFETY: this exactly closes the handle created above after its final use.
            let _ = unsafe { CloseHandle(handle) };
            return Err(invalid_input("path is not a directory"));
        }
        Ok((handle, identity, attributes, links))
    }

    fn file_identity(handle: HANDLE) -> io::Result<PathIdentity> {
        let mut information = FILE_ID_INFO::default();
        let size = u32::try_from(std::mem::size_of::<FILE_ID_INFO>())
            .map_err(|_| invalid_input("file identity buffer is too large"))?;
        // SAFETY: handle is retained and `information` is exact writable output storage.
        if unsafe {
            GetFileInformationByHandleEx(
                handle,
                FileIdInfo,
                std::ptr::addr_of_mut!(information).cast(),
                size,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if information.FileId.Identifier == [0; 16] {
            return Err(invalid_data("filesystem returned an unusable zero file ID"));
        }
        Ok(PathIdentity {
            volume_serial_number: information.VolumeSerialNumber,
            file_id: information.FileId.Identifier,
        })
    }

    fn close_open_error<T>(handle: HANDLE, error: io::Error) -> io::Result<T> {
        // SAFETY: `handle` was created in `open_path` and has not escaped on this path.
        let _ = unsafe { CloseHandle(handle) };
        Err(error)
    }

    fn file_attributes(handle: HANDLE) -> io::Result<u32> {
        let mut information = FILE_ATTRIBUTE_TAG_INFO::default();
        let size = u32::try_from(std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>())
            .map_err(|_| invalid_input("file attribute buffer is too large"))?;
        // SAFETY: handle is retained and `information` is exact writable output storage.
        if unsafe {
            GetFileInformationByHandleEx(
                handle,
                FileAttributeTagInfo,
                std::ptr::addr_of_mut!(information).cast(),
                size,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if information.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || information.ReparseTag != 0
        {
            return Err(invalid_data("opened path component has reparse metadata"));
        }
        Ok(information.FileAttributes)
    }

    fn file_link_count(handle: HANDLE) -> io::Result<u32> {
        let mut information = FILE_STANDARD_INFO::default();
        let size = u32::try_from(std::mem::size_of::<FILE_STANDARD_INFO>())
            .map_err(|_| invalid_input("file standard buffer is too large"))?;
        // SAFETY: handle is retained and `information` is exact writable output storage.
        if unsafe {
            GetFileInformationByHandleEx(
                handle,
                FileStandardInfo,
                std::ptr::addr_of_mut!(information).cast(),
                size,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(information.NumberOfLinks)
    }

    fn final_volume_path(handle: HANDLE) -> io::Result<PathBuf> {
        let path = final_normalized_path(handle, VOLUME_NAME_GUID)?;
        let rendered = path.to_string_lossy();
        if !rendered.starts_with("\\\\?\\Volume{") || !rendered.contains("}\\") {
            return Err(invalid_data(
                "path is not one local GUID-volume namespace; UNC and device paths are unsupported",
            ));
        }
        Ok(path)
    }

    fn final_dos_path(handle: HANDLE) -> io::Result<PathBuf> {
        const VOLUME_NAME_DOS: u32 = 0;
        let path = final_normalized_path(handle, VOLUME_NAME_DOS)?;
        if !local_dos_path(&path) {
            return Err(invalid_data(
                "path has no supported local DOS-volume process spelling",
            ));
        }
        Ok(path)
    }

    fn final_normalized_path(handle: HANDLE, volume_name: u32) -> io::Result<PathBuf> {
        // SAFETY: the handle remains live and the zero-size query asks Windows
        // only for the required UTF-16 length.
        let required = unsafe {
            GetFinalPathNameByHandleW(
                handle,
                std::ptr::null_mut(),
                0,
                FILE_NAME_NORMALIZED | volume_name,
            )
        };
        if required == 0 {
            return Err(io::Error::last_os_error());
        }
        if required > 32_767 {
            return Err(invalid_input(
                "final Windows path exceeds 32,767 UTF-16 units",
            ));
        }
        let capacity = usize::try_from(required)
            .map_err(|_| invalid_input("final Windows path is too long"))?
            + 1;
        let mut buffer = vec![0_u16; capacity];
        // SAFETY: the retained handle is valid and `buffer` has room for the
        // documented required length plus a NUL terminator.
        let written = unsafe {
            GetFinalPathNameByHandleW(
                handle,
                buffer.as_mut_ptr(),
                u32::try_from(buffer.len())
                    .map_err(|_| invalid_input("final Windows path is too long"))?,
                FILE_NAME_NORMALIZED | volume_name,
            )
        };
        if written == 0 || usize::try_from(written).unwrap_or(usize::MAX) >= buffer.len() {
            return Err(io::Error::last_os_error());
        }
        buffer.truncate(usize::try_from(written).map_err(|_| invalid_input("final path length"))?);
        Ok(PathBuf::from(OsString::from_wide(&buffer)))
    }

    fn query_process_image_path(process: HANDLE) -> io::Result<PathBuf> {
        let mut buffer = vec![0_u16; 32_767];
        let mut length = u32::try_from(buffer.len())
            .map_err(|_| invalid_input("process image path buffer is too large"))?;
        // SAFETY: `process` is retained and queryable; `buffer` is writable UTF-16
        // storage whose capacity is passed truthfully through `length`.
        if unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &raw mut length) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        let length =
            usize::try_from(length).map_err(|_| invalid_data("process image path length"))?;
        if length == 0 || length >= buffer.len() {
            return Err(invalid_data("process image path is invalid or truncated"));
        }
        buffer.truncate(length);
        Ok(PathBuf::from(OsString::from_wide(&buffer)))
    }

    fn with_cleanup(original: io::Error, cleanup: io::Result<()>) -> io::Error {
        match cleanup {
            Ok(()) => original,
            Err(cleanup) => io::Error::new(
                original.kind(),
                format!("{original}; cleanup was not proven: {cleanup}"),
            ),
        }
    }

    fn wide_null(value: &OsStr) -> io::Result<Vec<u16>> {
        let units = value.encode_wide().collect::<Vec<_>>();
        if units.contains(&0) {
            return Err(invalid_input("Windows path contains an embedded NUL"));
        }
        Ok(units.into_iter().chain(std::iter::once(0)).collect())
    }

    pub(crate) fn command_line(executable: &OsStr, arguments: &[OsString]) -> io::Result<Vec<u16>> {
        let mut output = Vec::new();
        append_quoted_argument(&mut output, executable)?;
        for argument in arguments {
            output.push(u16::from(b' '));
            append_quoted_argument(&mut output, argument)?;
        }
        output.push(0);
        if output.len() > 32_767 {
            return Err(invalid_input(
                "Windows command line exceeds 32,767 UTF-16 units",
            ));
        }
        Ok(output)
    }

    fn append_quoted_argument(output: &mut Vec<u16>, argument: &OsStr) -> io::Result<()> {
        let units = argument.encode_wide().collect::<Vec<_>>();
        if units.is_empty() || units.contains(&0) {
            return Err(invalid_input("child argument is empty or contains NUL"));
        }
        let needs_quotes = units.iter().any(|unit| {
            *unit == u16::from(b' ') || *unit == u16::from(b'\t') || *unit == u16::from(b'\"')
        });
        if !needs_quotes {
            output.extend(units);
            return Ok(());
        }
        output.push(u16::from(b'\"'));
        let mut backslashes = 0_usize;
        for unit in units {
            if unit == u16::from(b'\\') {
                backslashes += 1;
            } else if unit == u16::from(b'\"') {
                output.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes * 2 + 1));
                output.push(unit);
                backslashes = 0;
            } else {
                output.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes));
                output.push(unit);
                backslashes = 0;
            }
        }
        output.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes * 2));
        output.push(u16::from(b'\"'));
        Ok(())
    }

    pub(crate) fn unicode_environment_block(
        environment: &[(OsString, OsString)],
    ) -> io::Result<Vec<u16>> {
        if environment.is_empty() || environment.len() > 16 {
            return Err(invalid_input(
                "child environment must contain 1..=16 entries",
            ));
        }
        let mut entries = environment
            .iter()
            .map(|(key, value)| {
                let key = key.encode_wide().collect::<Vec<_>>();
                let value = value.encode_wide().collect::<Vec<_>>();
                if key.is_empty()
                    || key
                        .iter()
                        .any(|unit| *unit == 0 || *unit == u16::from(b'='))
                    || value.contains(&0)
                    || !key.iter().all(|unit| {
                        (*unit >= u16::from(b'A') && *unit <= u16::from(b'Z'))
                            || (*unit >= u16::from(b'a') && *unit <= u16::from(b'z'))
                            || (*unit >= u16::from(b'0') && *unit <= u16::from(b'9'))
                            || *unit == u16::from(b'_')
                    })
                {
                    return Err(invalid_input("child environment entry is unsafe"));
                }
                Ok((key, value))
            })
            .collect::<io::Result<Vec<_>>>()?;
        entries.sort_by_key(|(key, _)| {
            key.iter()
                .map(|unit| ascii_uppercase(*unit))
                .collect::<Vec<_>>()
        });
        if entries
            .windows(2)
            .any(|pair| ascii_utf16_equal_ignore_case(&pair[0].0, &pair[1].0))
        {
            return Err(invalid_input("child environment has duplicate keys"));
        }
        if !entries
            .iter()
            .any(|(key, _)| ascii_utf16_equal_ignore_case(key, &SYSTEM_ROOT))
        {
            return Err(invalid_input("child environment lacks SystemRoot"));
        }
        let mut block = Vec::new();
        for (key, value) in entries {
            block.extend(key);
            block.push(u16::from(b'='));
            block.extend(value);
            block.push(0);
        }
        block.push(0);
        Ok(block)
    }

    const fn ascii_uppercase(value: u16) -> u16 {
        if value >= 97 && value <= 122 {
            value - 32
        } else {
            value
        }
    }

    fn ascii_utf16_equal_ignore_case(left: &[u16], right: &[u16]) -> bool {
        left.len() == right.len()
            && left
                .iter()
                .zip(right)
                .all(|(left, right)| ascii_uppercase(*left) == ascii_uppercase(*right))
    }

    fn invalid_data(message: &str) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, message)
    }

    fn invalid_input(message: &str) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidInput, message)
    }

    #[cfg(test)]
    mod tests {
        use super::{
            CapabilityToken, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, ERROR_SEM_TIMEOUT,
            MAXIMUM_PIPE_PAYLOAD_BYTES, PathIdentity, SessionNonce, constant_time_equal,
            exact_sid_extent, finite_wait_millis, pipe_client_wait_for_duration,
            remaining_wait_millis, response_ack, retryable_pipe_wait_error,
            validate_pipe_payload_length, validate_private_component,
            windows_workspace_filesystem_identity_sha256,
        };
        use std::time::{Duration, Instant};

        #[test]
        fn workspace_identity_digest_uses_all_128_file_id_bits() {
            let baseline = PathIdentity {
                volume_serial_number: 7,
                file_id: [0; 16],
            };
            let mut high_byte_changed = baseline;
            high_byte_changed.file_id[15] = 1;
            assert_ne!(
                windows_workspace_filesystem_identity_sha256(baseline),
                windows_workspace_filesystem_identity_sha256(high_byte_changed),
                "a high FileId byte must be authority, not a lossy display projection"
            );
        }

        #[test]
        fn private_runtime_components_are_single_safe_names() {
            assert!(validate_private_component("session-a1B2-3").is_ok());
            for value in [
                "",
                "../escape",
                "child\\escape",
                "child/escape",
                "name space",
                "session.json",
                "stream:name",
                "trailing.",
                "CON",
            ] {
                assert!(
                    validate_private_component(value).is_err(),
                    "{value} must fail"
                );
            }
        }

        #[test]
        fn protected_record_tokens_require_one_canonical_lowercase_encoding() {
            let capability_text = "0123456789abcdef".repeat(4);
            let capability = CapabilityToken::parse_lowercase_hex(&capability_text)
                .expect("canonical capability");
            assert_eq!(capability.lowercase_hex(), capability_text);
            let nonce = SessionNonce::parse_lowercase_hex(&"ab".repeat(32))
                .expect("canonical session nonce");
            assert_eq!(nonce.lowercase_hex(), "ab".repeat(32));
            for invalid in [
                "a".repeat(63),
                "A0".repeat(32),
                "g0".repeat(32),
                format!("{} ", "00".repeat(32)),
            ] {
                assert!(CapabilityToken::parse_lowercase_hex(&invalid).is_err());
                assert!(SessionNonce::parse_lowercase_hex(&invalid).is_err());
            }
        }

        #[test]
        fn capability_is_csprng_sized_lowercase_hex_and_exactly_compared() {
            let capability = CapabilityToken::generate().expect("system CSPRNG capability");
            let rendered = capability.lowercase_hex();
            assert_eq!(rendered.len(), 64);
            assert!(
                rendered
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            );
            assert!(constant_time_equal(
                rendered.as_bytes(),
                rendered.as_bytes()
            ));
            let mut altered = rendered.into_bytes();
            altered[0] = if altered[0] == b'a' { b'b' } else { b'a' };
            assert!(!constant_time_equal(
                capability.lowercase_hex().as_bytes(),
                &altered
            ));
        }

        #[test]
        fn framed_payload_lengths_reject_empty_and_oversize_values() {
            assert!(validate_pipe_payload_length(0).is_err());
            assert_eq!(
                validate_pipe_payload_length(
                    u32::try_from(MAXIMUM_PIPE_PAYLOAD_BYTES).expect("bound fits u32")
                )
                .expect("maximum is accepted"),
                MAXIMUM_PIPE_PAYLOAD_BYTES
            );
            assert!(
                validate_pipe_payload_length(
                    u32::try_from(MAXIMUM_PIPE_PAYLOAD_BYTES + 1).expect("bound fits u32")
                )
                .is_err()
            );
        }

        #[test]
        fn waits_never_become_infinite_and_expired_deadlines_poll() {
            assert_eq!(
                remaining_wait_millis(
                    Instant::now()
                        .checked_sub(Duration::from_millis(1))
                        .expect("test instant supports one-millisecond subtraction"),
                ),
                0
            );
            assert_eq!(
                finite_wait_millis(Duration::from_secs(u64::MAX)),
                u32::MAX - 1
            );
            assert_eq!(pipe_client_wait_for_duration(Duration::from_nanos(1)), 1);
            assert_eq!(
                pipe_client_wait_for_duration(Duration::from_secs(u64::MAX)),
                u32::MAX - 1
            );
            assert!(retryable_pipe_wait_error(Some(
                ERROR_SEM_TIMEOUT.cast_signed()
            )));
            assert!(retryable_pipe_wait_error(Some(
                ERROR_FILE_NOT_FOUND.cast_signed()
            )));
            assert!(retryable_pipe_wait_error(Some(
                ERROR_PIPE_BUSY.cast_signed()
            )));
            assert!(!retryable_pipe_wait_error(Some(5)));
        }

        #[test]
        fn response_ack_is_domain_separated_and_payload_sensitive() {
            assert_ne!(response_ack(b"one"), response_ack(b"two"));
            assert_ne!(response_ack(b"response"), response_ack(b"response\0"));
        }

        #[test]
        fn sid_extent_requires_one_exact_bounded_sid() {
            assert_eq!(
                exact_sid_extent(100, 120, 104, 2).expect("exact SID extent"),
                (16, 120)
            );
            assert!(exact_sid_extent(100, 119, 104, 2).is_err());
            assert!(exact_sid_extent(100, 124, 104, 2).is_err());
            assert!(exact_sid_extent(100, 120, 99, 2).is_err());
        }

        #[test]
        fn debug_output_redacts_capability() {
            let capability = CapabilityToken([0x5a; 32]);
            let capability_debug = format!("{capability:?}");
            assert!(!capability_debug.contains("5a"));
            let nonce = SessionNonce([0x5a; 32]);
            let nonce_debug = format!("{nonce:?}");
            assert!(!nonce_debug.contains("5a"));
        }
    }
}

#[cfg(windows)]
pub use native::*;

#[cfg(all(test, windows))]
mod tests {
    use std::{
        env,
        ffi::OsString,
        fs, io,
        os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle},
        path::Path,
        process::Command,
        rc::Rc,
        sync::mpsc,
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use super::native::{
        AuthenticatedPipeClient, AuthenticatedPipeServer, CapabilityToken, CurrentUser,
        KillOnCloseJob, PrivateRuntimeDirectory, RealDirectory, RealFile, command_line,
        delete_file_exact, file_identity_from_open_file, private_lock_is_contended, response_ack,
        system_root_directory, unicode_environment_block,
    };
    use tempfile::TempDir;
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, ERROR_BROKEN_PIPE, GENERIC_READ, GENERIC_WRITE, HANDLE,
            HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
        },
        Security::SECURITY_ATTRIBUTES,
        Storage::FileSystem::{CreateFileW, OPEN_EXISTING, ReadFile, WriteFile},
        System::{
            Console::{GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle},
            Pipes::CreatePipe,
        },
    };

    const REDIRECTED_CHILD_TEST_ENVIRONMENT: &str = "MIGRATION_STREAM_NATIVE_REDIRECTED_CHILD_TEST";

    struct TestPipe {
        read: Option<OwnedHandle>,
        write: Option<OwnedHandle>,
    }

    impl TestPipe {
        fn inheritable_writer() -> io::Result<Self> {
            let security = SECURITY_ATTRIBUTES {
                nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>())
                    .map_err(|_| io::Error::other("test pipe security attributes are too large"))?,
                lpSecurityDescriptor: std::ptr::null_mut(),
                bInheritHandle: 1,
            };
            let mut read = std::ptr::null_mut();
            let mut write = std::ptr::null_mut();
            // SAFETY: both output pointers are writable and `security` remains
            // valid for this synchronous anonymous-pipe creation call.
            if unsafe {
                CreatePipe(
                    std::ptr::addr_of_mut!(read),
                    std::ptr::addr_of_mut!(write),
                    std::ptr::addr_of!(security),
                    0,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: successful `CreatePipe` returned two independently owned
            // handles; each is transferred into exactly one `OwnedHandle`.
            let read = unsafe { OwnedHandle::from_raw_handle(read) };
            let write = unsafe { OwnedHandle::from_raw_handle(write) };
            // SAFETY: only the writer is intended to be inheritable. The retained
            // read handle remains valid through this metadata update.
            if unsafe { SetHandleInformation(read.as_raw_handle(), HANDLE_FLAG_INHERIT, 0) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                read: Some(read),
                write: Some(write),
            })
        }

        fn write_handle(&self) -> HANDLE {
            self.write
                .as_ref()
                .expect("test pipe writer is retained")
                .as_raw_handle()
        }

        fn close_writer(&mut self) {
            drop(self.write.take());
        }

        fn take_reader(&mut self) -> OwnedHandle {
            self.read.take().expect("test pipe reader is retained")
        }
    }

    struct RedirectedTestStreams {
        stdout: TestPipe,
        stderr: TestPipe,
        original_stdout: HANDLE,
        original_stderr: HANDLE,
        installed: bool,
    }

    impl RedirectedTestStreams {
        fn install() -> io::Result<Self> {
            let stdout = TestPipe::inheritable_writer()?;
            let stderr = TestPipe::inheritable_writer()?;
            // SAFETY: these calls borrow the process's current standard-handle
            // values; the test never closes the returned original handles.
            let original_stdout = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
            let original_stderr = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
            let mut streams = Self {
                stdout,
                stderr,
                original_stdout,
                original_stderr,
                installed: false,
            };
            // SAFETY: both pipe writers remain owned by `streams` until after
            // the original process standard handles have been restored.
            if unsafe { SetStdHandle(STD_OUTPUT_HANDLE, streams.stdout.write_handle()) } == 0 {
                return Err(io::Error::last_os_error());
            }
            streams.installed = true;
            if unsafe { SetStdHandle(STD_ERROR_HANDLE, streams.stderr.write_handle()) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(streams)
        }

        fn restore(&mut self) -> io::Result<()> {
            if !self.installed {
                return Ok(());
            }
            // SAFETY: the saved values are borrowed from this same process and
            // remain owned by their original runtime for the test's lifetime.
            let stdout_restored = unsafe { SetStdHandle(STD_OUTPUT_HANDLE, self.original_stdout) };
            let stdout_error = (stdout_restored == 0).then(io::Error::last_os_error);
            // SAFETY: same ownership argument as the stdout restoration above.
            let stderr_restored = unsafe { SetStdHandle(STD_ERROR_HANDLE, self.original_stderr) };
            let stderr_error = (stderr_restored == 0).then(io::Error::last_os_error);
            if stdout_restored != 0 && stderr_restored != 0 {
                self.installed = false;
            }
            stdout_error.or(stderr_error).map_or(Ok(()), Err)
        }

        fn restore_and_take_readers(mut self) -> io::Result<(OwnedHandle, OwnedHandle)> {
            self.restore()?;
            self.stdout.close_writer();
            self.stderr.close_writer();
            Ok((self.stdout.take_reader(), self.stderr.take_reader()))
        }
    }

    impl Drop for RedirectedTestStreams {
        fn drop(&mut self) {
            if self.restore().is_err() {
                // A failed restoration would leave process-global handles
                // pointing at writers about to close, so the isolated test
                // helper cannot continue safely.
                std::process::abort();
            }
        }
    }

    fn read_test_pipe_to_eof(handle: &OwnedHandle) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0_u8; 1024];
            let mut read = 0_u32;
            // SAFETY: `handle` owns the pipe reader and `chunk` is writable for
            // exactly its declared size throughout this synchronous call.
            if unsafe {
                ReadFile(
                    handle.as_raw_handle(),
                    chunk.as_mut_ptr(),
                    u32::try_from(chunk.len())
                        .map_err(|_| io::Error::other("test pipe chunk is too large"))?,
                    std::ptr::addr_of_mut!(read),
                    std::ptr::null_mut(),
                )
            } == 0
            {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_BROKEN_PIPE.cast_signed()) {
                    return Ok(bytes);
                }
                return Err(error);
            }
            if read == 0 {
                return Ok(bytes);
            }
            let read = usize::try_from(read)
                .map_err(|_| io::Error::other("test pipe read length is invalid"))?;
            bytes.extend_from_slice(&chunk[..read]);
            if bytes.len() > 64 * 1024 {
                return Err(io::Error::other("test child emitted excessive output"));
            }
        }
    }

    fn test_component(label: &str) -> String {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("monotonic test wall clock")
            .as_nanos();
        format!("{label}-{nonce}")
    }

    struct PrivateTestRoot {
        directory: PrivateRuntimeDirectory,
        parent: Rc<RealDirectory>,
        user: Rc<CurrentUser>,
        _temporary: TempDir,
    }

    fn private_test_root() -> PrivateTestRoot {
        let temporary = tempfile::tempdir().expect("private test root");
        let parent = Rc::new(
            RealDirectory::capture(temporary.path()).expect("retained private test parent"),
        );
        let user = CurrentUser::capture().expect("current SID");
        let directory = PrivateRuntimeDirectory::open_or_create_child(
            Rc::clone(&parent),
            Rc::clone(&user),
            "private-runtime",
        )
        .expect("protected private child");
        PrivateTestRoot {
            directory,
            parent,
            user,
            _temporary: temporary,
        }
    }

    #[test]
    fn exact_file_delete_rejects_a_same_length_replacement() {
        let temporary = tempfile::tempdir().expect("exact-delete parent");
        let path = temporary.path().join("owned.bin");
        let displaced = temporary.path().join("displaced.bin");
        fs::write(&path, b"original").expect("write owned file");
        let retained = fs::File::open(&path).expect("open owned file");
        let identity = file_identity_from_open_file(&retained).expect("capture owned FileId");
        drop(retained);

        fs::rename(&path, &displaced).expect("displace owned file");
        fs::write(&path, b"replaced").expect("write same-length foreign file");
        assert_eq!(
            fs::metadata(&path).expect("foreign metadata").len(),
            fs::metadata(&displaced).expect("owned metadata").len()
        );
        assert!(delete_file_exact(&path, identity).is_err());
        assert_eq!(fs::read(&path).expect("foreign file remains"), b"replaced");

        delete_file_exact(&displaced, identity).expect("delete exact displaced file");
        assert!(!displaced.exists());
    }

    fn pipe_path(component: &str) -> Vec<u16> {
        format!(r"\\.\pipe\MigrationStream-{component}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect()
    }

    fn open_test_client(component: &str) -> io::Result<HANDLE> {
        let path = pipe_path(component);
        for _ in 0..100 {
            // SAFETY: `path` is a retained NUL-terminated local pipe name;
            // the returned client handle is closed by the test thread.
            let handle = unsafe {
                CreateFileW(
                    path.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    0,
                    std::ptr::null_mut(),
                )
            };
            if handle != INVALID_HANDLE_VALUE {
                return Ok(handle);
            }
            thread::sleep(Duration::from_millis(10));
        }
        Err(io::Error::last_os_error())
    }

    fn write_fragmented(handle: HANDLE, bytes: &[u8]) -> io::Result<()> {
        for byte in bytes {
            let mut written = 0_u32;
            // SAFETY: each one-byte source remains valid through synchronous
            // WriteFile; the test client owns `handle`.
            if unsafe { WriteFile(handle, byte, 1, &raw mut written, std::ptr::null_mut()) } == 0
                || written != 1
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    fn read_exact_client(handle: HANDLE, bytes: &mut [u8]) -> io::Result<()> {
        let mut offset = 0_usize;
        while offset < bytes.len() {
            let mut read = 0_u32;
            // SAFETY: destination storage is valid and the test client owns its
            // synchronous pipe handle for this read.
            if unsafe {
                ReadFile(
                    handle,
                    bytes[offset..].as_mut_ptr(),
                    u32::try_from(bytes.len() - offset)
                        .map_err(|_| io::Error::other("test read too large"))?,
                    &raw mut read,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "test pipe EOF",
                ));
            }
            offset += usize::try_from(read).map_err(|_| io::Error::other("test read invalid"))?;
        }
        Ok(())
    }

    fn close_test_handle(handle: HANDLE) {
        // SAFETY: this helper is called once for each successful CreateFileW.
        let _ = unsafe { CloseHandle(handle) };
    }

    fn client_request_then_optional_response_ack(
        component: &str,
        capability: &[u8],
        acknowledgement: Option<[u8; 32]>,
    ) -> io::Result<()> {
        let handle = open_test_client(component)?;
        let result = (|| {
            write_fragmented(handle, &1_u32.to_be_bytes())?;
            write_fragmented(handle, capability)?;
            write_fragmented(handle, b"x")?;
            let mut response_length = [0_u8; 4];
            read_exact_client(handle, &mut response_length)?;
            let mut response = vec![
                0_u8;
                usize::try_from(u32::from_be_bytes(response_length))
                    .map_err(|_| io::Error::other("response length"))?
            ];
            read_exact_client(handle, &mut response)?;
            if response != b"ok" {
                return Err(io::Error::other("response mismatch"));
            }
            if let Some(acknowledgement) = acknowledgement {
                write_fragmented(handle, &acknowledgement)?;
            }
            Ok(())
        })();
        close_test_handle(handle);
        result
    }

    #[test]
    fn captured_directory_replays_its_handle_derived_identity() {
        let current = std::env::current_dir().expect("current directory");
        let directory = RealDirectory::capture(&current).expect("real current directory");
        directory.verify().expect("unchanged directory identity");
        assert_ne!(directory.identity().file_id(), [0; 16]);
    }

    #[test]
    fn direct_command_line_quotes_shell_metacharacters_as_literal_arguments() {
        let arguments = [
            OsString::from("space value"),
            OsString::from("quote\"value"),
            OsString::from("&|<>()^%"),
        ];
        let encoded = command_line(
            OsString::from(r"C:\bundle\bin\workbench.exe").as_os_str(),
            &arguments,
        )
        .expect("direct command line");
        let rendered = String::from_utf16(&encoded[..encoded.len() - 1]).expect("UTF-16");
        assert!(rendered.contains("\"space value\""));
        assert!(rendered.contains("\"quote\\\"value\""));
        assert!(rendered.contains("&|<>()^%"));
    }

    #[test]
    fn minimal_environment_requires_one_unambiguous_system_root() {
        let accepted = unicode_environment_block(&[
            (OsString::from("SystemRoot"), OsString::from(r"C:\Windows")),
            (OsString::from("TEMP"), OsString::from(r"C:\runtime\tmp")),
        ]);
        assert!(accepted.is_ok());
        let duplicate = unicode_environment_block(&[
            (OsString::from("SystemRoot"), OsString::from(r"C:\Windows")),
            (OsString::from("systemroot"), OsString::from(r"C:\Windows")),
        ]);
        assert!(duplicate.is_err());
    }

    #[test]
    fn relative_directories_are_not_authority() {
        assert!(RealDirectory::capture(Path::new("relative-workspace")).is_err());
    }

    #[test]
    fn private_directory_dacl_reopens_exactly_for_the_current_sid() {
        let root = private_test_root();
        let original = root.directory.directory().identity();
        let reopened = PrivateRuntimeDirectory::open_or_create_child(
            Rc::clone(&root.parent),
            Rc::clone(&root.user),
            "private-runtime",
        )
        .expect("reopened protected private child");
        reopened
            .directory()
            .verify()
            .expect("reopened directory replay");
        assert_eq!(reopened.directory().identity(), original);
    }

    #[test]
    fn ambient_descriptor_state_is_rejected_without_mutating_bytes() {
        let root = private_test_root();
        let path = root.directory.directory().path().join("ambient-state");
        fs::write(&path, b"foreign").expect("ambient state fixture");
        assert!(
            root.directory.read_state_file("ambient-state", 64).is_err(),
            "a file without the exact protected owner-only DACL must fail"
        );
        assert_eq!(
            fs::read(&path).expect("preserved ambient bytes"),
            b"foreign"
        );
    }

    #[test]
    fn protected_state_is_no_replace_and_exact_delete_preserves_tampering() {
        let root = private_test_root();
        let expected = br#"{"schema":"v2","value":1}"#;
        root.directory
            .create_state_file_new("session-json", expected)
            .expect("create protected state");
        assert_eq!(
            root.directory
                .read_state_file("session-json", 1024)
                .expect("read protected state"),
            expected
        );
        assert!(
            root.directory
                .create_state_file_new("session-json", b"replacement")
                .is_err(),
            "CREATE_NEW must never replace an existing state record"
        );

        let tampered = br#"{"schema":"v2","value":2}"#;
        let path = root.directory.directory().path().join("session-json");
        fs::write(&path, tampered).expect("same-user tamper fixture");
        assert!(
            root.directory
                .delete_state_file_exact("session-json", expected)
                .is_err(),
            "stale expected bytes must not delete a changed record"
        );
        assert_eq!(fs::read(&path).expect("tampered bytes preserved"), tampered);
        root.directory
            .delete_state_file_exact("session-json", tampered)
            .expect("delete exact current protected record");
        assert!(!path.exists());
    }

    #[test]
    fn private_lock_contends_then_recovers_the_stale_filename_after_drop() {
        let root = private_test_root();
        let first = root
            .directory
            .open_and_lock("owner-lock")
            .expect("first owner lock");
        first.verify().expect("retained lock authority");
        let collision = root
            .directory
            .open_and_lock("owner-lock")
            .expect_err("duplicate owner lock must contend");
        assert!(private_lock_is_contended(&collision));
        drop(first);
        root.directory
            .open_and_lock("owner-lock")
            .expect("abandoned lock filename is recoverable")
            .verify()
            .expect("recovered lock authority");
    }

    #[allow(clippy::too_many_lines)]
    fn redirected_standard_stream_boundary_inner() {
        let system_root = system_root_directory().expect("trusted SystemRoot");
        system_root.verify().expect("SystemRoot replay");
        let current_directory = Rc::new(
            RealDirectory::capture(system_root.environment_path())
                .expect("trusted child directory"),
        );
        let private_root = private_test_root();
        let executable_name = "redirected-trusted-ping.exe";
        let executable_path = private_root
            .directory
            .directory()
            .path()
            .join(executable_name);
        let executable_bytes = fs::read(system_root.environment_path().join("System32/ping.exe"))
            .expect("read trusted ping executable");
        private_root
            .directory
            .create_test_executable_file_new(executable_name, &executable_bytes)
            .expect("create private single-link executable copy");
        let executable = Rc::new(
            RealFile::capture(&executable_path).expect("exact private ping executable copy"),
        );
        let executable_identity = executable.identity();
        let job = KillOnCloseJob::new().expect("kill-on-close Job");

        let redirected = RedirectedTestStreams::install().expect("redirect test standard handles");
        let child = job
            .spawn_and_assign(
                Rc::clone(&executable),
                &[
                    OsString::from("-n"),
                    OsString::from("30"),
                    OsString::from("127.0.0.1"),
                ],
                &[(
                    OsString::from("SystemRoot"),
                    system_root.environment_path().as_os_str().to_owned(),
                )],
                current_directory,
            )
            .expect("launch through redirected standard handles");
        let (stdout, stderr) = redirected
            .restore_and_take_readers()
            .expect("restore test standard handles");

        let (sender, receiver) = mpsc::channel();
        let stdout_sender = sender.clone();
        let stdout_reader = thread::spawn(move || {
            let _ = stdout_sender.send(("stdout", read_test_pipe_to_eof(&stdout)));
        });
        let stderr_reader = thread::spawn(move || {
            let _ = sender.send(("stderr", read_test_pipe_to_eof(&stderr)));
        });
        let stream_deadline = Instant::now() + Duration::from_secs(2);
        let mut stream_results = Vec::new();
        while stream_results.len() < 2 {
            let remaining = stream_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match receiver.recv_timeout(remaining) {
                Ok(result) => stream_results.push(result),
                Err(_) => break,
            }
        }
        let prompt_stream_count = stream_results.len();
        let child_was_live = child.try_wait().expect("redirected child poll").is_none();

        // Cleanup is unconditional and precedes every regression assertion so a
        // failing old boundary cannot leave its trusted fixture process behind.
        job.terminate(37)
            .expect("terminate exact redirected Job tree");
        let reap_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if child
                .try_wait()
                .expect("redirected child reap poll")
                .is_some()
            {
                break;
            }
            assert!(
                Instant::now() < reap_deadline,
                "redirected Job child did not reap"
            );
            thread::sleep(Duration::from_millis(20));
        }
        while stream_results.len() < 2 {
            stream_results.push(
                receiver
                    .recv_timeout(Duration::from_secs(5))
                    .expect("redirected stream reader did not finish after Job cleanup"),
            );
        }
        stdout_reader.join().expect("stdout reader thread");
        stderr_reader.join().expect("stderr reader thread");
        drop(child);
        drop(job);
        drop(executable);
        delete_file_exact(&executable_path, executable_identity)
            .expect("delete exact redirected executable copy");

        assert!(child_was_live, "trusted child exited before the EOF proof");
        assert_eq!(
            prompt_stream_count, 2,
            "native child retained a redirected controller stream"
        );
        for (name, result) in stream_results {
            let bytes = result.unwrap_or_else(|error| panic!("{name} EOF failed: {error}"));
            assert!(bytes.is_empty(), "native child wrote to redirected {name}");
        }
    }

    #[test]
    fn persistent_native_child_does_not_retain_redirected_standard_streams() {
        if env::var_os(REDIRECTED_CHILD_TEST_ENVIRONMENT).is_some() {
            redirected_standard_stream_boundary_inner();
            return;
        }
        let status = Command::new(env::current_exe().expect("current native test executable"))
            .arg("persistent_native_child_does_not_retain_redirected_standard_streams")
            .arg("--nocapture")
            .env(REDIRECTED_CHILD_TEST_ENVIRONMENT, "1")
            .status()
            .expect("isolated redirected-stream test helper");
        assert!(status.success(), "redirected-stream helper failed");
    }

    #[test]
    fn job_termination_reaps_one_exact_trusted_child() {
        let system_root = system_root_directory().expect("trusted SystemRoot");
        system_root.verify().expect("SystemRoot replay");
        let current_directory = Rc::new(
            RealDirectory::capture(system_root.environment_path())
                .expect("trusted child directory"),
        );
        let private_root = private_test_root();
        let executable_name = "trusted-ping-exe.exe";
        let executable_path = private_root
            .directory
            .directory()
            .path()
            .join(executable_name);
        let executable_bytes = fs::read(system_root.environment_path().join("System32/ping.exe"))
            .expect("read trusted ping executable");
        private_root
            .directory
            .create_test_executable_file_new(executable_name, &executable_bytes)
            .expect("create private single-link executable copy");
        let executable = Rc::new(
            RealFile::capture(&executable_path).expect("exact private ping executable copy"),
        );
        let executable_identity = executable.identity();
        let job = KillOnCloseJob::new().expect("kill-on-close Job");
        let child = job
            .spawn_and_assign(
                Rc::clone(&executable),
                &[
                    OsString::from("-n"),
                    OsString::from("30"),
                    OsString::from("127.0.0.1"),
                ],
                &[(
                    OsString::from("SystemRoot"),
                    system_root.environment_path().as_os_str().to_owned(),
                )],
                current_directory,
            )
            .expect("suspended assign-before-resume child");
        assert_eq!(child.try_wait().expect("initial child poll"), None);
        job.terminate(37).expect("terminate exact Job tree");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if child.try_wait().expect("child reap poll").is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "Job child did not reap");
            thread::sleep(Duration::from_millis(20));
        }
        drop(child);
        drop(executable);
        delete_file_exact(&executable_path, executable_identity)
            .expect("delete exact private executable copy");
        assert!(!executable_path.exists());
    }

    #[test]
    fn fragmented_request_and_response_use_one_accepted_transaction() {
        let user = CurrentUser::capture().expect("current SID");
        let capability = CapabilityToken::generate().expect("capability");
        let capability_hex = capability.lowercase_hex().into_bytes();
        let component = test_component("fragmented");
        let mut server = AuthenticatedPipeServer::create(user, &component, capability)
            .expect("first private pipe instance");
        let payload = br#"{"op":"ping"}"#.to_vec();
        let client_component = component.clone();
        let client = thread::spawn(move || -> io::Result<()> {
            let handle = open_test_client(&client_component)?;
            let result = (|| {
                write_fragmented(
                    handle,
                    &u32::try_from(payload.len())
                        .expect("test length")
                        .to_be_bytes(),
                )?;
                write_fragmented(handle, &capability_hex)?;
                write_fragmented(handle, &payload)?;
                let mut response_length = [0_u8; 4];
                read_exact_client(handle, &mut response_length)?;
                let mut response = vec![
                    0_u8;
                    usize::try_from(u32::from_be_bytes(response_length))
                        .map_err(|_| io::Error::other("response length"))?
                ];
                read_exact_client(handle, &mut response)?;
                write_fragmented(handle, &response_ack(&response))?;
                (response == br"{}")
                    .then_some(())
                    .ok_or_else(|| io::Error::other("response mismatch"))
            })();
            close_test_handle(handle);
            result
        });
        let transaction = server
            .accept(Instant::now() + Duration::from_secs(5))
            .expect("fragmented authenticated request");
        assert_eq!(transaction.payload(), br#"{"op":"ping"}"#);
        transaction.finish(br"{}").expect("bounded response");
        client
            .join()
            .expect("client thread")
            .expect("client exchange");
    }

    #[test]
    fn wrong_capability_is_disconnected_and_the_instance_reaccepts() {
        let user = CurrentUser::capture().expect("current SID");
        let capability = CapabilityToken::generate().expect("capability");
        let capability_hex = capability.lowercase_hex().into_bytes();
        let component = test_component("wrong-capability");
        let mut server = AuthenticatedPipeServer::create(user, &component, capability)
            .expect("first private pipe instance");
        let first_component = component.clone();
        let wrong_client = thread::spawn(move || -> io::Result<()> {
            let handle = open_test_client(&first_component)?;
            let result = (|| {
                write_fragmented(handle, &1_u32.to_be_bytes())?;
                write_fragmented(handle, &[b'x'; 64])?;
                write_fragmented(handle, b"x")
            })();
            close_test_handle(handle);
            result
        });
        assert!(
            server
                .accept(Instant::now() + Duration::from_secs(5))
                .is_err()
        );
        wrong_client
            .join()
            .expect("wrong-token thread")
            .expect("wrong-token write");

        let second_component = component.clone();
        let valid_client = thread::spawn(move || -> io::Result<()> {
            let handle = open_test_client(&second_component)?;
            let result = (|| {
                write_fragmented(handle, &1_u32.to_be_bytes())?;
                write_fragmented(handle, &capability_hex)?;
                write_fragmented(handle, b"x")?;
                let mut response_length = [0_u8; 4];
                read_exact_client(handle, &mut response_length)?;
                let mut response = vec![
                    0_u8;
                    usize::try_from(u32::from_be_bytes(response_length))
                        .map_err(|_| io::Error::other("response length"))?
                ];
                read_exact_client(handle, &mut response)?;
                write_fragmented(handle, &response_ack(&response))?;
                (response == b"x")
                    .then_some(())
                    .ok_or_else(|| io::Error::other("response mismatch"))
            })();
            close_test_handle(handle);
            result
        });
        let transaction = server
            .accept(Instant::now() + Duration::from_secs(5))
            .expect("server reaccepts valid capability");
        assert_eq!(transaction.payload(), b"x");
        transaction.finish(b"x").expect("response after reaccept");
        valid_client
            .join()
            .expect("valid-token thread")
            .expect("valid-token exchange");
    }

    #[test]
    fn wrong_or_missing_response_ack_is_rejected_and_the_server_reaccepts() {
        let user = CurrentUser::capture().expect("current SID");
        let capability = CapabilityToken::generate().expect("capability");
        let capability_hex = capability.lowercase_hex().into_bytes();
        let component = test_component("response-ack");
        let mut server = AuthenticatedPipeServer::create(user, &component, capability)
            .expect("first private pipe instance");
        let wrong_ack = [0_u8; 32];
        assert_ne!(wrong_ack, response_ack(b"ok"));

        for acknowledgement in [Some(wrong_ack), None] {
            let client_component = component.clone();
            let client_capability = capability_hex.clone();
            let client = thread::spawn(move || {
                client_request_then_optional_response_ack(
                    &client_component,
                    &client_capability,
                    acknowledgement,
                )
            });
            let transaction = server
                .accept(Instant::now() + Duration::from_secs(5))
                .expect("authenticated request before acknowledgement failure");
            assert_eq!(transaction.payload(), b"x");
            assert!(transaction.finish(b"ok").is_err());
            client
                .join()
                .expect("ack client thread")
                .expect("ack client exchange");
        }

        let client_component = component.clone();
        let client_capability = capability_hex;
        let client = thread::spawn(move || {
            client_request_then_optional_response_ack(
                &client_component,
                &client_capability,
                Some(response_ack(b"ok")),
            )
        });
        let transaction = server
            .accept(Instant::now() + Duration::from_secs(5))
            .expect("server reaccepts after acknowledgement failures");
        transaction
            .finish(b"ok")
            .expect("valid acknowledgement completes response");
        client
            .join()
            .expect("valid ack client thread")
            .expect("valid ack client exchange");
    }

    #[test]
    fn stalled_client_deadline_cancels_the_pending_read() {
        let user = CurrentUser::capture().expect("current SID");
        let capability = CapabilityToken::generate().expect("capability");
        let capability_hex = capability.lowercase_hex().into_bytes();
        let component = test_component("stalled");
        let mut server = AuthenticatedPipeServer::create(user, &component, capability)
            .expect("first private pipe instance");
        let (opened_sender, opened_receiver) = mpsc::channel();
        let client_component = component.clone();
        let client = thread::spawn(move || -> io::Result<()> {
            let handle = open_test_client(&client_component)?;
            opened_sender.send(()).expect("test receiver present");
            write_fragmented(handle, &1_u32.to_be_bytes()[..2])?;
            thread::sleep(Duration::from_millis(500));
            close_test_handle(handle);
            Ok(())
        });
        let accepted = server.accept(Instant::now() + Duration::from_millis(200));
        opened_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("client connected");
        assert_eq!(
            accepted.expect_err("stalled header must time out").kind(),
            io::ErrorKind::TimedOut
        );
        client
            .join()
            .expect("client thread")
            .expect("stalled client");

        let client_component = component.clone();
        let client = thread::spawn(move || {
            client_request_then_optional_response_ack(
                &client_component,
                &capability_hex,
                Some(response_ack(b"ok")),
            )
        });
        let transaction = server
            .accept(Instant::now() + Duration::from_secs(5))
            .expect("server reaccepts after cancelled partial read");
        transaction
            .finish(b"ok")
            .expect("response after partial-read timeout");
        client
            .join()
            .expect("reaccept client thread")
            .expect("reaccept exchange");
    }

    #[test]
    fn first_instance_flag_preserves_foreign_server_ownership() {
        let user = CurrentUser::capture().expect("current SID");
        let component = test_component("first-instance");
        let capability = CapabilityToken::generate().expect("capability");
        let capability_hex = capability.lowercase_hex().into_bytes();
        let first =
            AuthenticatedPipeServer::create(std::rc::Rc::clone(&user), &component, capability)
                .expect("initial server owns first instance");
        assert!(
            AuthenticatedPipeServer::create(
                user,
                &component,
                CapabilityToken::generate().expect("capability"),
            )
            .is_err()
        );
        let mut first = first;
        let client_component = component.clone();
        let client = thread::spawn(move || {
            client_request_then_optional_response_ack(
                &client_component,
                &capability_hex,
                Some(response_ack(b"ok")),
            )
        });
        let transaction = first
            .accept(Instant::now() + Duration::from_secs(5))
            .expect("original first instance remains authoritative");
        transaction.finish(b"ok").expect("original server exchange");
        client
            .join()
            .expect("first-instance client thread")
            .expect("first-instance exchange");
    }

    #[test]
    fn client_connect_timeout_does_not_prevent_a_later_server_exchange() {
        let component = test_component("connect-timeout");
        let capability = CapabilityToken::generate().expect("capability");
        let timeout = AuthenticatedPipeClient::connect(
            CurrentUser::capture().expect("current SID"),
            &component,
            capability.clone(),
            Instant::now() + Duration::from_millis(100),
        )
        .expect_err("absent pipe must time out");
        assert_eq!(timeout.kind(), io::ErrorKind::TimedOut);

        let mut server = AuthenticatedPipeServer::create(
            CurrentUser::capture().expect("current SID"),
            &component,
            capability.clone(),
        )
        .expect("server after timed-out connection");
        let client_component = component.clone();
        let client = thread::spawn(move || -> io::Result<Vec<u8>> {
            let response = AuthenticatedPipeClient::connect(
                CurrentUser::capture()?,
                &client_component,
                capability,
                Instant::now() + Duration::from_secs(5),
            )?
            .transact(b"ping")?;
            Ok(response.payload().to_vec())
        });
        let transaction = server
            .accept(Instant::now() + Duration::from_secs(5))
            .expect("request after connection timeout");
        assert_eq!(transaction.payload(), b"ping");
        transaction.finish(b"pong").expect("bounded response");
        assert_eq!(
            client
                .join()
                .expect("native client thread")
                .expect("native client exchange"),
            b"pong"
        );
    }

    #[test]
    fn maximum_response_completes_the_authenticated_ack_path() {
        let component = test_component("large-response");
        let capability = CapabilityToken::generate().expect("capability");
        let mut server = AuthenticatedPipeServer::create(
            CurrentUser::capture().expect("current SID"),
            &component,
            capability.clone(),
        )
        .expect("large-response server");
        let client_component = component.clone();
        let client = thread::spawn(move || -> io::Result<Vec<u8>> {
            let response = AuthenticatedPipeClient::connect(
                CurrentUser::capture()?,
                &client_component,
                capability,
                Instant::now() + Duration::from_secs(10),
            )?
            .transact(b"large")?;
            Ok(response.payload().to_vec())
        });
        let transaction = server
            .accept(Instant::now() + Duration::from_secs(10))
            .expect("large-response request");
        assert_eq!(transaction.payload(), b"large");
        let response = vec![b'x'; 128 * 1024];
        transaction
            .finish(&response)
            .expect("maximum response and ACK");
        assert_eq!(
            client
                .join()
                .expect("large response client thread")
                .expect("large response exchange"),
            response
        );
    }
}
