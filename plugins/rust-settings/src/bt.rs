//! Bluetooth and airplane mode through Windows.Devices.Radios / Devices.Enumeration (no admin, no Settings app): the radio switch,
//! paired devices with their connection state, pairing a nearby device and removing a paired one. The WinRT calls wait, so they
//! run in a thread and leave their result in `STATE`, which the window reads on its next refresh.
use std::{
    collections::HashSet,
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering::Relaxed},
    },
};
use windows::{
    Devices::{
        Bluetooth::{
            BluetoothConnectionStatus, BluetoothDevice, BluetoothDeviceId, BluetoothLEDevice,
            GenericAttributeProfile::GattSession,
        },
        Enumeration::{DeviceInformation, DevicePairingKinds, DevicePairingResultStatus, DeviceUnpairingResultStatus},
        Radios::{Radio, RadioKind, RadioState},
    },
    Foundation::TypedEventHandler,
    core::HSTRING,
};
use windows_sys::{
    Win32::{
        Devices::Bluetooth::{
            BLUETOOTH_ADDRESS, BLUETOOTH_ADDRESS_0, BLUETOOTH_DEVICE_INFO, BLUETOOTH_FIND_RADIO_PARAMS, BLUETOOTH_SERVICE_DISABLE,
            BLUETOOTH_SERVICE_ENABLE, BluetoothEnumerateInstalledServices, BluetoothFindFirstRadio, BluetoothFindRadioClose,
            BluetoothGetDeviceInfo, BluetoothSetServiceState,
        },
        Foundation::{CloseHandle, HANDLE},
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx},
    },
    core::GUID,
};

#[derive(Clone, Debug, PartialEq)]
pub struct Dev {
    pub id: String,
    pub name: String,
    pub connected: bool,
}

#[derive(Default, Clone)]
pub struct State {
    pub bt: Option<bool>,        // a Bluetooth radio: on or off; None without one
    pub air: Option<bool>,       // every radio is off
    pub paired: Vec<Dev>,
    pub nearby: Vec<Dev>,
    pub scanning: bool,
    pub note: String,            // the outcome of the last pairing / removal
}

static STATE: Mutex<State> = Mutex::new(State { bt: None, air: None, paired: vec![], nearby: vec![], scanning: false, note: String::new() });
static BUSY: AtomicBool = AtomicBool::new(false);
type Done = Box<dyn FnOnce() + Send + 'static>;
static PENDING: Mutex<Vec<Done>> = Mutex::new(Vec::new());
static LE_SESSIONS: Mutex<Vec<(String, GattSession)>> = Mutex::new(Vec::new());

fn pending() -> MutexGuard<'static, Vec<Done>> {
    PENDING.lock().unwrap_or_else(|e| e.into_inner())
}

fn com() {
    unsafe { let _ = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32); }
}

pub fn state() -> State {
    STATE.lock().map(|s| s.clone()).unwrap_or_default()
}

fn with<T>(f: impl FnOnce(&mut State) -> T) -> Option<T> {
    STATE.lock().ok().map(|mut s| f(&mut s))
}

fn radios() -> Vec<Radio> {
    let _ = Radio::RequestAccessAsync().and_then(|a| a.join());
    Radio::GetRadiosAsync().and_then(|a| a.join()).map(|v| v.into_iter().collect()).unwrap_or_default()
}

/// Radio and device lists, read again in a thread (one at a time); `done` runs after the newest state is stored.
pub fn refresh(done: impl FnOnce() + Send + 'static) {
    pending().push(Box::new(done));
    if BUSY.swap(true, Relaxed) {
        return;
    }
    std::thread::spawn(|| loop {
        com();
        let callbacks = std::mem::take(&mut *pending());
        let rs = radios();
        let bt: Vec<_> = rs.iter().filter(|r| r.Kind().is_ok_and(|k| k == RadioKind::Bluetooth)).collect();
        let bt_on = (!bt.is_empty()).then(|| bt.iter().any(|r| r.State().is_ok_and(|s| s == RadioState::On)));
        let air = (!rs.is_empty()).then(|| rs.iter().all(|r| r.State().is_ok_and(|s| s != RadioState::On)));
        let paired = if bt_on == Some(true) { list(true) } else { vec![] };
        with(|s| (s.bt, s.air, s.paired) = (bt_on, air, paired));
        BUSY.store(false, Relaxed);
        for callback in callbacks {
            callback();
        }
        if pending().is_empty() || BUSY.swap(true, Relaxed) {
            break;
        }
    });
}

