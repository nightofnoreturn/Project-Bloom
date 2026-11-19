use serde::{Deserialize, Serialize};

/// Live Samsung phone/tablet detected over standard Windows WPD (MTP) without extra drivers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectedSamsungDevice {
    pub device_id: String,
    pub friendly_name: String,
    pub manufacturer: String,
    pub model: String,
    pub firmware_build: Option<String>,
    pub battery_percent: Option<u8>,
    pub serial: Option<String>,
    pub usb_serial: Option<String>,
}

impl ConnectedSamsungDevice {
    /// Returns the user-visible device name (falling back to the `SM-...` model string).
    pub fn display_title(&self) -> &str {
        if self.friendly_name.trim().is_empty() {
            &self.model
        } else {
            &self.friendly_name
        }
    }
}

/// Extracts the USB hardware serial number segment from a Windows WPD PnP device instance path,
/// ignoring Windows composite `&` virtual interface identifiers.
pub fn parse_usb_serial_from_pnp_id(pnp_id: &str) -> Option<String> {
    let seg = pnp_id.split('#').nth(2)?.trim();
    if seg.is_empty() || seg.contains('&') {
        return None;
    }
    Some(seg.to_ascii_uppercase())
}

/// Identifies Samsung mobile devices from WPD manufacturer, model code, or USB VID (`04E8`).
pub fn is_samsung_device(manufacturer: &str, model: &str, pnp_id: &str) -> bool {
    let mfg = manufacturer.trim().to_ascii_lowercase();
    let mdl = model.trim().to_ascii_uppercase();
    let pnp = pnp_id.to_ascii_lowercase();

    mfg.contains("samsung")
        || pnp.contains("vid_04e8")
        || mdl.starts_with("SM-")
        || mdl.starts_with("GT-")
        || mdl.starts_with("SCH-")
        || mdl.starts_with("SGH-")
        || mdl.starts_with("SPH-")
        || mdl.starts_with("SC-")
}

