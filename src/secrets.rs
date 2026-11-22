//! API key storage in Windows Credential Manager.
//!
//! Keys never touch config.json. The Reddit password stays in memory only.

/// Credential Manager target for one LLM provider.
pub fn target(provider: &str) -> String {
    format!("RedditResearch/llm/{}", provider)
}

#[cfg(target_os = "windows")]
mod windows_store {
    const CRED_TYPE_GENERIC: u32 = 1;
    const CRED_PERSIST_LOCAL_MACHINE: u32 = 2;
    const ERROR_NOT_FOUND: i32 = 1168;

    #[repr(C)]
    struct CredentialW {
        flags: u32,
        kind: u32,
        target_name: *mut u16,
        comment: *mut u16,
        last_written: [u32; 2],
        credential_blob_size: u32,
        credential_blob: *mut u8,
        persist: u32,
        attribute_count: u32,
        attributes: *mut core::ffi::c_void,
        target_alias: *mut u16,
        user_name: *mut u16,
    }

    #[allow(non_snake_case)]
    #[link(name = "advapi32")]
    extern "system" {
        fn CredWriteW(credential: *const CredentialW, flags: u32) -> i32;
        fn CredReadW(
            target_name: *const u16,
            kind: u32,
            flags: u32,
            credential: *mut *mut CredentialW,
        ) -> i32;
        fn CredDeleteW(target_name: *const u16, kind: u32, flags: u32) -> i32;
        fn CredFree(buffer: *const core::ffi::c_void);
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn save(target: &str, secret: &str) -> Result<(), String> {
        if secret.is_empty() {
            return delete(target);
        }
        let mut name = wide(target);
        let mut blob = secret.as_bytes().to_vec();
        let credential = CredentialW {
            flags: 0,
            kind: CRED_TYPE_GENERIC,
            target_name: name.as_mut_ptr(),
            comment: std::ptr::null_mut(),
            last_written: [0, 0],
            credential_blob_size: blob.len() as u32,
            credential_blob: blob.as_mut_ptr(),
            persist: CRED_PERSIST_LOCAL_MACHINE,
            attribute_count: 0,
            attributes: std::ptr::null_mut(),
            target_alias: std::ptr::null_mut(),
            user_name: std::ptr::null_mut(),
        };
        let ok = unsafe { CredWriteW(&credential, 0) };
        for byte in blob.iter_mut() {
            *byte = 0;
        }
        if ok != 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error().to_string())
        }
    }

    pub fn load(target: &str) -> Option<String> {
        let name = wide(target);
        let mut pointer: *mut CredentialW = std::ptr::null_mut();
        let ok = unsafe { CredReadW(name.as_ptr(), CRED_TYPE_GENERIC, 0, &mut pointer) };
        if ok == 0 || pointer.is_null() {
            return None;
        }
        let credential = unsafe { &*pointer };
        let value = if credential.credential_blob.is_null() || credential.credential_blob_size == 0
        {
            String::new()
        } else {
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    credential.credential_blob,
                    credential.credential_blob_size as usize,
                )
            };
            String::from_utf8_lossy(bytes).to_string()
        };
        unsafe { CredFree(pointer as *const core::ffi::c_void) };
        (!value.is_empty()).then_some(value)
    }

    pub fn delete(target: &str) -> Result<(), String> {
        let name = wide(target);
        let ok = unsafe { CredDeleteW(name.as_ptr(), CRED_TYPE_GENERIC, 0) };
        if ok != 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_NOT_FOUND) {
            Ok(())
        } else {
            Err(error.to_string())
        }
    }
}

#[cfg(target_os = "windows")]
pub fn save(target: &str, secret: &str) -> Result<(), String> {
    windows_store::save(target, secret)
}

#[cfg(target_os = "windows")]
pub fn load(target: &str) -> Option<String> {
    windows_store::load(target)
}

#[cfg(target_os = "windows")]
pub fn delete(target: &str) -> Result<(), String> {
    windows_store::delete(target)
}

#[cfg(not(target_os = "windows"))]
pub fn save(_target: &str, _secret: &str) -> Result<(), String> {
    Err("Credential Manager is only available on Windows".to_string())
}

#[cfg(not(target_os = "windows"))]
pub fn load(_target: &str) -> Option<String> {
    None
}

#[cfg(not(target_os = "windows"))]
pub fn delete(_target: &str) -> Result<(), String> {
    Ok(())
}
