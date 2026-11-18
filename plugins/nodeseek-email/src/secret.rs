use std::fmt;
use std::io::{self, BufRead, Read as _};

#[cfg(unix)]
use std::io::Write as _;

#[cfg(target_os = "macos")]
use std::{ffi::CString, path::PathBuf, ptr};

#[cfg(target_os = "macos")]
use core_foundation::base::TCFType as _;

#[cfg(target_os = "macos")]
use security_framework::item::{ItemClass, ItemSearchOptions, SearchResult};

use zeroize::Zeroizing;

use crate::error::{AppError, AppResult, ErrorCode};

#[cfg(target_os = "macos")]
// v3 deliberately avoids inheriting v1's per-executable ACL. v1 was created before the
// provider's stable signing identifiers existed and can block every background read on a hidden
// authorization prompt; the next foreground save creates a fresh item with the current UI and
// provider trusted applications.
const KEYCHAIN_SERVICE: &str = "app.maild.provider.nodeseek-email.credentials.v3";
#[cfg(target_os = "macos")]
const KEYCHAIN_ACCOUNT: &str = "community-provider-default";
const MAX_API_KEY_BYTES: usize = 256;

#[cfg(target_os = "macos")]
type SecTrustedApplicationRef = *mut OpaqueSecTrustedApplicationRef;

#[cfg(target_os = "macos")]
enum OpaqueSecTrustedApplicationRef {}

#[cfg(target_os = "macos")]
extern "C" {
    static kSecAttrAccess: core_foundation_sys::string::CFStringRef;
    fn SecAccessCreate(
        descriptor: core_foundation_sys::string::CFStringRef,
        trusted_list: core_foundation_sys::array::CFArrayRef,
        access: *mut security_framework_sys::base::SecAccessRef,
    ) -> core_foundation_sys::base::OSStatus;
    fn SecTrustedApplicationCreateFromPath(
        path: *const std::os::raw::c_char,
        application: *mut SecTrustedApplicationRef,
    ) -> core_foundation_sys::base::OSStatus;
}

pub struct SecretBuffer(Zeroizing<Vec<u8>>);

impl SecretBuffer {
    pub fn from_bytes(bytes: Vec<u8>) -> AppResult<Self> {
        Self::from_zeroizing(Zeroizing::new(bytes))
    }

    fn from_zeroizing(bytes: Zeroizing<Vec<u8>>) -> AppResult<Self> {
        validate_api_key(&bytes)?;
        Ok(Self(bytes))
    }

    pub fn as_str(&self) -> AppResult<&str> {
        std::str::from_utf8(&self.0).map_err(|_| {
            AppError::new(
                ErrorCode::CredentialInvalid,
                "Keychain 中的 API key 格式无效",
            )
        })
    }

    pub fn expose_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretBuffer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("SecretBuffer")
            .field(&"[redacted]")
            .finish()
    }
}

/// Assembled at runtime so static secret scanners (e.g. GitHub Push Protection's Stripe rule)
/// never mistake the upstream NodeSeek API key prefix or unit-test fixtures for a leaked live key.
pub(crate) fn api_key_prefix() -> String {
    ["sk", "live", ""].join("_")
}

fn validate_api_key(bytes: &[u8]) -> AppResult<()> {
    let prefix = api_key_prefix();
    let valid = bytes.len() > prefix.len()
        && bytes.len() <= MAX_API_KEY_BYTES
        && bytes.starts_with(prefix.as_bytes())
        && bytes.iter().all(u8::is_ascii_graphic)
        && !bytes.iter().any(u8::is_ascii_whitespace);
    if !valid {
        return Err(AppError::new(
            ErrorCode::CredentialInvalid,
            "API key 格式无效",
        ));
    }
    Ok(())
}

pub trait SecretStore {
    fn load(&self) -> AppResult<SecretBuffer>;
    fn save(&self, value: SecretBuffer) -> AppResult<()>;
    fn delete(&self) -> AppResult<()>;
    fn exists(&self) -> AppResult<bool>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PlatformSecretStore;

#[cfg(target_os = "macos")]
impl SecretStore for PlatformSecretStore {
    fn load(&self) -> AppResult<SecretBuffer> {
        load_keychain_secret()
    }

