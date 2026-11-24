//! OS system proxy detection (macOS SystemConfiguration, Windows Registry, Linux/env fallback)
//! and macOS `SCDynamicStore` push-based change watcher via `dispatch2`.

#[cfg(any(target_os = "macos", test))]
use crate::model::{normalize_host, ProxyEndpoint};
use crate::model::{parse_proxy_url_with_default_scheme, ProxyProtocol, SystemProxySnapshot};

/// Detect current system proxy settings for this OS, falling back to environment variables.
pub fn detect_system_proxy() -> SystemProxySnapshot {
    #[cfg(target_os = "macos")]
    {
        macos::detect()
    }
    #[cfg(target_os = "windows")]
    {
        windows::detect()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        detect_from_current_env()
    }
}

/// Read system proxy settings solely from standard proxy environment variables.
pub fn detect_from_current_env() -> SystemProxySnapshot {
    system_proxy_from_env(|key| std::env::var(key).ok())
}

/// Parse proxy settings from environment variables (`ALL_PROXY`, `HTTPS_PROXY`, `HTTP_PROXY`, `NO_PROXY`).
pub fn system_proxy_from_env(get: impl Fn(&str) -> Option<String>) -> SystemProxySnapshot {
    let read_pair = |upper: &str, lower: &str| -> Option<String> {
        get(upper)
            .filter(|v| !v.trim().is_empty())
            .or_else(|| get(lower).filter(|v| !v.trim().is_empty()))
    };

    let mut http = read_pair("HTTP_PROXY", "http_proxy")
        .and_then(|raw| parse_proxy_url_with_default_scheme(&raw, ProxyProtocol::Http).ok());
    let mut https = read_pair("HTTPS_PROXY", "https_proxy")
        .and_then(|raw| parse_proxy_url_with_default_scheme(&raw, ProxyProtocol::Http).ok());
    let mut socks5 = None;

    if let Some(raw_all) = read_pair("ALL_PROXY", "all_proxy") {
        if let Ok(ep) = parse_proxy_url_with_default_scheme(&raw_all, ProxyProtocol::Socks5h) {
            match ep.protocol {
                ProxyProtocol::Socks5 | ProxyProtocol::Socks5h => {
                    socks5 = Some(ep);
                }
                ProxyProtocol::Http | ProxyProtocol::Https => {
                    if http.is_none() {
                        http = Some(ep.clone());
                    }
                    if https.is_none() {
                        https = Some(ep);
                    }
                }
            }
        }
    }

    let bypass = read_no_proxy_env(&get);
    let source = if http.is_some() || https.is_some() || socks5.is_some() {
        "env".to_string()
    } else {
        "none".to_string()
    };

    SystemProxySnapshot {
        http,
        https,
        socks5,
        bypass,
        exclude_simple_hostnames: false,
        pac_url: None,
        source,
    }
}

fn read_no_proxy_env(get: &impl Fn(&str) -> Option<String>) -> Vec<String> {
    let raw = get("NO_PROXY")
        .filter(|v| !v.trim().is_empty())
        .or_else(|| get("no_proxy").filter(|v| !v.trim().is_empty()));
    match raw {
        Some(value) => split_bypass_entries(&value),
        None => Vec::new(),
    }
}

fn split_bypass_entries(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    for entry in raw.split([',', ';']) {
        let trimmed = entry.trim();
        if !trimmed.is_empty() && !out.iter().any(|existing| existing == trimmed) {
            out.push(trimmed.to_string());
        }
    }
    out
}

#[cfg(any(target_os = "macos", target_os = "windows", test))]
fn merge_env_no_proxy(snapshot: &mut SystemProxySnapshot, get: impl Fn(&str) -> Option<String>) {
    for entry in read_no_proxy_env(&get) {
        if !snapshot.bypass.iter().any(|existing| existing == &entry) {
            snapshot.bypass.push(entry);
        }
    }
}