fn ids(selectors: [windows::core::Result<windows::core::HSTRING>; 2]) -> HashSet<String> {
    let mut out = HashSet::new();
    for selector in selectors.into_iter().flatten() {
        if let Ok(all) = DeviceInformation::FindAllAsyncAqsFilter(&selector).and_then(|a| a.join()) {
            out.extend(all.into_iter().filter_map(|d| d.Id().ok().map(|id| id.to_string())));
        }
    }
    out
}

fn list(paired: bool) -> Vec<Dev> {
    let connected = if paired {
        ids([
            BluetoothDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected),
            BluetoothLEDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected),
        ])
    } else {
        HashSet::new()
    };
    let names = ids([
        BluetoothDevice::GetDeviceSelectorFromPairingState(paired),
        BluetoothLEDevice::GetDeviceSelectorFromPairingState(paired),
    ]);
    let mut out = Vec::new();
    for id in names {
        let name = DeviceInformation::CreateFromIdAsync(&HSTRING::from(&id)).and_then(|a| a.join()).and_then(|d| d.Name()).map(|n| n.to_string()).unwrap_or_default();
        if !name.trim().is_empty() {
            let is_on = connected.contains(&id);
            out.push(Dev { id, name, connected: is_on });
        }
    }
    out.sort_by(|a, b| b.connected.cmp(&a.connected).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out
}

/// Switches every radio of that kind (`None`: all radios, which is airplane mode) and re-reads the state.
pub fn set(kind: Option<RadioKind>, on: bool, done: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || {
        com();
        for r in radios().into_iter().filter(|r| kind.is_none_or(|k| r.Kind().is_ok_and(|rk| rk == k))) {
            let _ = r.SetStateAsync(if on { RadioState::On } else { RadioState::Off }).and_then(|a| a.join());
        }
        refresh(done);
    });
}

pub fn set_bluetooth(on: bool, done: impl FnOnce() + Send + 'static) {
    set(Some(RadioKind::Bluetooth), on, done);
}

pub fn set_airplane(on: bool, done: impl FnOnce() + Send + 'static) {
    // airplane mode: everything off; back from it: Wi-Fi and Bluetooth on (the radios that have a switch of their own)
    std::thread::spawn(move || {
        com();
        for r in radios() {
            let kind = r.Kind().ok();
            if on || matches!(kind, Some(RadioKind::WiFi | RadioKind::Bluetooth)) {
                let _ = r.SetStateAsync(if on { RadioState::Off } else { RadioState::On }).and_then(|a| a.join());
            }
        }
        refresh(done);
    });
}

/// Looks for devices in range (it takes about ten seconds).
pub fn scan(done: impl FnOnce() + Send + 'static) {
    if with(|s| std::mem::replace(&mut s.scanning, true)) != Some(false) {
        return;
    }
    std::thread::spawn(move || {
        com();
        let found = list(false);
        with(|s| (s.nearby, s.scanning) = (found, false));
        done();
    });
}

fn pairing_text(code: i32) -> String {
    match DevicePairingResultStatus(code) {
        DevicePairingResultStatus::Paired => t!("Устройство подключено", "Device paired").into(),
        DevicePairingResultStatus::AlreadyPaired => t!("Уже сопряжено", "Already paired").into(),
        DevicePairingResultStatus::AuthenticationFailure | DevicePairingResultStatus::AuthenticationNotAllowed => t!("Сопряжение отклонено устройством", "The device refused to pair").into(),
        DevicePairingResultStatus::RequiredHandlerNotRegistered | DevicePairingResultStatus::RejectedByHandler => t!("Для этого устройства нужен ввод PIN: сопрягите его в параметрах Windows", "This device needs a PIN: pair it in Windows' own settings").into(),
        _ => tf!("Не удалось сопрячь (код {})", "Could not pair (code {})", code),
    }
}