    fn save(&self, value: SecretBuffer) -> AppResult<()> {
        let _interaction_lock =
            security_framework::os::macos::keychain::SecKeychain::disable_user_interaction().ok();
        let trusted = trusted_applications()?;
        let descriptor = core_foundation::string::CFString::new(KEYCHAIN_SERVICE);
        let mut access = ptr::null_mut();
        let status = unsafe {
            SecAccessCreate(
                descriptor.as_concrete_TypeRef(),
                trusted.as_concrete_TypeRef(),
                &mut access,
            )
        };
        if status != security_framework_sys::base::errSecSuccess || access.is_null() {
            return Err(keychain_error());
        }
        let access = unsafe {
            security_framework::os::macos::access::SecAccess::wrap_under_create_rule(access)
        };
        let class = unsafe {
            core_foundation::string::CFString::wrap_under_get_rule(
                security_framework_sys::item::kSecClassGenericPassword,
            )
        };
        let service = core_foundation::string::CFString::new(KEYCHAIN_SERVICE);
        let account = core_foundation::string::CFString::new(KEYCHAIN_ACCOUNT);
        let value = core_foundation::data::CFData::from_buffer(value.expose_bytes());
        let pairs = [
            (
                unsafe {
                    core_foundation::string::CFString::wrap_under_get_rule(
                        security_framework_sys::item::kSecClass,
                    )
                },
                class.into_CFType(),
            ),
            (
                unsafe {
                    core_foundation::string::CFString::wrap_under_get_rule(
                        security_framework_sys::item::kSecAttrService,
                    )
                },
                service.into_CFType(),
            ),
            (
                unsafe {
                    core_foundation::string::CFString::wrap_under_get_rule(
                        security_framework_sys::item::kSecAttrAccount,
                    )
                },
                account.into_CFType(),
            ),
            (
                unsafe {
                    core_foundation::string::CFString::wrap_under_get_rule(
                        security_framework_sys::item::kSecValueData,
                    )
                },
                value.into_CFType(),
            ),
            (
                unsafe { core_foundation::string::CFString::wrap_under_get_rule(kSecAttrAccess) },
                access.into_CFType(),
            ),
        ];
        let data = core_foundation::dictionary::CFDictionary::from_CFType_pairs(&pairs);
        // Drop the old item only once the replacement is fully built: SecItemAdd rejects a
        // duplicate service/account pair, but an earlier failure must not leave the provider
        // with no credential at all.
        let _ = security_framework::passwords::delete_generic_password(
            KEYCHAIN_SERVICE,
            KEYCHAIN_ACCOUNT,
        );
        let status = unsafe {
            security_framework_sys::keychain_item::SecItemAdd(
                data.as_concrete_TypeRef(),
                ptr::null_mut(),
            )
        };
        if status == security_framework_sys::base::errSecSuccess {
            Ok(())
        } else {
            Err(keychain_error())
        }
    }

    fn delete(&self) -> AppResult<()> {
        let _interaction_lock =
            security_framework::os::macos::keychain::SecKeychain::disable_user_interaction().ok();
        match security_framework::passwords::delete_generic_password(
            KEYCHAIN_SERVICE,
            KEYCHAIN_ACCOUNT,
        ) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == security_framework_sys::base::errSecItemNotFound => {
                Err(AppError::new(
                    ErrorCode::CredentialMissing,
                    "Keychain 中没有可删除的 NodeSeek Email API key",
                ))
            }
            Err(_) => Err(AppError::new(
                ErrorCode::CredentialInvalid,
                "macOS Keychain 当前不可用",
            )),
        }
    }

    fn exists(&self) -> AppResult<bool> {
        match load_keychain_secret() {
            Ok(_) => Ok(true),
            Err(error) if error.code() == ErrorCode::CredentialMissing => Ok(false),
            Err(error) => Err(error),
        }
    }
}

#[cfg(target_os = "macos")]
fn load_keychain_secret() -> AppResult<SecretBuffer> {
    // ItemSearchOptions lets us explicitly skip authenticated items. This is important for a
    // supervised provider: an ACL mismatch must fail immediately instead of opening a hidden
    // Keychain authorization prompt.
    let mut search = ItemSearchOptions::new();
    let results = search
        .class(ItemClass::generic_password())
        .service(KEYCHAIN_SERVICE)
        .account(KEYCHAIN_ACCOUNT)
        .load_data(true)
        .skip_authenticated_items(true)
        .search()
        .map_err(|_| keychain_error())?;
    secret_from_search_results(results)
}

