#[allow(
    non_snake_case,
    non_upper_case_globals,
    non_camel_case_types,
    dead_code,
    clippy::all
)]
pub mod Microsoft {
    pub mod Web {
        pub mod WebView2 {
            pub mod Win32 {
                mod windows_core {
                    // 원래는 WebView2Loader.dll 을 링크 시점에 가져오지만, 단일 exe 로 내보내려고
                    // 실행 중에 LoadLibrary 로 읽도록 바꿨다(claudy-manager4 의 vendor 사본).
                    macro_rules! link_webview2 {
                        ($library:literal $abi:literal fn $name:ident ( $($arg:ident : $ty:ty),* $(,)? ) -> $ret:ty) => (
                            #[allow(non_snake_case)]
                            unsafe extern $abi fn $name($($arg : $ty),*) -> $ret {
                                type F = unsafe extern $abi fn($($ty),*) -> $ret;
                                let p = crate::loader::resolve(stringify!($name));
                                if p == 0 {
                                    // E_FAIL
                                    return windows_core::HRESULT(0x8000_4005u32 as i32);
                                }
                                let f: F = unsafe { core::mem::transmute::<usize, F>(p) };
                                unsafe { f($($arg),*) }
                            }
                        )
                    }

                    pub(crate) use ::windows_core::*;
                    pub(crate) use link_webview2 as link;
                }

                include!("bindings.rs");
            }
        }
    }
}

pub mod declared_interfaces;

#[cfg(test)]
mod test {
    use windows_core::w;

    use crate::Microsoft::Web::WebView2::Win32::*;

    #[test]
    fn compare_eq() {
        let mut result = 1;
        unsafe { CompareBrowserVersions(w!("1.0.0"), w!("1.0.0"), &mut result) }.unwrap();
        assert_eq!(0, result);
    }

    #[test]
    fn compare_lt() {
        let mut result = 0;
        unsafe { CompareBrowserVersions(w!("1.0.0"), w!("1.0.1"), &mut result) }.unwrap();
        assert_eq!(-1, result);
    }

    #[test]
    fn compare_gt() {
        let mut result = 0;
        unsafe { CompareBrowserVersions(w!("2.0.0"), w!("1.0.1"), &mut result) }.unwrap();
        assert_eq!(1, result);
    }
}

/// WebView2Loader.dll 을 실행 중에 찾아 읽는다. 경로는 set_path 로 정하고, 없으면 기본 검색 경로를 쓴다.
#[cfg(windows)]
pub mod loader {
    use std::ffi::c_void;
    use std::sync::OnceLock;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LoadLibraryW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    }

    static PATH: OnceLock<Vec<u16>> = OnceLock::new();
    static MODULE: OnceLock<usize> = OnceLock::new();

    pub fn set_path(p: &std::path::Path) {
        use std::os::windows::ffi::OsStrExt;
        let mut v: Vec<u16> = p.as_os_str().encode_wide().collect();
        v.push(0);
        let _ = PATH.set(v);
    }

    fn module() -> usize {
        *MODULE.get_or_init(|| {
            let default: Vec<u16> = "WebView2Loader.dll\0".encode_utf16().collect();
            let name = PATH.get().unwrap_or(&default);
            unsafe { LoadLibraryW(name.as_ptr()) as usize }
        })
    }

    /// 함수 주소. 읽지 못하면 0.
    pub fn resolve(name: &str) -> usize {
        let m = module();
        if m == 0 {
            return 0;
        }
        let mut n = name.as_bytes().to_vec();
        n.push(0);
        unsafe { GetProcAddress(m as *mut c_void, n.as_ptr()) as usize }
    }
}