/// Pairs a device from `nearby`: confirmation and a displayed PIN are accepted here (the PIN is shown in the note so it can be
/// compared with the device); a device that wants a PIN typed in is reported.
pub fn pair(id: String, done: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || {
        com();
        let msg = (|| -> windows::core::Result<String> {
            let info = DeviceInformation::CreateFromIdAsync(&HSTRING::from(&id))?.join()?;
            let custom = info.Pairing()?.Custom()?;
            let token = custom.PairingRequested(&TypedEventHandler::new(|_, args: windows::core::Ref<windows::Devices::Enumeration::DevicePairingRequestedEventArgs>| {
                if let Some(a) = args.as_ref() {
                    if a.PairingKind().is_ok_and(|k| k == DevicePairingKinds::ConfirmPinMatch || k == DevicePairingKinds::DisplayPin) {
                        let pin = a.Pin().map(|p| p.to_string()).unwrap_or_default();
                        with(|s| s.note = tf!("PIN: {}  (сравните с устройством)", "PIN: {}  (compare with the device)", pin));
                    }
                    a.Accept()?;
                }
                Ok(())
            }))?;
            let kinds = DevicePairingKinds::ConfirmOnly | DevicePairingKinds::DisplayPin | DevicePairingKinds::ConfirmPinMatch;
            let r = custom.PairAsync(kinds)?.join()?;
            custom.RemovePairingRequested(token).ok();
            Ok(pairing_text(r.Status()?.0))
        })()
        .unwrap_or_else(|e| tf!("Не удалось сопрячь: {}", "Could not pair: {}", e.message()));
        with(|s| s.note = msg);
        refresh(done);
    });
}

/// Removes a paired device.
pub fn unpair(id: String, done: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || {
        com();
        let msg = (|| -> windows::core::Result<String> {
            let info = DeviceInformation::CreateFromIdAsync(&HSTRING::from(&id))?.join()?;
            let r = info.Pairing()?.UnpairAsync()?.join()?;
            Ok(if r.Status()? == DeviceUnpairingResultStatus::Unpaired { t!("Устройство удалено", "Device removed").into() } else { tf!("Не удалось удалить (код {})", "Could not remove (code {})", r.Status()?.0) })
        })()
        .unwrap_or_else(|e| tf!("Не удалось удалить: {}", "Could not remove: {}", e.message()));
        with(|s| s.note = msg);
        refresh(done);
    });
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn scan_mac(s: &str, at: usize) -> Option<([u8; 6], usize)> {
    let b = s.as_bytes();
    if at + 17 <= b.len() && [2, 5, 8, 11, 14].iter().all(|&i| b[at + i] == b':' || b[at + i] == b'-') {
        let mut out = [0; 6];
        for i in 0..6 {
            out[i] = hex(b[at + i * 3])? * 16 + hex(b[at + i * 3 + 1])?;
        }
        return Some((out, at + 17));
    }
    if at + 12 <= b.len() {
        let mut out = [0; 6];
        for i in 0..6 {
            out[i] = hex(b[at + i * 2])? * 16 + hex(b[at + i * 2 + 1])?;
        }
        return Some((out, at + 12));
    }
    None
}

fn mac_from_id(id: &str) -> Option<[u8; 6]> {
    let start = id.find("Bluetooth-")? + "Bluetooth-".len();
    let (_, after) = scan_mac(id, start)?;
    let b = id.as_bytes();
    let sep = (after..b.len()).find(|&i| b[i] == b'-')?;
    let (mut out, _) = scan_mac(id, sep + 1)?;
    out.reverse();
    Some(out)
}

fn service_guid(code: u32) -> GUID {
    GUID { data1: 0x00001100 + code, data2: 0, data3: 0x1000, data4: [0x80, 0x00, 0x00, 0x80, 0x5f, 0x9b, 0x34, 0xfb] }
}

unsafe fn installed_services(radio: HANDLE, info: &BLUETOOTH_DEVICE_INFO) -> Vec<GUID> {
    unsafe {
        let mut n = 0u32;
        if BluetoothEnumerateInstalledServices(radio, info, &mut n, std::ptr::null_mut()) != 0 || n == 0 {
            return vec![];
        }
        let mut services = vec![GUID::default(); n as usize];
        let e = BluetoothEnumerateInstalledServices(radio, info, &mut n, services.as_mut_ptr());
        if e != 0 && e != 234 {
            return vec![];
        }
        services.truncate(n as usize);
        services
    }
}