/// Pure-Rust parser for Windows `Internet Settings` registry values (tested on all platforms).
#[cfg(any(target_os = "windows", test))]
pub(crate) fn parse_windows_registry_proxy(
    proxy_enable: bool,
    proxy_server: Option<&str>,
    proxy_override: Option<&str>,
    auto_config_url: Option<&str>,
) -> SystemProxySnapshot {
    let mut snapshot = SystemProxySnapshot {
        source: "windows_registry".to_string(),
        ..Default::default()
    };

    if let Some(pac) = auto_config_url.map(str::trim).filter(|s| !s.is_empty()) {
        snapshot.pac_url = Some(pac.to_string());
    }

    if !proxy_enable {
        return snapshot;
    }

    if let Some(raw_override) = proxy_override {
        snapshot.bypass = split_bypass_entries(raw_override);
        if snapshot
            .bypass
            .iter()
            .any(|entry| entry.eq_ignore_ascii_case("<local>"))
        {
            snapshot.exclude_simple_hostnames = true;
        }
    }

    let Some(server) = proxy_server.map(str::trim).filter(|s| !s.is_empty()) else {
        return snapshot;
    };

    if !server.contains('=') {
        // Single global `host:port` applies to HTTP & HTTPS as HTTP CONNECT.
        if let Ok(ep) = parse_proxy_url_with_default_scheme(server, ProxyProtocol::Http) {
            snapshot.http = Some(ep.clone());
            snapshot.https = Some(ep);
        }
        return snapshot;
    }

    for entry in server.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let Some((scheme, value)) = entry.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match scheme.trim().to_ascii_lowercase().as_str() {
            "http" => {
                if let Ok(ep) = parse_proxy_url_with_default_scheme(value, ProxyProtocol::Http) {
                    snapshot.http = Some(ep);
                }
            }
            "https" => {
                if let Ok(ep) = parse_proxy_url_with_default_scheme(value, ProxyProtocol::Http) {
                    snapshot.https = Some(ep);
                }
            }
            "socks" | "socks5" | "socks5h" => {
                if let Ok(ep) = parse_proxy_url_with_default_scheme(value, ProxyProtocol::Socks5h) {
                    snapshot.socks5 = Some(ep);
                }
            }
            _ => {}
        }
    }

    snapshot
}