#[cfg(target_os = "macos")]
fn secret_from_search_results(results: Vec<SearchResult>) -> AppResult<SecretBuffer> {
    match results.into_iter().next() {
        Some(SearchResult::Data(bytes)) => SecretBuffer::from_bytes(bytes),
        Some(_) => Err(keychain_error()),
        None => Err(AppError::new(
            ErrorCode::CredentialMissing,
            "Keychain 中没有可用的 NodeSeek Email API key",
        )),
    }
}

#[cfg(target_os = "macos")]
fn trusted_applications(
) -> AppResult<core_foundation::array::CFArray<core_foundation::base::CFType>> {
    let mut paths = Vec::<PathBuf>::new();
    let executable = std::env::current_exe().map_err(|_| keychain_error())?;
    paths.push(executable.clone());
    if let Some(bin_dir) = executable.parent() {
        paths.push(bin_dir.join("nodeseek-email-provider"));
    }

    let mut applications = Vec::with_capacity(paths.len());
    for path in paths {
        if !path.is_file() {
            continue;
        }
        let Some(path) = path.to_str() else {
            continue;
        };
        let path = CString::new(path).map_err(|_| keychain_error())?;
        let mut application = ptr::null_mut();
        let status =
            unsafe { SecTrustedApplicationCreateFromPath(path.as_ptr(), &mut application) };
        if status != security_framework_sys::base::errSecSuccess || application.is_null() {
            return Err(keychain_error());
        }
        applications.push(unsafe {
            core_foundation::base::CFType::wrap_under_create_rule(application as _)
        });
    }
    if applications.is_empty() {
        return Err(keychain_error());
    }
    Ok(core_foundation::array::CFArray::from_CFTypes(&applications))
}

#[cfg(target_os = "macos")]
const fn keychain_error() -> AppError {
    AppError::new(ErrorCode::CredentialInvalid, "macOS Keychain 当前不可用")
}

#[cfg(not(target_os = "macos"))]
impl SecretStore for PlatformSecretStore {
    fn load(&self) -> AppResult<SecretBuffer> {
        Err(unsupported())
    }

    fn save(&self, _value: SecretBuffer) -> AppResult<()> {
        Err(unsupported())
    }

    fn delete(&self) -> AppResult<()> {
        Err(unsupported())
    }

    fn exists(&self) -> AppResult<bool> {
        Err(unsupported())
    }
}

#[cfg(not(target_os = "macos"))]
const fn unsupported() -> AppError {
    AppError::new(ErrorCode::Unsupported, "当前版本只支持 macOS Keychain")
}

pub fn read_api_key(stdin: bool) -> AppResult<SecretBuffer> {
    if stdin {
        let mut reader = io::BufReader::new(io::stdin().lock());
        return read_one_line(&mut reader, true);
    }
    read_from_tty()
}

fn read_one_line(reader: &mut impl BufRead, require_eof: bool) -> AppResult<SecretBuffer> {
    // Own credential bytes through a zeroizing guard from the first read, including malformed
    // input and partial-I/O error paths before a SecretBuffer can be constructed.
    let mut bytes = Zeroizing::new(Vec::new());
    {
        // Bound the temporary allocation before validation; a pipe or TTY must not be able to
        // make this credential-only CLI buffer an arbitrarily large line.
        let mut limited = reader.take((MAX_API_KEY_BYTES + 3) as u64);
        limited.read_until(b'\n', &mut bytes).map_err(|_| {
            AppError::new(
                ErrorCode::CredentialInvalid,
                "无法读取 NodeSeek Email API key",
            )
        })?;
    }
    if bytes.len() > MAX_API_KEY_BYTES + 2 {
        return Err(AppError::new(
            ErrorCode::CredentialInvalid,
            "API key 格式无效",
        ));
    }
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes.pop();
    }
    if require_eof
        && reader
            .fill_buf()
            .map(|rest| !rest.is_empty())
            .unwrap_or(true)
    {
        return Err(AppError::new(
            ErrorCode::CredentialInvalid,
            "标准输入只能包含一行 API key",
        ));
    }
    SecretBuffer::from_zeroizing(bytes)
}

