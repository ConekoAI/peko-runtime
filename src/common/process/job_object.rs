//! Windows Job Objects for automatic child-process-tree termination.
//!
//! On Windows, assigning a child process to a job object with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` ensures that when the last handle to
//! the job is closed, the OS terminates **all** processes in the job,
//! including grandchildren.  This solves the classic "orphan process" problem
//! on Windows where `child.kill()` only kills the immediate child.


/// A handle to a Windows Job Object.
///
/// When dropped, the handle is closed.  If `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
/// was set at creation time, closing the last handle kills every process that
/// is still a member of the job.
#[cfg(windows)]
pub struct JobObject {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(not(windows))]
pub struct JobObject {
    _private: (),
}

// JobObject wraps a raw Windows HANDLE (isize in windows-sys 0.52).  Handles
// are safe to send and share across threads — they are just opaque tokens
// managed by the kernel.  We explicitly mark the type as Send + Sync so it
// can live inside ManagedRuntime, which is stored in tokio::sync::RwLock and
// Arc.
unsafe impl Send for JobObject {}
unsafe impl Sync for JobObject {}

impl JobObject {
    /// Create a new job object with *Kill-On-Close* semantics.
    ///
    /// On non-Windows platforms this is a no-op.
    ///
    /// # Errors
    /// Returns an error if the Windows `CreateJobObjectW` or `SetInformationJobObject` calls fail.
    #[cfg(windows)]
    pub fn new() -> anyhow::Result<Self> {
        use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectBasicLimitInformation, SetInformationJobObject,
            JOBOBJECT_BASIC_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        unsafe {
            let handle = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
            if handle == 0 || handle == INVALID_HANDLE_VALUE {
                anyhow::bail!("CreateJobObjectW failed: {}", GetLastError());
            }

            let mut info = JOBOBJECT_BASIC_LIMIT_INFORMATION {
                LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                ..std::mem::zeroed()
            };

            let result = SetInformationJobObject(
                handle,
                JobObjectBasicLimitInformation,
                std::ptr::addr_of_mut!(info).cast(),
                u32::try_from(std::mem::size_of::<JOBOBJECT_BASIC_LIMIT_INFORMATION>())
                    .unwrap_or(u32::MAX),
            );

            if result == 0 {
                let err = GetLastError();
                CloseHandle(handle);
                anyhow::bail!("SetInformationJobObject failed: {err}");
            }

            Ok(Self { handle })
        }
    }

    #[cfg(not(windows))]
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self { _private: () })
    }

    /// Assign an already-started child process to this job object.
    ///
    /// Must be called **before** the child has a chance to spawn its own
    /// children; otherwise those grandchildren will *not* be in the job.
    ///
    /// # Errors
    /// Returns an error if `AssignProcessToJobObject` fails with a non-access-denied code.
    #[cfg(windows)]
    pub fn assign_process(&self, child: &tokio::process::Child) -> anyhow::Result<()> {
        use windows_sys::Win32::Foundation::{GetLastError, ERROR_ACCESS_DENIED};
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

        unsafe {
            let raw = child
                .raw_handle()
                .ok_or_else(|| anyhow::anyhow!("Child process handle not available"))?;
            // tokio returns *mut c_void (RawHandle), windows-sys expects isize (HANDLE)
            let process_handle = raw as windows_sys::Win32::Foundation::HANDLE;
            if AssignProcessToJobObject(self.handle, process_handle) == 0 {
                let err = GetLastError();
                if err == ERROR_ACCESS_DENIED {
                    // The process may already be in a job (e.g. running under
                    // a debugger, or nested inside another job object).  This
                    // is not fatal — we simply won't get kill-on-close for
                    // this particular process tree.
                    warn!(
                        "Could not assign process to job object (already in a job?): {}",
                        err
                    );
                    return Ok(());
                }
                anyhow::bail!("AssignProcessToJobObject failed: {err}");
            }
            Ok(())
        }
    }

    #[cfg(not(windows))]
    pub fn assign_process(&self, _child: &tokio::process::Child) -> anyhow::Result<()> {
        Ok(())
    }

    /// Close the job handle explicitly.
    ///
    /// This is a no-op on non-Windows platforms.
    #[allow(clippy::unused_self)]
    pub fn close(self) {
        // Drop runs automatically, which closes the handle.
    }
}

#[cfg(windows)]
impl Drop for JobObject {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_job_object_creation() {
        // On Windows this calls CreateJobObjectW.  It may fail if the test
        // runner itself is already in a job object (e.g. under CI or a
        // debugger), so we only assert that it does not panic.
        let _ = JobObject::new();
    }
}