/// Enumerates connected Samsung MTP/WPD devices on Windows; returns empty on other platforms.
pub fn scan_connected_samsung_devices() -> Vec<ConnectedSamsungDevice> {
    #[cfg(windows)]
    {
        win_wpd::scan()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

#[cfg(windows)]
mod win_wpd {
    use std::ffi::c_void;
    use std::ptr::{null, null_mut};

    use super::{is_samsung_device, parse_usb_serial_from_pnp_id, ConnectedSamsungDevice};

    const COINIT_MULTITHREADED: u32 = 0x0;
    const CLSCTX_INPROC_SERVER: u32 = 0x1;
    const RPC_E_CHANGED_MODE: i32 = 0x8001_0106u32 as i32;

    const VT_I4: u16 = 3;
    const VT_UI1: u16 = 17;
    const VT_UI2: u16 = 18;
    const VT_UI4: u16 = 19;
    const VT_LPWSTR: u16 = 31;

    #[repr(C)]
    #[derive(Clone, Copy, PartialEq, Eq, Default)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    impl Guid {
        const fn new(data1: u32, data2: u16, data3: u16, data4: [u8; 8]) -> Self {
            Self {
                data1,
                data2,
                data3,
                data4,
            }
        }
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct PropertyKey {
        fmtid: Guid,
        pid: u32,
    }

    // Matches Windows `PROPVARIANT`: `vt` at offset 0, union payload at offset 8 (24 bytes on x64).
    #[repr(C)]
    #[derive(Default)]
    struct PropVariant {
        vt: u16,
        reserved1: u16,
        reserved2: u16,
        reserved3: u16,
        data: [usize; 2],
    }

    const CLSID_PORTABLE_DEVICE_MANAGER: Guid =
        Guid::new(0x0af1_0cec, 0x2ecd, 0x4b92, [0x95, 0x81, 0x34, 0xf6, 0xae, 0x06, 0x37, 0xf3]);
    const IID_IPORTABLE_DEVICE_MANAGER: Guid =
        Guid::new(0xa156_7595, 0x4c2f, 0x4574, [0xa6, 0xfa, 0xec, 0xef, 0x91, 0x7b, 0x9a, 0x40]);

    const CLSID_PORTABLE_DEVICE: Guid =
        Guid::new(0x728a_21c5, 0x3d9e, 0x48d7, [0x98, 0x10, 0x86, 0x48, 0x48, 0xf0, 0xf4, 0x04]);
    const IID_IPORTABLE_DEVICE: Guid =
        Guid::new(0x625e_2df8, 0x6392, 0x4cf0, [0x9a, 0xd1, 0x3c, 0xfa, 0x5f, 0x17, 0x77, 0x5c]);

    const CLSID_PORTABLE_DEVICE_VALUES: Guid =
        Guid::new(0x0c15_d503, 0xd017, 0x47ce, [0x90, 0x16, 0x7b, 0x3f, 0x97, 0x87, 0x21, 0xcc]);
    const IID_IPORTABLE_DEVICE_VALUES: Guid =
        Guid::new(0x6848_f6f2, 0x3155, 0x4f86, [0xb6, 0xf5, 0x26, 0x3e, 0xee, 0xab, 0x31, 0x43]);

    const WPD_CLIENT_INFO_FMTID: Guid =
        Guid::new(0x204d_9f0c, 0x2292, 0x4080, [0x9f, 0x42, 0x40, 0x66, 0x4e, 0x70, 0xf8, 0x59]);
    const WPD_DEVICE_PROPS_FMTID: Guid =
        Guid::new(0x26d4_979a, 0xe643, 0x4626, [0x9e, 0x2b, 0x73, 0x6d, 0xc0, 0xc9, 0x2f, 0xdc]);
    const WPD_OBJECT_PROPS_FMTID: Guid =
        Guid::new(0xef6b_490d, 0x5cd8, 0x437a, [0xaf, 0xfc, 0xda, 0x8b, 0x60, 0xee, 0x4a, 0x3c]);

    #[link(name = "ole32")]
    extern "system" {
        fn CoInitializeEx(pv_reserved: *mut c_void, dw_co_init: u32) -> i32;
        fn CoUninitialize();
        fn CoCreateInstance(
            rclsid: *const Guid,
            p_unk_outer: *mut c_void,
            dw_cls_context: u32,
            riid: *const Guid,
            ppv: *mut *mut c_void,
        ) -> i32;
        fn CoTaskMemFree(pv: *mut c_void);
        fn PropVariantClear(pvar: *mut PropVariant) -> i32;
    }

    struct ComGuard {
        should_uninit: bool,
    }

    impl ComGuard {
        fn init_multithreaded() -> Option<Self> {
            let hr = unsafe { CoInitializeEx(null_mut(), COINIT_MULTITHREADED) };
            if hr >= 0 {
                Some(Self {
                    should_uninit: true,
                })
            } else if hr == RPC_E_CHANGED_MODE {
                Some(Self {
                    should_uninit: false,
                })
            } else {
                None
            }
        }
    }

    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.should_uninit {
                unsafe { CoUninitialize() };
            }
        }
    }

    struct ComPtr(*mut c_void);

    impl Drop for ComPtr {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let release: unsafe extern "system" fn(*mut c_void) -> u32 =
                        vtable_entry(self.0, 2);
                    let _ = release(self.0);
                }
            }
        }
    }

    unsafe fn vtable_entry<T: Copy>(obj: *mut c_void, slot: usize) -> T {
        let vtbl = *(obj as *const *const usize);
        std::ptr::read(vtbl.add(slot).cast::<T>())
    }

    unsafe fn co_create(clsid: &Guid, iid: &Guid) -> Option<ComPtr> {
        let mut ptr: *mut c_void = null_mut();
        let hr = CoCreateInstance(clsid, null_mut(), CLSCTX_INPROC_SERVER, iid, &mut ptr);
        if hr < 0 || ptr.is_null() {
            None
        } else {
            Some(ComPtr(ptr))
        }
    }

    fn to_wide_null(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    unsafe fn wide_ptr_to_string(ptr: *const u16) -> String {
        if ptr.is_null() {
            return String::new();
        }
        let mut len = 0usize;
        while len < 4096 && *ptr.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
            .trim()
            .to_owned()
    }

    pub fn scan() -> Vec<ConnectedSamsungDevice> {
        let Some(_com) = ComGuard::init_multithreaded() else {
            return Vec::new();
        };

        let Some(manager) =
            (unsafe { co_create(&CLSID_PORTABLE_DEVICE_MANAGER, &IID_IPORTABLE_DEVICE_MANAGER) })
        else {
            return Vec::new();
        };

        let pnp_ids = enumerate_wpd_device_ids(&manager);
        let mut out = Vec::new();
        for pnp_id in pnp_ids {
            if let Some(dev) = read_samsung_device_props(&pnp_id) {
                out.push(dev);
            }
        }
        out
    }

    fn enumerate_wpd_device_ids(manager: &ComPtr) -> Vec<String> {
        unsafe {
            type GetDevicesFn = unsafe extern "system" fn(*mut c_void, *mut *mut u16, *mut u32) -> i32;
            let get_devices: GetDevicesFn = vtable_entry(manager.0, 3);

            let mut count: u32 = 0;
            let hr = get_devices(manager.0, null_mut(), &mut count);
            if hr < 0 || count == 0 {
                return Vec::new();
            }

            let mut raw_ids: Vec<*mut u16> = vec![null_mut(); count as usize];
            let hr = get_devices(manager.0, raw_ids.as_mut_ptr(), &mut count);

            let mut ids = Vec::with_capacity(count as usize);
            for raw in raw_ids {
                if !raw.is_null() {
                    if hr >= 0 {
                        let s = wide_ptr_to_string(raw);
                        if !s.is_empty() {
                            ids.push(s);
                        }
                    }
                    CoTaskMemFree(raw.cast());
                }
            }
            ids
        }
    }

    fn read_samsung_device_props(pnp_id: &str) -> Option<ConnectedSamsungDevice> {
        unsafe {
            let device = co_create(&CLSID_PORTABLE_DEVICE, &IID_IPORTABLE_DEVICE)?;
            let client_info =
                co_create(&CLSID_PORTABLE_DEVICE_VALUES, &IID_IPORTABLE_DEVICE_VALUES)?;

            type SetStringFn =
                unsafe extern "system" fn(*mut c_void, *const PropertyKey, *const u16) -> i32;
            let set_string: SetStringFn = vtable_entry(client_info.0, 7);
            let client_name_key = PropertyKey {
                fmtid: WPD_CLIENT_INFO_FMTID,
                pid: 2,
            };
            let app_name = to_wide_null("ProjectBloom");
            let _ = set_string(client_info.0, &client_name_key, app_name.as_ptr());

            let wide_id = to_wide_null(pnp_id);
            type OpenFn = unsafe extern "system" fn(*mut c_void, *const u16, *mut c_void) -> i32;
            let open: OpenFn = vtable_entry(device.0, 3);
            if open(device.0, wide_id.as_ptr(), client_info.0) < 0 {
                return None;
            }

            type ContentFn = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32;
            let content_fn: ContentFn = vtable_entry(device.0, 5);
            let mut content_ptr: *mut c_void = null_mut();
            if content_fn(device.0, &mut content_ptr) < 0 || content_ptr.is_null() {
                return None;
            }
            let content = ComPtr(content_ptr);

            type PropsFn = unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32;
            let props_fn: PropsFn = vtable_entry(content.0, 4);
            let mut props_ptr: *mut c_void = null_mut();
            if props_fn(content.0, &mut props_ptr) < 0 || props_ptr.is_null() {
                return None;
            }
            let props = ComPtr(props_ptr);

            type GetValuesFn = unsafe extern "system" fn(
                *mut c_void,
                *const u16,
                *const c_void,
                *mut *mut c_void,
            ) -> i32;
            let get_values: GetValuesFn = vtable_entry(props.0, 5);
            let device_obj_id = to_wide_null("DEVICE");
            let mut vals_ptr: *mut c_void = null_mut();
            if get_values(props.0, device_obj_id.as_ptr(), null(), &mut vals_ptr) < 0
                || vals_ptr.is_null()
            {
                return None;
            }
            let vals = ComPtr(vals_ptr);

            type GetCountFn = unsafe extern "system" fn(*mut c_void, *mut u32) -> i32;
            type GetAtFn = unsafe extern "system" fn(
                *mut c_void,
                u32,
                *mut PropertyKey,
                *mut PropVariant,
            ) -> i32;
            let get_count: GetCountFn = vtable_entry(vals.0, 3);
            let get_at: GetAtFn = vtable_entry(vals.0, 4);

            let mut prop_count: u32 = 0;
            if get_count(vals.0, &mut prop_count) < 0 {
                return None;
            }

            let mut friendly_name = String::new();
            let mut sync_partner = String::new();
            let mut manufacturer = String::new();
            let mut model = String::new();
            let mut object_name = String::new();
            let mut firmware_build: Option<String> = None;
            let mut battery_percent: Option<u8> = None;
            let mut serial: Option<String> = None;

            for i in 0..prop_count {
                let mut key = PropertyKey::default();
                let mut pv = PropVariant::default();
                if get_at(vals.0, i, &mut key, &mut pv) >= 0 {
                    if key.fmtid == WPD_DEVICE_PROPS_FMTID {
                        match key.pid {
                            2 if pv.vt == VT_LPWSTR => {
                                sync_partner = wide_ptr_to_string(pv.data[0] as *const u16);
                            }
                            3 if pv.vt == VT_LPWSTR => {
                                let fw = wide_ptr_to_string(pv.data[0] as *const u16)
                                    .to_ascii_uppercase();
                                if !fw.is_empty() {
                                    firmware_build = Some(fw);
                                }
                            }
                            4 => {
                                let raw = match pv.vt {
                                    VT_UI4 => Some(pv.data[0] as u32),
                                    VT_I4 => u32::try_from((pv.data[0] as u32) as i32).ok(),
                                    VT_UI2 => Some(u32::from(pv.data[0] as u16)),
                                    VT_UI1 => Some(u32::from(pv.data[0] as u8)),
                                    _ => None,
                                };
                                if let Some(lvl) = raw {
                                    battery_percent = Some(lvl.min(100) as u8);
                                }
                            }
                            7 if pv.vt == VT_LPWSTR => {
                                manufacturer = wide_ptr_to_string(pv.data[0] as *const u16);
                            }
                            8 if pv.vt == VT_LPWSTR => {
                                model = wide_ptr_to_string(pv.data[0] as *const u16)
                                    .to_ascii_uppercase();
                            }
                            9 if pv.vt == VT_LPWSTR => {
                                let s = wide_ptr_to_string(pv.data[0] as *const u16);
                                if !s.is_empty() {
                                    serial = Some(s);
                                }
                            }
                            12 if pv.vt == VT_LPWSTR => {
                                friendly_name = wide_ptr_to_string(pv.data[0] as *const u16);
                            }
                            _ => {}
                        }
                    } else if key.fmtid == WPD_OBJECT_PROPS_FMTID
                        && key.pid == 4
                        && pv.vt == VT_LPWSTR
                    {
                        object_name = wide_ptr_to_string(pv.data[0] as *const u16);
                    }
                }
                let _ = PropVariantClear(&mut pv);
            }

            if model.is_empty() && !object_name.is_empty() {
                model = object_name.to_ascii_uppercase();
            }
            if friendly_name.is_empty() {
                friendly_name = sync_partner;
            }
            if model.is_empty() || !is_samsung_device(&manufacturer, &model, pnp_id) {
                return None;
            }

            Some(ConnectedSamsungDevice {
                device_id: pnp_id.to_owned(),
                friendly_name,
                manufacturer,
                model,
                firmware_build,
                battery_percent,
                serial,
                usb_serial: parse_usb_serial_from_pnp_id(pnp_id),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_usb_serial_and_rejects_composite_segments() {
        let real_pnp = r"\\?\usb#vid_04e8&pid_6865#rrgyc063qzr#{6ac27878-a6fa-4155-ba85-f98f491d4f33}";
        assert_eq!(
            parse_usb_serial_from_pnp_id(real_pnp).as_deref(),
            Some("RRGYC063QZR")
        );

        let composite = r"\\?\usb#vid_04e8&pid_6860#6&12ab34cd&0&2#{6ac27878-a6fa-4155-ba85-f98f491d4f33}";
        assert_eq!(parse_usb_serial_from_pnp_id(composite), None);
    }

    #[test]
    fn recognizes_samsung_wpd_devices() {
        assert!(is_samsung_device("samsung", "SM-S921B", ""));
        assert!(is_samsung_device("", "sm-a356e", ""));
        assert!(is_samsung_device("", "Unknown", r"\\?\usb#vid_04e8&pid_6865#abc"));
        assert!(!is_samsung_device("Google", "Pixel 9", r"\\?\usb#vid_18d1#xyz"));
    }
}
