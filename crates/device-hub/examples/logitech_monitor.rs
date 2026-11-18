//! Foreground profile selection preview. No HID handles or input injection.

use std::collections::HashMap;
use std::path::Path;

struct Profiles {
    by_executable: HashMap<String, String>,
}

impl Profiles {
    fn parse(config: &str) -> Result<Self, String> {
        let mut by_executable = HashMap::new();
        for (index, row) in config.trim_start_matches('\u{feff}').lines().enumerate() {
            let row = row.trim();
            if row.is_empty() || row.starts_with('#') {
                continue;
            }
            let invalid = || format!("Invalid profile rule on line {}", index + 1);
            let (label, path) = row.split_once('\t').ok_or_else(invalid)?;
            let path = path.trim();
            let label = label.trim();
            if label.is_empty()
                || label.eq_ignore_ascii_case("Desktop")
                || label.chars().any(char::is_control)
                || path.chars().any(char::is_control)
                || !Path::new(path).is_absolute()
            {
                return Err(invalid());
            }
            if by_executable
                .insert(path.to_lowercase(), label.to_owned())
                .is_some()
            {
                return Err(format!("Duplicate program on line {}", index + 1));
            }
        }
        Ok(Self { by_executable })
    }

    fn select(&self, foreground: Option<&str>) -> &str {
        foreground
            .and_then(|path| self.by_executable.get(&path.to_lowercase()))
            .map(String::as_str)
            .unwrap_or("Desktop")
    }
}

#[cfg(windows)]
mod windows {
    use super::Profiles;
    use std::ffi::c_void;
    use std::io;
    use std::ptr::null_mut;
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    use std::time::Duration;

    type Handle = *mut c_void;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const DEVICE_NOTIFY_CALLBACK: u32 = 2;
    const PBT_APMSUSPEND: u32 = 4;
    const PBT_APMRESUMEAUTOMATIC: u32 = 18;
    const RIDI_DEVICENAME: u32 = 0x2000_0007;
    const RIDI_DEVICEINFO: u32 = 0x2000_000b;
    const RIM_TYPEHID: u32 = 2;
    const ERROR_INSUFFICIENT_BUFFER: i32 = 122;

    // Bit 0 records suspension. Each resume increments the generation even if the
    // foreground executable is unchanged or both events land between samples.
    static POWER: AtomicUsize = AtomicUsize::new(0);

    #[repr(C)]
    struct Subscription {
        callback: extern "system" fn(*mut c_void, u32, *mut c_void) -> u32,
        context: *mut c_void,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct RawDevice {
        handle: Handle,
        kind: u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct RawHidInfo {
        vendor_id: u32,
        product_id: u32,
        version: u32,
        usage_page: u16,
        usage: u16,
    }

    #[repr(C)]
    union RawDeviceData {
        hid: RawHidInfo,
        // RID_DEVICE_INFO_KEYBOARD is the largest member, at six DWORDs.
        storage: [u32; 6],
    }

    #[repr(C)]
    struct RawDeviceInfo {
        size: u32,
        kind: u32,
        data: RawDeviceData,
    }

    #[link(name = "user32")]
    extern "system" {
        fn GetForegroundWindow() -> Handle;
        fn GetWindowThreadProcessId(window: Handle, process_id: *mut u32) -> u32;
        fn GetRawInputDeviceList(devices: *mut RawDevice, count: *mut u32, size: u32) -> u32;
        fn GetRawInputDeviceInfoW(
            device: Handle,
            command: u32,
            data: *mut c_void,
            size: *mut u32,
        ) -> u32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, process_id: u32) -> Handle;
        fn QueryFullProcessImageNameW(
            process: Handle,
            flags: u32,
            path: *mut u16,
            size: *mut u32,
        ) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
    }

    #[link(name = "powrprof")]
    extern "system" {
        fn PowerRegisterSuspendResumeNotification(
            flags: u32,
            recipient: *const Subscription,
            registration: *mut Handle,
        ) -> u32;
        fn PowerUnregisterSuspendResumeNotification(registration: Handle) -> u32;
    }

    extern "system" fn power_event(_: *mut c_void, event: u32, _: *mut c_void) -> u32 {
        update_power(&POWER, event);
        0
    }