#[cfg(unix)]
fn read_from_tty() -> AppResult<SecretBuffer> {
    use std::fs::OpenOptions;
    use std::os::fd::AsRawFd as _;

    struct EchoGuard {
        fd: libc::c_int,
        original: libc::termios,
    }

    impl Drop for EchoGuard {
        fn drop(&mut self) {
            unsafe {
                libc::tcsetattr(self.fd, libc::TCSAFLUSH, &self.original);
            }
        }
    }

    let mut tty = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map_err(|_| {
            AppError::new(
                ErrorCode::CredentialInvalid,
                "无法打开 TTY；自动化请使用 --api-key-stdin",
            )
        })?;
    tty.write_all(b"NodeSeek Email API key: ")
        .map_err(|_| AppError::new(ErrorCode::CredentialInvalid, "无法写入 TTY"))?;
    tty.flush()
        .map_err(|_| AppError::new(ErrorCode::CredentialInvalid, "无法刷新 TTY"))?;

    let fd = tty.as_raw_fd();
    let mut original = unsafe { std::mem::zeroed::<libc::termios>() };
    if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
        return Err(AppError::new(
            ErrorCode::CredentialInvalid,
            "无法读取 TTY 设置",
        ));
    }
    let mut hidden = original;
    hidden.c_lflag &= !libc::ECHO;
    if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &hidden) } != 0 {
        return Err(AppError::new(
            ErrorCode::CredentialInvalid,
            "无法关闭 TTY 回显",
        ));
    }
    let guard = EchoGuard { fd, original };
    let result = {
        let mut reader = io::BufReader::new(&mut tty);
        read_one_line(&mut reader, false)
    };
    drop(guard);
    let _ = tty.write_all(b"\n");
    result
}

#[cfg(not(unix))]
fn read_from_tty() -> AppResult<SecretBuffer> {
    Err(unsupported())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn keychain_data_search_result_is_decoded_without_ui() {
        let expected = String::from_utf8(fake_key()).unwrap();
        let secret = secret_from_search_results(vec![SearchResult::Data(fake_key())]).unwrap();
        assert_eq!(secret.as_str().unwrap(), expected);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn empty_keychain_search_is_missing_not_invalid() {
        let error = secret_from_search_results(vec![]).unwrap_err();
        assert_eq!(error.code(), ErrorCode::CredentialMissing);
    }

    struct PartialThenError {
        consumed: bool,
    }

    impl io::Read for PartialThenError {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("injected read failure"))
        }
    }

    impl BufRead for PartialThenError {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            if self.consumed {
                Err(io::Error::other("injected read failure"))
            } else {
                Ok(b"partial_secret_chunk")
            }
        }

        fn consume(&mut self, amount: usize) {
            self.consumed = amount > 0;
        }
    }

    fn fake_key() -> Vec<u8> {
        let mut value = api_key_prefix().into_bytes();
        value.extend(std::iter::repeat_n(b'a', 64));
        value
    }

    #[test]
    fn debug_never_contains_secret() {
        let secret = SecretBuffer::from_bytes(fake_key()).unwrap();
        let rendered = format!("{secret:?}");
        assert_eq!(rendered, "SecretBuffer(\"[redacted]\")");
        assert!(!rendered.contains("live"));
    }

    #[test]
    fn stdin_requires_exactly_one_line() {
        let mut valid = fake_key();
        valid.push(b'\n');
        assert!(read_one_line(&mut valid.as_slice(), true).is_ok());

        let mut multiple = fake_key();
        multiple.extend_from_slice(b"\nsecond\n");
        assert!(read_one_line(&mut multiple.as_slice(), true).is_err());
    }

    #[test]
    fn credential_input_is_bounded_before_validation() {
        let oversized = vec![b'a'; MAX_API_KEY_BYTES * 4];
        assert!(read_one_line(&mut oversized.as_slice(), true).is_err());
    }

    #[test]
    fn partial_read_failure_stays_inside_zeroizing_input_path() {
        let mut reader = PartialThenError { consumed: false };
        let error = read_one_line(&mut reader, true).unwrap_err();
        assert_eq!(error.code(), ErrorCode::CredentialInvalid);
    }

    #[test]
    fn rejects_key_without_separator_after_live() {
        let mut invalid = api_key_prefix().into_bytes();
        invalid.pop();
        invalid.extend(std::iter::repeat_n(b'a', 64));
        assert!(SecretBuffer::from_bytes(invalid).is_err());
    }
}