unsafe fn set_connection(id: &str, on: bool) -> Result<(), String> {
    unsafe {
        let bytes = mac_from_id(id).ok_or_else(|| t!("не удалось прочитать адрес устройства", "could not read the device address").to_string())?;
        let find = BLUETOOTH_FIND_RADIO_PARAMS { dwSize: std::mem::size_of::<BLUETOOTH_FIND_RADIO_PARAMS>() as u32 };
        let mut radio: HANDLE = std::ptr::null_mut();
        let search = BluetoothFindFirstRadio(&find, &mut radio);
        if search.is_null() {
            let e = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            return Err(tf!("радио Bluetooth не найдено (ошибка {})", "No Bluetooth radio found (error {})", e));
        }
        BluetoothFindRadioClose(search);
        let mut info = BLUETOOTH_DEVICE_INFO {
            dwSize: std::mem::size_of::<BLUETOOTH_DEVICE_INFO>() as u32,
            Address: BLUETOOTH_ADDRESS { Anonymous: BLUETOOTH_ADDRESS_0 { rgBytes: bytes } },
            ..Default::default()
        };
        let e = BluetoothGetDeviceInfo(radio, &mut info);
        if e != 0 {
            CloseHandle(radio);
            return Err(tf!("устройство не найдено (код {})", "Device not found (code {})", e));
        }
        let mut services = installed_services(radio, &info);
        services.extend([0x0b, 0x0c, 0x0e, 0x1e].map(service_guid));
        let flag = if on { BLUETOOTH_SERVICE_ENABLE } else { BLUETOOTH_SERVICE_DISABLE };
        let mut ok = false;
        let mut last = 0;
        for g in &services {
            let e = BluetoothSetServiceState(radio, &info, g, flag);
            if e == 0 {
                ok = true;
            } else {
                last = e;
            }
        }
        CloseHandle(radio);
        if ok { Ok(()) } else { Err(tf!("не удалось изменить состояние (код {})", "Could not change the state (code {})", last)) }
    }
}

fn is_le(id: &str) -> bool {
    BluetoothDeviceId::FromId(&HSTRING::from(id)).and_then(|d| d.IsLowEnergyDevice()).unwrap_or(false)
}

fn set_le_connection(id: &str, on: bool) -> Result<(), String> {
    (|| -> windows::core::Result<()> {
        let device = BluetoothLEDevice::FromIdAsync(&HSTRING::from(id))?.join()?;
        let bid = BluetoothDeviceId::FromId(&device.DeviceId()?)?;
        let session = GattSession::FromDeviceIdAsync(&bid)?.join()?;
        let mut sessions = LE_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
        sessions.retain(|(saved, _)| saved != id);
        if on {
            session.SetMaintainConnection(true)?;
            sessions.push((id.to_string(), session));
        } else {
            let _ = session.SetMaintainConnection(false);
            session.Close()?;
        }
        Ok(())
    })()
    .map_err(|e| e.message().to_string())
}

pub fn connect(id: String, connected: bool, done: impl FnOnce() + Send + 'static) {
    std::thread::spawn(move || {
        com();
        let on = !connected;
        let msg = if is_le(&id) {
            set_le_connection(&id, on)
        } else {
            unsafe { set_connection(&id, on) }
        }
        .map(|()| if on { t!("Подключено", "Connected") } else { t!("Отключено", "Disconnected") }.to_string())
        .unwrap_or_else(|e| {
            let verb = if on { t!("подключить", "connect") } else { t!("отключить", "disconnect") };
            tf!("Не удалось {}: {}", "Could not {}: {}", verb, e)
        });
        with(|s| s.note = msg);
        refresh(done);
    });
}

pub fn take_note() -> Option<String> {
    with(|s| std::mem::take(&mut s.note)).filter(|n| !n.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_from_id_takes_the_remote_device_address() {
        let id = "Bluetooth#Bluetooth-00:11:22:33:44:55-66:77:88:99:AA:BB#BluetoothLE#x";
        assert_eq!(mac_from_id(id), Some([0xBB, 0xAA, 0x99, 0x88, 0x77, 0x66]));
        let id = "Bluetooth#Bluetooth-001122334455-66778899aabb";
        assert_eq!(mac_from_id(id), Some([0xBB, 0xAA, 0x99, 0x88, 0x77, 0x66]));
        assert_eq!(mac_from_id("not-a-device"), None);
    }

    #[test]
    fn mac_from_id_accepts_dashed_remote_addresses() {
        let id = "Bluetooth#Bluetooth-00-11-22-33-44-55-66-77-88-99-AA-BB";
        assert_eq!(mac_from_id(id), Some([0xBB, 0xAA, 0x99, 0x88, 0x77, 0x66]));
    }
}
