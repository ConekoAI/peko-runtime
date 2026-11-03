//! Windows named-pipe security attributes (DACL) — ADR-038.
//!
//! On Windows, the peko daemon binds a named pipe as its IPC transport. To
//! match the trust story we get "for free" from a Unix domain socket (file
//! mode 0600), the pipe must reject connections from any process running as
//! a user other than the daemon's owner. The kernel enforces this via the
//! pipe's discretionary ACL (DACL).
//!
//! The SDDL string `D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;<user SID>)` says:
//!   - no `O:` part        — the owner defaults to the creating token's.
//!                            An explicit owner the token may not assign
//!                            fails the bind: `O:BA` (Administrators) did,
//!                            with ERROR_INVALID_OWNER (1307), for every
//!                            user who wasn't elevated.
//!   - `D:P`               — a protected DACL follows (nothing inherited).
//!   - `(A;;GA;;;SY)`      — Allow, Generic All, to LocalSystem (so
//!                            SCM-managed services can still introspect).
//!   - `(A;;GA;;;BA)`      — Allow, Generic All, to Built-in Administrators.
//!   - `(A;;GA;;;<SID>)`   — Allow, Generic All, to the user running the
//!                            daemon, named by SID. (`OW`, the owner, would
//!                            be Administrators for an elevated daemon.)
//!
//! The result is a pipe that any user-mode process owned by the daemon's
//! user can `CreateFileW`, and no one else. The pipe also refuses clients
//! on other machines (`PIPE_REJECT_REMOTE_CLIENTS`, set in `server.rs`).
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
/// Returns an error if the process token's user can't be read, or if
/// `ConvertStringSecurityDescriptorToSecurityDescriptorW` rejects the SDDL.
#[cfg(windows)]
pub fn current_user_only() -> anyhow::Result<PipeSecurityAttributes> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;

    // SDDL_REVISION_1 (the only revision Win32 currently supports).
    const SDDL_REVISION_1: u32 = 1;

    let sddl: Vec<u16> = sddl_for_user(&current_user_sid()?)
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: the call is documented as taking a NUL-terminated UTF-16
    // string and a pointer to receive the allocated descriptor. We pass
    // a pointer to a stack `len` u32 that the API will write into.
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

/// The SDDL for a pipe only `user_sid` (plus LocalSystem and
/// Administrators) may open. See the module docs for each part.
#[cfg(windows)]
pub(crate) fn sddl_for_user(user_sid: &str) -> String {
    format!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;{user_sid})")
}

/// The string SID (`S-1-5-21-…`) of the user this process runs as.
#[cfg(windows)]
pub(crate) fn current_user_sid() -> anyhow::Result<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, LocalFree, HANDLE};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // SAFETY: every out-pointer points at a live local; the TOKEN_USER
    // buffer is u64-aligned and sized by the first GetTokenInformation
    // call; the SID string ConvertSidToStringSidW allocates is
    // NUL-terminated, read before it is freed, and freed with LocalFree
    // as documented.
    unsafe {
        let mut token: HANDLE = 0;
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            anyhow::bail!("OpenProcessToken failed: {}", GetLastError());
        }
        let mut len: u32 = 0;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len);
        let err = GetLastError();
        CloseHandle(token);
        if ok == 0 {
            anyhow::bail!("GetTokenInformation(TokenUser) failed: {err}");
        }
        let user = &*(buf.as_ptr() as *const TOKEN_USER);

        let mut wide: *mut u16 = std::ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut wide) == 0 {
            anyhow::bail!("ConvertSidToStringSidW failed: {}", GetLastError());
        }
        let n = (0..).take_while(|&i| *wide.add(i) != 0).count();
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(wide, n));
        let _ = LocalFree(wide as _);
        Ok(sid)
    }
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

    #[cfg(windows)]
    #[test]
    fn sddl_string_matches_documented_value() {
        assert_eq!(
            sddl_for_user("S-1-5-21-1-2-3-1001"),
            "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;S-1-5-21-1-2-3-1001)",
            "ADR-038 documents this exact SDDL; do not change it without updating the ADR"
        );
    }

    /// The bind failed for every non-elevated user while the SDDL named
    /// an owner (`O:BA`), and CI runs elevated, so it passed there. Assert
    /// on the descriptor itself: no owner, and an ACE for this user.
    #[cfg(windows)]
    #[test]
    fn descriptor_sets_no_owner_and_grants_the_current_user() {
        use windows_sys::Win32::Security::GetSecurityDescriptorOwner;

        let sid = current_user_sid().expect("current user SID");
        assert!(sid.starts_with("S-1-"), "got {sid}");
        assert!(sddl_for_user(&sid).ends_with(&format!(";;;{sid})")));

        let attrs = current_user_only().expect("build descriptor");
        let mut owner = std::ptr::null_mut();
        let mut defaulted = 0;
        // SAFETY: `attrs.descriptor` is a valid descriptor for the
        // lifetime of `attrs`; both out-pointers point at live locals.
        let ok =
            unsafe { GetSecurityDescriptorOwner(attrs.descriptor, &mut owner, &mut defaulted) };
        assert_ne!(ok, 0);
        assert!(
            owner.is_null(),
            "the descriptor must leave the owner to the token"
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