#[cfg(any(target_os = "macos", test))]
fn make_os_endpoint(
    protocol: ProxyProtocol,
    host: Option<String>,
    port: Option<i64>,
    user: Option<String>,
) -> Option<ProxyEndpoint> {
    let host = normalize_host(host?.trim()).ok()?;
    let port_raw = port?;
    let port = u16::try_from(port_raw).ok().filter(|p| *p != 0)?;
    let username = user
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty());
    Some(ProxyEndpoint {
        protocol,
        host,
        port,
        username,
        password: None,
    })
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;
    use std::ptr;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use core_foundation::array::CFArray;
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;
    use core_foundation_sys::array::CFArrayRef;
    use core_foundation_sys::base::{Boolean, CFAllocatorRef, CFIndex, CFRelease};
    use core_foundation_sys::dictionary::CFDictionaryRef;
    use core_foundation_sys::string::CFStringRef;
    use dispatch2::{DispatchQueue, DispatchRetained, DispatchTime};

    use super::{detect_from_current_env, make_os_endpoint, merge_env_no_proxy};
    use crate::model::{ProxyProtocol, SystemProxySnapshot};

    const GLOBAL_PROXIES_KEY: &str = "State:/Network/Global/Proxies";
    const WATCHER_DEBOUNCE: Duration = Duration::from_millis(500);

    type SCDynamicStoreRef = *const c_void;
    type SCDynamicStoreCallBack = extern "C" fn(SCDynamicStoreRef, CFArrayRef, *mut c_void);

    #[repr(C)]
    struct SCDynamicStoreContext {
        version: CFIndex,
        info: *mut c_void,
        retain: Option<extern "C" fn(*const c_void) -> *const c_void>,
        release: Option<extern "C" fn(*const c_void)>,
        copy_description: Option<extern "C" fn(*const c_void) -> CFStringRef>,
    }

    #[link(name = "SystemConfiguration", kind = "framework")]
    extern "C" {
        fn SCDynamicStoreCopyProxies(store: SCDynamicStoreRef) -> CFDictionaryRef;
        fn SCDynamicStoreCreate(
            allocator: CFAllocatorRef,
            name: CFStringRef,
            callout: Option<SCDynamicStoreCallBack>,
            context: *mut SCDynamicStoreContext,
        ) -> SCDynamicStoreRef;
        fn SCDynamicStoreSetNotificationKeys(
            store: SCDynamicStoreRef,
            keys: CFArrayRef,
            patterns: CFArrayRef,
        ) -> Boolean;
        fn SCDynamicStoreSetDispatchQueue(
            store: SCDynamicStoreRef,
            queue: *const c_void,
        ) -> Boolean;
    }

    pub(super) fn detect() -> SystemProxySnapshot {
        let mut os_snapshot = read_system_configuration_proxies();
        merge_env_no_proxy(&mut os_snapshot, |k| std::env::var(k).ok());
        if os_snapshot.has_any_proxy() || os_snapshot.pac_url.is_some() {
            return os_snapshot;
        }
        let env_snapshot = detect_from_current_env();
        if env_snapshot.has_any_proxy() {
            return env_snapshot;
        }
        os_snapshot
    }

    fn read_system_configuration_proxies() -> SystemProxySnapshot {
        let mut snapshot = SystemProxySnapshot {
            source: "macos_system_configuration".to_string(),
            ..Default::default()
        };

        // SAFETY: passing NULL uses a one-shot default dynamic store session; returns +1 retained CFDictionary or NULL.
        let raw_dict = unsafe { SCDynamicStoreCopyProxies(ptr::null()) };
        if raw_dict.is_null() {
            return snapshot;
        }
        // SAFETY: `raw_dict` is a non-null +1 retained CFDictionaryRef from Copy rule.
        let dict: CFDictionary<CFString, CFType> =
            unsafe { CFDictionary::wrap_under_create_rule(raw_dict) };

        if dict_bool(&dict, "HTTPEnable") {
            snapshot.http = make_os_endpoint(
                ProxyProtocol::Http,
                dict_string(&dict, "HTTPProxy"),
                dict_i64(&dict, "HTTPPort"),
                dict_string(&dict, "HTTPUser"),
            );
        }
        if dict_bool(&dict, "HTTPSEnable") {
            // macOS "Secure Web Proxy (HTTPS)" uses plain HTTP CONNECT to the proxy server.
            snapshot.https = make_os_endpoint(
                ProxyProtocol::Http,
                dict_string(&dict, "HTTPSProxy"),
                dict_i64(&dict, "HTTPSPort"),
                dict_string(&dict, "HTTPSUser"),
            );
        }
        if dict_bool(&dict, "SOCKSEnable") {
            snapshot.socks5 = make_os_endpoint(
                ProxyProtocol::Socks5h,
                dict_string(&dict, "SOCKSProxy"),
                dict_i64(&dict, "SOCKSPort"),
                dict_string(&dict, "SOCKSUser"),
            );
        }

        snapshot.exclude_simple_hostnames = dict_bool(&dict, "ExcludeSimpleHostnames");
        snapshot.bypass = dict_string_array(&dict, "ExceptionsList");

        if dict_bool(&dict, "ProxyAutoConfigEnable") {
            snapshot.pac_url = dict_string(&dict, "ProxyAutoConfigURLString")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
        }

        snapshot
    }

    fn dict_i64(dict: &CFDictionary<CFString, CFType>, key: &'static str) -> Option<i64> {
        let cf_key = CFString::from_static_string(key);
        let value = dict.find(&cf_key)?;
        let number = value.downcast::<CFNumber>()?;
        number.to_i64()
    }

    fn dict_bool(dict: &CFDictionary<CFString, CFType>, key: &'static str) -> bool {
        dict_i64(dict, key).is_some_and(|v| v != 0)
    }

    fn dict_string(dict: &CFDictionary<CFString, CFType>, key: &'static str) -> Option<String> {
        let cf_key = CFString::from_static_string(key);
        let value = dict.find(&cf_key)?;
        let string = value.downcast::<CFString>()?;
        Some(string.to_string())
    }

    fn dict_string_array(dict: &CFDictionary<CFString, CFType>, key: &'static str) -> Vec<String> {
        let cf_key = CFString::from_static_string(key);
        let Some(value) = dict.find(&cf_key) else {
            return Vec::new();
        };
        let Some(array) = value.downcast::<CFArray>() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for item in array.iter() {
            // SAFETY: elements of `ExceptionsList` CFArray are valid CFTypeRef pointers borrowed from `array`.
            let cf_type = unsafe { CFType::wrap_under_get_rule(*item) };
            if let Some(s) = cf_type.downcast::<CFString>() {
                let text = s.to_string();
                let trimmed = text.trim();
                if !trimmed.is_empty() && !out.iter().any(|existing| existing == trimmed) {
                    out.push(trimmed.to_string());
                }
            }
        }
        out
    }

    struct WatcherInner {
        queue: DispatchRetained<DispatchQueue>,
        pending_seq: AtomicU64,
        stopped: AtomicBool,
        last_snapshot: Mutex<SystemProxySnapshot>,
        reader: Arc<dyn Fn() -> SystemProxySnapshot + Send + Sync>,
        on_change: Arc<dyn Fn(SystemProxySnapshot) + Send + Sync>,
    }

    extern "C" fn retain_context(info: *const c_void) -> *const c_void {
        if !info.is_null() {
            // SAFETY: `info` was created by `Arc::into_raw` on `Arc<WatcherInner>`.
            unsafe { Arc::increment_strong_count(info as *const WatcherInner) };
        }
        info
    }

    extern "C" fn release_context(info: *const c_void) {
        if !info.is_null() {
            // SAFETY: balances `Arc::into_raw` / `retain_context`.
            unsafe { Arc::decrement_strong_count(info as *const WatcherInner) };
        }
    }

    extern "C" fn dynamic_store_callback(
        _store: SCDynamicStoreRef,
        _changed_keys: CFArrayRef,
        info: *mut c_void,
    ) {
        if info.is_null() {
            return;
        }
        // SAFETY: `SCDynamicStoreContext` retains `Arc<WatcherInner>` for the duration of callbacks.
        let inner: Arc<WatcherInner> = unsafe {
            let ptr = info as *const WatcherInner;
            Arc::increment_strong_count(ptr);
            Arc::from_raw(ptr)
        };
        if inner.stopped.load(Ordering::Acquire) {
            return;
        }
        let seq = inner.pending_seq.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        let when = DispatchTime::try_from(WATCHER_DEBOUNCE).unwrap_or(DispatchTime::NOW);
        let delayed = Arc::clone(&inner);
        let _ = inner.queue.after(when, move || {
            if delayed.stopped.load(Ordering::Acquire) {
                return;
            }
            if delayed.pending_seq.load(Ordering::Acquire) != seq {
                return;
            }
            let next = (delayed.reader)();
            let changed = {
                let mut guard = delayed.last_snapshot.lock().unwrap();
                if *guard == next {
                    false
                } else {
                    *guard = next.clone();
                    true
                }
            };
            if changed && !delayed.stopped.load(Ordering::Acquire) {
                (delayed.on_change)(next);
            }
        });
    }

    /// Push-based macOS `SCDynamicStore` watcher for `State:/Network/Global/Proxies`.
    ///
    /// Uses `dispatch2` serial queue with 500ms debounce and snapshot equality check (zero polling).
    pub struct SystemProxyWatcher {
        store: SCDynamicStoreRef,
        inner: Arc<WatcherInner>,
    }

    // SAFETY: `SCDynamicStoreRef` lifecycle calls (`SCDynamicStoreSetDispatchQueue` + `CFRelease`)
    // are thread-safe and executed once during `Drop`.
    unsafe impl Send for SystemProxyWatcher {}
    unsafe impl Sync for SystemProxyWatcher {}

    impl SystemProxyWatcher {
        /// Start watching `State:/Network/Global/Proxies`. Returns `None` if SystemConfiguration
        /// refuses the store session (never panics).
        pub fn start(
            initial: SystemProxySnapshot,
            reader: Arc<dyn Fn() -> SystemProxySnapshot + Send + Sync>,
            on_change: Arc<dyn Fn(SystemProxySnapshot) + Send + Sync>,
        ) -> Option<Self> {
            let queue = DispatchQueue::new("com.featherpost.proxy-watcher", None);
            let queue_ptr = DispatchRetained::as_ptr(&queue).as_ptr().cast::<c_void>();
            let inner = Arc::new(WatcherInner {
                queue,
                pending_seq: AtomicU64::new(0),
                stopped: AtomicBool::new(false),
                last_snapshot: Mutex::new(initial),
                reader,
                on_change,
            });

            let raw_info = Arc::into_raw(Arc::clone(&inner)) as *mut c_void;
            let mut context = SCDynamicStoreContext {
                version: 0,
                info: raw_info,
                retain: Some(retain_context),
                release: Some(release_context),
                copy_description: None,
            };
            let name = CFString::from_static_string("featherpost-proxy-watcher");
            // SAFETY: valid CFStringRef and stack-allocated SCDynamicStoreContext (copied by SCDynamicStoreCreate).
            let store = unsafe {
                SCDynamicStoreCreate(
                    ptr::null(),
                    name.as_concrete_TypeRef(),
                    Some(dynamic_store_callback),
                    &mut context,
                )
            };
            // Release our temporary ownership of `raw_info`; if `SCDynamicStoreCreate` succeeded,
            // it invoked `retain_context` to take its own strong reference.
            // SAFETY: balances `Arc::into_raw` above.
            unsafe { Arc::decrement_strong_count(raw_info as *const WatcherInner) };

            if store.is_null() {
                return None;
            }

            let keys = CFArray::from_CFTypes(&[CFString::from_static_string(GLOBAL_PROXIES_KEY)]);
            // SAFETY: `store` and `keys` are valid CF objects.
            let keys_ok = unsafe {
                SCDynamicStoreSetNotificationKeys(
                    store,
                    keys.as_concrete_TypeRef(),
                    ptr::null(),
                )
            };
            if keys_ok == 0 {
                // SAFETY: `store` is a valid +1 retained SCDynamicStoreRef.
                unsafe { CFRelease(store) };
                return None;
            }

            // SAFETY: `store` is valid and `queue_ptr` is a valid retained `dispatch_queue_t`.
            let queue_ok = unsafe { SCDynamicStoreSetDispatchQueue(store, queue_ptr) };
            if queue_ok == 0 {
                // SAFETY: `store` is a valid +1 retained SCDynamicStoreRef.
                unsafe { CFRelease(store) };
                return None;
            }

            Some(Self { store, inner })
        }
    }

    impl Drop for SystemProxyWatcher {
        fn drop(&mut self) {
            self.inner.stopped.store(true, Ordering::Release);
            // SAFETY: detaching the dispatch queue and releasing the dynamic store session.
            unsafe {
                SCDynamicStoreSetDispatchQueue(self.store, ptr::null());
                CFRelease(self.store);
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub use macos::SystemProxyWatcher;

#[cfg(target_os = "windows")]
mod windows {
    use std::ptr;

    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
        REG_DWORD, REG_SZ, REG_VALUE_TYPE,
    };

    use super::{detect_from_current_env, merge_env_no_proxy, parse_windows_registry_proxy};
    use crate::model::SystemProxySnapshot;

    pub(super) fn detect() -> SystemProxySnapshot {
        let mut os_snapshot = read_registry_proxies();
        merge_env_no_proxy(&mut os_snapshot, |k| std::env::var(k).ok());
        if os_snapshot.has_any_proxy() || os_snapshot.pac_url.is_some() {
            return os_snapshot;
        }
        let env_snapshot = detect_from_current_env();
        if env_snapshot.has_any_proxy() {
            return env_snapshot;
        }
        os_snapshot
    }

    fn wide_null(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn read_registry_proxies() -> SystemProxySnapshot {
        let subkey = wide_null(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings");
        let mut key: HKEY = ptr::null_mut();
        // SAFETY: `subkey` is NUL-terminated UTF-16 and `key` is a valid output slot.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                0,
                KEY_READ,
                &mut key,
            )
        };
        if status != ERROR_SUCCESS {
            return SystemProxySnapshot {
                source: "windows_registry".to_string(),
                ..Default::default()
            };
        }

        let proxy_enable = read_reg_dword(key, "ProxyEnable").is_some_and(|v| v != 0);
        let proxy_server = read_reg_string(key, "ProxyServer");
        let proxy_override = read_reg_string(key, "ProxyOverride");
        let auto_config_url = read_reg_string(key, "AutoConfigURL");

        // SAFETY: `key` was successfully opened above.
        unsafe { RegCloseKey(key) };

        parse_windows_registry_proxy(
            proxy_enable,
            proxy_server.as_deref(),
            proxy_override.as_deref(),
            auto_config_url.as_deref(),
        )
    }

    fn read_reg_dword(key: HKEY, name: &str) -> Option<u32> {
        let wide_name = wide_null(name);
        let mut value_type: REG_VALUE_TYPE = 0;
        let mut data: u32 = 0;
        let mut data_len: u32 = std::mem::size_of::<u32>() as u32;
        // SAFETY: output buffer matches `data_len`.
        let status = unsafe {
            RegQueryValueExW(
                key,
                wide_name.as_ptr(),
                ptr::null(),
                &mut value_type,
                (&mut data as *mut u32).cast::<u8>(),
                &mut data_len,
            )
        };
        if status == ERROR_SUCCESS && value_type == REG_DWORD {
            Some(data)
        } else {
            None
        }
    }

    fn read_reg_string(key: HKEY, name: &str) -> Option<String> {
        let wide_name = wide_null(name);
        let mut value_type: REG_VALUE_TYPE = 0;
        let mut byte_len: u32 = 0;
        // SAFETY: size query with NULL data pointer.
        let status = unsafe {
            RegQueryValueExW(
                key,
                wide_name.as_ptr(),
                ptr::null(),
                &mut value_type,
                ptr::null_mut(),
                &mut byte_len,
            )
        };
        if status != ERROR_SUCCESS || value_type != REG_SZ || byte_len == 0 || byte_len > 64 * 1024
        {
            return None;
        }
        let u16_len = (byte_len as usize).div_ceil(2);
        let mut buf = vec![0u16; u16_len];
        let mut actual_len = (buf.len() * 2) as u32;
        // SAFETY: `buf` has `actual_len` bytes capacity.
        let status = unsafe {
            RegQueryValueExW(
                key,
                wide_name.as_ptr(),
                ptr::null(),
                &mut value_type,
                buf.as_mut_ptr().cast::<u8>(),
                &mut actual_len,
            )
        };
        if status != ERROR_SUCCESS || value_type != REG_SZ {
            return None;
        }
        let units = (actual_len as usize / 2).min(buf.len());
        let slice = &buf[..units];
        let end = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
        String::from_utf16(&slice[..end]).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn env_parser_handles_all_http_https_and_no_proxy() {
        let map: HashMap<&str, &str> = HashMap::from([
            ("HTTP_PROXY", "http://127.0.0.1:8080"),
            ("HTTPS_PROXY", "https://alice:secret@proxy.corp:8443"),
            ("ALL_PROXY", "socks5h://127.0.0.1:7890"),
            ("NO_PROXY", "localhost, .corp.example; 10.0.0.0/8"),
        ]);
        let snap = system_proxy_from_env(|k| map.get(k).map(|v| (*v).to_string()));
        assert_eq!(snap.source, "env");
        assert_eq!(snap.http.as_ref().unwrap().port, 8080);
        assert_eq!(snap.https.as_ref().unwrap().protocol, ProxyProtocol::Https);
        assert_eq!(snap.https.as_ref().unwrap().username.as_deref(), Some("alice"));
        assert_eq!(snap.socks5.as_ref().unwrap().protocol, ProxyProtocol::Socks5h);
        assert_eq!(snap.socks5.as_ref().unwrap().port, 7890);
        assert_eq!(snap.bypass, vec!["localhost", ".corp.example", "10.0.0.0/8"]);

        // Secret stripping removes password from effective_system views.
        let public = snap.without_secrets();
        assert_eq!(public.https.as_ref().unwrap().password, None);
    }

    #[test]
    fn windows_registry_parser_handles_single_and_per_protocol_values() {
        let single = parse_windows_registry_proxy(
            true,
            Some("127.0.0.1:7890"),
            Some("*.local;10.0.0.0/8;<local>"),
            Some("http://wpad.corp/proxy.pac"),
        );
        assert_eq!(single.http.as_ref().unwrap().port, 7890);
        assert_eq!(single.https.as_ref().unwrap().port, 7890);
        assert_eq!(single.socks5, None);
        assert!(single.exclude_simple_hostnames);
        assert_eq!(single.pac_url.as_deref(), Some("http://wpad.corp/proxy.pac"));

        let multi = parse_windows_registry_proxy(
            true,
            Some("http=10.0.0.1:8080;https=10.0.0.1:8443;socks=10.0.0.1:1080"),
            None,
            None,
        );
        assert_eq!(multi.http.as_ref().unwrap().port, 8080);
        assert_eq!(multi.https.as_ref().unwrap().port, 8443);
        assert_eq!(multi.https.as_ref().unwrap().protocol, ProxyProtocol::Http);
        assert_eq!(multi.socks5.as_ref().unwrap().port, 1080);
        assert_eq!(multi.socks5.as_ref().unwrap().protocol, ProxyProtocol::Socks5h);

        let disabled = parse_windows_registry_proxy(
            false,
            Some("127.0.0.1:7890"),
            Some("<local>"),
            Some("https://pac.example/a.pac"),
        );
        assert!(!disabled.has_any_proxy());
        assert_eq!(disabled.pac_url.as_deref(), Some("https://pac.example/a.pac"));
    }

    #[test]
    fn merge_no_proxy_deduplicates_entries() {
        let mut snap = SystemProxySnapshot {
            bypass: vec!["*.local".into()],
            ..Default::default()
        };
        merge_env_no_proxy(&mut snap, |k| {
            (k == "NO_PROXY").then(|| "*.local, .corp.example".into())
        });
        assert_eq!(snap.bypass, vec!["*.local", ".corp.example"]);
    }

    #[test]
    fn make_os_endpoint_validates_host_and_port() {
        assert!(make_os_endpoint(ProxyProtocol::Http, Some("".into()), Some(8080), None).is_none());
        assert!(make_os_endpoint(ProxyProtocol::Http, Some("127.0.0.1".into()), Some(0), None).is_none());
        let ep = make_os_endpoint(
            ProxyProtocol::Socks5h,
            Some("127.0.0.1".into()),
            Some(7890),
            Some("u".into()),
        )
        .unwrap();
        assert_eq!(ep.port, 7890);
        assert_eq!(ep.username.as_deref(), Some("u"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_detect_and_watcher_lifecycle_do_not_panic() {
        use std::sync::Arc;

        let snap = detect_system_proxy();
        assert!(!snap.source.is_empty());
        let watcher = SystemProxyWatcher::start(
            snap,
            Arc::new(detect_system_proxy),
            Arc::new(|_| {}),
        );
        drop(watcher);
    }
}
