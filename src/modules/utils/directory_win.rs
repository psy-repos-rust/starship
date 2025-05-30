use std::{mem, os::windows::ffi::OsStrExt, path::Path};

use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, HANDLE},
        Security::{
            AccessCheck, DACL_SECURITY_INFORMATION, DuplicateToken, GENERIC_MAPPING,
            GROUP_SECURITY_INFORMATION, GetFileSecurityW, MapGenericMask,
            OWNER_SECURITY_INFORMATION, PRIVILEGE_SET, PSECURITY_DESCRIPTOR, SecurityImpersonation,
            TOKEN_DUPLICATE, TOKEN_IMPERSONATE, TOKEN_QUERY, TOKEN_READ_CONTROL,
        },
        Storage::FileSystem::{
            FILE_ALL_ACCESS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
        UI::Shell::PathIsNetworkPathW,
    },
    core::{BOOL, PCWSTR},
};

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        if let Err(e) = unsafe { CloseHandle(self.0) } {
            log::debug!("CloseHandle failed: {e:?}");
        }
    }
}

/// Checks if the current user has write access right to the `folder_path`
///
/// First, the function extracts DACL from the given directory and then calls `AccessCheck` against
/// the current process access token and directory's security descriptor.
/// Does not work for network drives and always returns true
pub fn is_write_allowed(folder_path: &Path) -> std::result::Result<bool, String> {
    let wpath_vec: Vec<u16> = folder_path.as_os_str().encode_wide().chain([0]).collect();
    let wpath = PCWSTR(wpath_vec.as_ptr());

    if unsafe { PathIsNetworkPathW(wpath) }.as_bool() {
        log::info!(
            "Directory '{:?}' is a network drive, unable to check write permissions. See #1506 for details",
            folder_path
        );
        return Ok(true);
    }

    let mut length = 0;

    let rc = unsafe {
        GetFileSecurityW(
            wpath,
            (OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION).0,
            None,
            0,
            &mut length,
        )
    };

    // expect ERROR_INSUFFICIENT_BUFFER
    match rc.ok() {
        Err(e) if e.code() == ERROR_INSUFFICIENT_BUFFER.into() => (),
        result => {
            return Err(format!(
                "GetFileSecurityW returned unexpected return value when asked for the security descriptor size: {result:?}"
            ));
        }
    }

    let mut buf = vec![0u8; length as usize];
    let psecurity_descriptor = PSECURITY_DESCRIPTOR(buf.as_mut_ptr().cast::<std::ffi::c_void>());

    let rc = unsafe {
        GetFileSecurityW(
            wpath,
            (OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION).0,
            Some(psecurity_descriptor),
            length,
            &mut length,
        )
    };

    if let Err(e) = rc.ok() {
        return Err(format!(
            "GetFileSecurityW failed to retrieve the security descriptor: {e:?}"
        ));
    }

    let token = {
        let mut token = HANDLE::default();

        let rc = unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_IMPERSONATE | TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_READ_CONTROL,
                &mut token,
            )
        };
        if let Err(e) = rc {
            return Err(format!(
                "OpenProcessToken failed to retrieve current process' security token: {e:?}"
            ));
        }

        Handle(token)
    };

    let impersonated_token = {
        let mut impersonated_token = HANDLE::default();
        let rc = unsafe { DuplicateToken(token.0, SecurityImpersonation, &mut impersonated_token) };

        if let Err(e) = rc {
            return Err(format!("DuplicateToken failed: {e:?}"));
        }

        Handle(impersonated_token)
    };

    let mapping = GENERIC_MAPPING {
        GenericRead: FILE_GENERIC_READ.0,
        GenericWrite: FILE_GENERIC_WRITE.0,
        GenericExecute: FILE_GENERIC_EXECUTE.0,
        GenericAll: FILE_ALL_ACCESS.0,
    };

    let mut privileges: PRIVILEGE_SET = PRIVILEGE_SET::default();
    let mut priv_size = mem::size_of::<PRIVILEGE_SET>() as _;
    let mut granted_access = 0;
    let mut access_rights = FILE_GENERIC_WRITE;
    let mut result = BOOL::default();
    unsafe { MapGenericMask(&mut access_rights.0, &mapping) };
    let rc = unsafe {
        AccessCheck(
            psecurity_descriptor,
            impersonated_token.0,
            access_rights.0,
            &mapping,
            Some(&mut privileges),
            &mut priv_size,
            &mut granted_access,
            &mut result,
        )
    };

    if let Err(e) = rc {
        return Err(format!("AccessCheck failed: {e:?}"));
    }

    Ok(result.as_bool())
}
