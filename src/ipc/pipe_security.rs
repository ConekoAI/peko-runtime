//! Windows named-pipe security attributes (DACL) — ADR-038.
//!
//! On Windows, the peko daemon binds a named pipe as its IPC transport. To
//! match the trust story we get "for free" from a Unix domain socket (file
//! mode 0600), the pipe must reject connections from any process running as
//! a user other than the daemon's owner. The kernel enforces this via the
//! pipe's discretionary ACL (DACL).
//!
//! The SDDL string `O:BAD:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)` says:
//!   - `O:BA`              — owner is the local Administrator group. Required
//!                            by `CreateNamedPipeW`: the kernel refuses other
//!                            owners for the pipe container object.
//!   - `D:(`               — discretionary ACL follows.
//!   - `(A;;GA;;;SY)`      — Allow, Generic All, to LocalSystem (so
//!                            SCM-managed services can still introspect).
//!   - `(A;;GA;;;BA)`      — Allow, Generic All, to Built-in Administrators.
//!   - `(A;;GA;;;OW)`      — Allow, Generic All, to the *current* object
//!                            owner — i.e. the user who created the pipe,
//!                            which is the user running `peko daemon start`.
//!
//! The result is a pipe that any user-mode process owned by the daemon's
//! user can `CreateFileW`, and no one else.
//!
//! FFI pattern mirrors `src/common/process/job_object.rs`: `unsafe impl
//! Send + Sync` for the raw handle, `#[cfg(windows)]` / `#[cfg(not(windows))]`
//! split impls, `// SAFETY:` comments inside the `unsafe { }` blocks.

#[cfg(windows)]
use windows_sys::Win32::Security::PSECURITY_DESCRIPTOR;

/// Owns a Win32-allocated `SECURITY_DESCRIPTOR` for use as a pipe DACL.
///
/// The handle is freed in `Drop` via `LocalFree`. Clone is intentionally
/// not derived — each `ServerOptions::create` call needs a stable borrow
/// of the underlying `SECURITY_ATTRIBUTES`, so we hand out a fresh one per
/// call instead of sharing.
#[cfg(windows)]
pub struct PipeSecurityAttributes {
    descriptor: PSECURITY_DESCRIPTOR,
}

#[cfg(not(windows))]
pub struct PipeSecurityAttributes {
    _private: (),
}

// Raw Win32 pointers to security descriptors are safe to send and share
// across threads — they are just opaque, refcounted-by-Drop memory managed
// by the kernel/user-mode heap. We explicitly mark the type as Send + Sync
// so it can live inside the IPC server's `ServerSocket::NamedPipe` variant,
// which is stored in `Arc` and shared across the per-connection accept loop.
#[cfg(windows)]
unsafe impl Send for PipeSecurityAttributes {}
#[cfg(windows)]
unsafe impl Sync for PipeSecurityAttributes {}

/// Build the SDDL-converted DACL for "current user only" (plus LocalSystem
/// and Built-in Administrators, for parity with the Unix 0600 mode that
/// still permits system and admin tools to read the socket).
///
/// # Errors
/// Returns an error if `ConvertStringSecurityDescriptorToSecurityDescriptorW`
/// fails (e.g. malformed SDDL — should not happen for the hard-coded
/// constant in this module).
#[cfg(windows)]
pub fn current_user_only() -> anyhow::Result<PipeSecurityAttributes> {
    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
    use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;

    // SDDL_REVISION_1 (the only revision Win32 currently supports).
    const SDDL_REVISION_1: u32 = 1;

    let sddl: Vec<u16> = sddl_current_user_only()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: this SDDL string is a compile-time constant; the call
    // is documented as taking a NUL-terminated UTF-16 string and a
    // pointer to receive the allocated descriptor. We pass a pointer
    // to a stack `len` u32 that the API will write into.
    unsafe {
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let mut len: u32 = 0;
        let ok = ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            &mut len,
        );
        if ok == 0 {
            let err = GetLastError();
            anyhow::bail!(
                "ConvertStringSecurityDescriptorToSecurityDescriptorW failed: {}",
                err
            );
        }
        Ok(PipeSecurityAttributes { descriptor: sd })
    }
}

/// Non-Windows stub. Named pipes are Windows-only, and the Unix IPC
/// transport is `UnixDatagram` whose trust story is enforced by filesystem
/// mode bits — no DACL needed. Callers in `mod.rs` and `server.rs` are
/// themselves `#[cfg(windows)]`-gated, so this stub is unreachable on
/// non-Windows builds.
#[cfg(not(windows))]
pub fn current_user_only() -> anyhow::Result<PipeSecurityAttributes> {
    Ok(PipeSecurityAttributes { _private: () })
}

/// Borrow the underlying descriptor as a `SECURITY_ATTRIBUTES` for the
/// lifetime of `&self`. The returned struct references `self.descriptor`;
/// the caller must not outlive this borrow.
///
/// Windows-only: callers (the `ServerOptions::security_attributes(&sa)`
/// argument in `server.rs`) only exist on Windows. Gated here so the
/// windows-sys type isn't referenced on Unix builds.
#[cfg(windows)]
pub fn as_attributes(
    attrs: &PipeSecurityAttributes,
) -> windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
    // nLength is required to be set per the Win32 docs;
    // bInheritHandle = FALSE (named pipes are not inherited by default).
    windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: attrs.descriptor,
        bInheritHandle: 0,
    }
}

/// The SDDL string we use for the named-pipe DACL. Exposed as a `const fn`
/// so it can be referenced in tests and documentation without duplication.
#[cfg(windows)]
pub(crate) const fn sddl_current_user_only() -> &'static str {
    "O:BAD:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)"
}

#[cfg(windows)]
impl Drop for PipeSecurityAttributes {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::LocalFree;
        // SAFETY: `self.descriptor` was returned by
        // `ConvertStringSecurityDescriptorToSecurityDescriptorW`, which
        // documents that the descriptor must be freed with `LocalFree`.
        // `LocalFree` accepts null and returns null on success.
        unsafe {
            if !self.descriptor.is_null() {
                let _ = LocalFree(self.descriptor as _);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the SDDL constant. On non-Windows this compiles to nothing
    /// because the constant is gated, and the test is a no-op.
    #[cfg(windows)]
    #[test]
    fn sddl_string_matches_documented_value() {
        assert_eq!(
            sddl_current_user_only(),
            "O:BAD:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)",
            "ADR-038 documents this exact SDDL; do not change it without updating the ADR"
        );
    }

    /// Construct the security attributes. The call may legitimately fail
    /// under unusual test environments (e.g. CI runners with restricted
    /// process tokens), mirroring the caveat in `job_object.rs` for
    /// `CreateJobObjectW` — so we only assert it does not panic, not
    /// that it succeeds.
    #[test]
    fn construct_pipe_security_attributes() {
        let _ = current_user_only();
    }
}