    fn update_power(state: &AtomicUsize, event: u32) {
        match event {
            PBT_APMSUSPEND => {
                state.fetch_or(1, Relaxed);
            }
            PBT_APMRESUMEAUTOMATIC => {
                // An even generation means awake; the next generation is always new.
                let mut previous = state.load(Relaxed);
                loop {
                    match state.compare_exchange_weak(
                        previous,
                        (previous & !1).wrapping_add(2),
                        Relaxed,
                        Relaxed,
                    ) {
                        Ok(_) => break,
                        Err(current) => previous = current,
                    }
                }
            }
            _ => {}
        }
    }

    struct PowerNotifications(Handle);

    impl PowerNotifications {
        fn register(subscription: &Subscription) -> io::Result<Self> {
            let mut handle = null_mut();
            let result = unsafe {
                PowerRegisterSuspendResumeNotification(
                    DEVICE_NOTIFY_CALLBACK,
                    subscription,
                    &mut handle,
                )
            };
            if result != 0 {
                return Err(io::Error::from_raw_os_error(result as i32));
            }
            Ok(Self(handle))
        }
    }

    impl Drop for PowerNotifications {
        fn drop(&mut self) {
            unsafe { PowerUnregisterSuspendResumeNotification(self.0) };
        }
    }

    fn foreground_path(buffer: &mut [u16]) -> Option<String> {
        // Query the exact image path with minimum process access. A change mid-query
        // invalidates the sample. No titles or executable paths are logged.
        unsafe {
            let window = GetForegroundWindow();
            if window.is_null() {
                return None;
            }
            let mut pid = 0;
            GetWindowThreadProcessId(window, &mut pid);
            if pid == 0 {
                return None;
            }
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return None;
            }
            let mut size = buffer.len() as u32;
            let result = QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut size);
            CloseHandle(process);
            if result == 0 || size == 0 || GetForegroundWindow() != window {
                return None;
            }
            String::from_utf16(&buffer[..size as usize]).ok()
        }
    }

    fn raw_devices() -> io::Result<Vec<RawDevice>> {
        let size = std::mem::size_of::<RawDevice>() as u32;
        for _ in 0..3 {
            let mut count = 0;
            if unsafe { GetRawInputDeviceList(null_mut(), &mut count, size) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            if count == 0 {
                return Ok(Vec::new());
            }
            let mut devices = vec![
                RawDevice {
                    handle: null_mut(),
                    kind: 0
                };
                count as usize
            ];
            let read = unsafe { GetRawInputDeviceList(devices.as_mut_ptr(), &mut count, size) };
            if read != u32::MAX {
                devices.truncate(read as usize);
                return Ok(devices);
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER) {
                return Err(error);
            }
        }
        Err(io::Error::other(
            "Raw Input device list changed during enumeration",
        ))
    }

    fn device_name(device: Handle) -> Option<String> {
        let mut size = 0;
        let result =
            unsafe { GetRawInputDeviceInfoW(device, RIDI_DEVICENAME, null_mut(), &mut size) };
        if result == u32::MAX || size == 0 {
            return None;
        }
        let mut name = vec![0u16; size as usize];
        let result = unsafe {
            GetRawInputDeviceInfoW(device, RIDI_DEVICENAME, name.as_mut_ptr().cast(), &mut size)
        };
        if result == u32::MAX {
            return None;
        }
        let end = name.iter().position(|&ch| ch == 0).unwrap_or(name.len());
        String::from_utf16(&name[..end]).ok()
    }

    fn candidate(name: &str) -> Option<(&'static str, &'static str)> {
        let upper = name.to_ascii_uppercase();
        let (_, ids) = upper.split_once("#VID_046D&PID_")?;
        let pid = ids.split(['&', '#']).next()?;
        match pid {
            "C09B" => Some(("C09B", "PRO X SUPERLIGHT 2 wired candidate")),
            "C54D" => Some(("C54D", "LIGHTSPEED receiver; paired model unverified")),
            "C088" => Some(("C088", "G PRO Wireless wired candidate")),
            _ => None,
        }
    }

    fn hid_usage(device: RawDevice) -> Option<(u16, u16)> {
        if device.kind != RIM_TYPEHID {
            return None;
        }
        let mut size = std::mem::size_of::<RawDeviceInfo>() as u32;
        let mut info = RawDeviceInfo {
            size,
            kind: 0,
            data: RawDeviceData { storage: [0; 6] },
        };
        let result = unsafe {
            GetRawInputDeviceInfoW(
                device.handle,
                RIDI_DEVICEINFO,
                (&mut info as *mut RawDeviceInfo).cast(),
                &mut size,
            )
        };
        if result == u32::MAX || info.kind != RIM_TYPEHID {
            return None;
        }
        let hid = unsafe { info.data.hid };
        (hid.vendor_id == 0x046d).then_some((hid.usage_page, hid.usage))
    }

    pub fn inventory() -> io::Result<()> {
        let mut candidates = std::collections::BTreeSet::new();
        for device in raw_devices()? {
            if let Some(found) = device_name(device.handle).as_deref().and_then(candidate) {
                candidates.insert((found, hid_usage(device)));
            }
        }
        println!("Raw Input device candidates (no HID handles opened):");
        for ((pid, name), usage) in &candidates {
            match usage {
                Some((page, usage)) => println!("046D:{pid} {page:04X}/{usage:04X} {name}"),
                None => println!("046D:{pid} {name}"),
            }
        }
        if candidates.is_empty() {
            println!("No matching VID/PID candidates in this session.");
        }
        Ok(())
    }

    pub fn run(profiles: Profiles, once: bool) -> io::Result<()> {
        let subscription = Subscription {
            callback: power_event,
            context: null_mut(),
        };
        let _registration = PowerNotifications::register(&subscription)?;
        let mut path_buffer = vec![0; 32_768];
        let mut last = None;
        println!("Profile preview (Ctrl+C to stop). No mouse configuration is applied.");
        loop {
            let power = POWER.load(Relaxed);
            let suspended = power & 1 != 0;
            let foreground = if suspended {
                None
            } else {
                foreground_path(&mut path_buffer)
            };
            let profile = if suspended {
                "Paused"
            } else {
                profiles.select(foreground.as_deref())
            };
            if POWER.load(Relaxed) == power && last != Some((power, profile)) {
                println!("Profile: {profile}");
                last = Some((power, profile));
                if once {
                    println!("Foreground path resolved: {}", foreground.is_some());
                    return Ok(());
                }
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    #[cfg(test)]
    #[test]
    fn resume_changes_generation_even_without_an_observed_suspend() {
        let state = AtomicUsize::new(0);
        update_power(&state, PBT_APMSUSPEND);
        assert_eq!(state.load(Relaxed), 1);
        update_power(&state, PBT_APMRESUMEAUTOMATIC);
        assert_eq!(state.load(Relaxed), 2);
        update_power(&state, 7); // Later interactive-resume notification.
        assert_eq!(state.load(Relaxed), 2);
        update_power(&state, PBT_APMRESUMEAUTOMATIC);
        assert_eq!(state.load(Relaxed), 4);
    }

    #[test]
    fn raw_inventory_cannot_identify_a_paired_mouse_from_a_receiver() {
        let (_, name) = candidate(r"\\?\HID#Vid_046d&Pid_c54d&MI_01#anonymous").unwrap();
        assert!(name.contains("paired model unverified"));
        assert!(candidate(r"\\?\HID#Vid_046d&Pid_c54d0#anonymous").is_none());
    }
}

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    const USAGE: &str = "Usage: logitech_monitor <profiles.tsv> [--once] | --inventory";
    let mut args = std::env::args_os().skip(1);
    let config = args.next().ok_or(USAGE)?;
    if config == "--inventory" {
        if args.next().is_some() {
            return Err(USAGE.into());
        }
        return Ok(windows::inventory()?);
    }
    let once = match args.next() {
        None => false,
        Some(arg) if arg == "--once" && args.next().is_none() => true,
        _ => return Err(USAGE.into()),
    };
    let profiles = Profiles::parse(&std::fs::read_to_string(config)?)?;
    windows::run(profiles, once)?;
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("This diagnostic requires Windows.");
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn matches_a_full_path_and_falls_back_to_desktop() {
        let profiles = Profiles::parse("Game\tC:\\Game\\Play.exe").unwrap();
        assert_eq!(profiles.select(Some("c:\\game\\PLAY.exe")), "Game");
        assert_eq!(profiles.select(Some("D:\\Game\\Play.exe")), "Desktop");
        assert_eq!(profiles.select(None), "Desktop");
    }

    #[test]
    fn rejects_duplicate_and_relative_paths() {
        assert!(Profiles::parse("Game\tPlay.exe").is_err());
        assert!(Profiles::parse("One\tC:\\Play.exe\nTwo\tc:\\PLAY.exe").is_err());
    }
}
