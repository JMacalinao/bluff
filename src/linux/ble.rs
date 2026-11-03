//! BlueZ GATT server, advertisement and pairing agent for a HID-over-GATT peripheral.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::future::Future;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, OwnedFd};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use log::{debug, info, warn};
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::AbortHandle;
use tokio::time::sleep;
use zbus::fdo::{ObjectManager, ObjectManagerProxy, PropertiesProxy};
use zbus::message::Type;
use zbus::names::InterfaceName;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, Value};
use zbus::{Connection, MatchRule, MessageStream, interface};

use crate::bridge::{self, Bridge, Peripheral as _};
use crate::hid::{KEYBOARD_ID, MOUSE_ID, REPORT_MAP};
use crate::linux::hosts::{Change, Host, Hosts, Props};

pub type Shared = Arc<Mutex<Bridge<Peripheral>>>;
/// Asks the user a pairing question and resolves to whether they accepted.
pub type Confirm = Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

const BLUEZ: &str = "org.bluez";
const DEVICE: &str = "org.bluez.Device1";
const GATT_MANAGER: &str = "org.bluez.GattManager1";
const ADVERTISING_MANAGER: &str = "org.bluez.LEAdvertisingManager1";
const ROOT: &str = "/org/bluff/gatt";
const ADVERTISEMENT: &str = "/org/bluff/advertisement";
const AGENT: &str = "/org/bluff/agent";
const HID_SERVICE: &str = "/org/bluff/gatt/service0";
const INFO_SERVICE: &str = "/org/bluff/gatt/service2";
const SPARE: &str = "/org/bluff/gatt/service2/char1";
const APPEARANCE_KEYBOARD: u16 = 0x03C1;
// Report flags force an encrypted link, which is what makes iPadOS bond.
const SECURE_READ: &[&str] = &["encrypt-read"];
const SECURE_NOTIFY: &[&str] = &["encrypt-read", "notify"];
const RESUBSCRIBE: Duration = Duration::from_secs(10);
// At most one nudge per host in this long, so a host that never subscribes is not kicked forever.
const NUDGE: Duration = Duration::from_secs(120);

pub use crate::bridge::lock;

fn path(path: &str) -> ObjectPath<'_> {
    ObjectPath::from_str_unchecked(path)
}

fn read(value: &[u8], options: &Props) -> Vec<u8> {
    let offset = options
        .get("offset")
        .and_then(|v| u16::try_from(v).ok())
        .unwrap_or(0);
    value
        .get(usize::from(offset)..)
        .unwrap_or_default()
        .to_vec()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    Keyboard,
    Mouse,
}

/// A report socket that failed or that BlueZ hung up.
#[derive(Debug)]
pub struct Dropped {
    which: Which,
    device: String,
    id: u64,
}

/// Our end of the socket BlueZ forwards one host's notifications from.
pub struct Socket {
    id: u64,
    fd: Arc<OwnedFd>,
    watcher: Option<AbortHandle>,
}

impl Drop for Socket {
    fn drop(&mut self) {
        if let Some(watcher) = &self.watcher {
            watcher.abort();
        }
    }
}

/// Input report delivered to one host at a time.
///
/// Changing `Value` would notify every subscribed host. Exposing `NotifyAcquired`
/// makes BlueZ call `AcquireNotify` once per subscribing host instead, with the
/// host's device path, and send whatever is written to that host's socket to
/// that host only.
pub struct Report {
    path: String,
    value: Vec<u8>,
    pub sockets: HashMap<String, Socket>,
}

impl Report {
    /// Send `value` to the target host, or to every host when there is no target.
    fn notify(
        &mut self,
        which: Which,
        value: &[u8],
        target: Option<&str>,
        dropped: &UnboundedSender<Dropped>,
    ) {
        self.value = value.to_vec();
        for (device, socket) in &self.sockets {
            if target.is_some_and(|target| target != device) {
                continue;
            }
            // SAFETY: sends from a valid buffer on a socket we own.
            let sent = unsafe {
                libc::send(
                    socket.fd.as_raw_fd(),
                    value.as_ptr().cast(),
                    value.len(),
                    libc::MSG_NOSIGNAL,
                )
            };
            if sent < 0 {
                warn!(
                    "dropping {device} from {}: {}",
                    self.path,
                    io::Error::last_os_error()
                );
                // Not from here: dropping tells the bridge, which may be sending this very report.
                let _ = dropped.send(Dropped {
                    which,
                    device: device.clone(),
                    id: socket.id,
                });
            }
        }
    }
}

/// What the bridge sends input to: the report sockets, the hosts and the pairing state.
pub struct Peripheral {
    pub keyboard: Report,
    pub mouse: Report,
    pub hosts: Hosts,
    pub confirm: Option<Confirm>,
    dropped: UnboundedSender<Dropped>,
}

impl Peripheral {
    pub fn new(hosts: Hosts) -> (Self, UnboundedReceiver<Dropped>) {
        let (dropped, receiver) = mpsc::unbounded_channel();
        let report = |index: u8| Report {
            path: format!("{HID_SERVICE}/char{index}"),
            value: Vec::new(),
            sockets: HashMap::new(),
        };
        (
            Self {
                keyboard: report(3),
                mouse: report(4),
                hosts,
                confirm: None,
                dropped,
            },
            receiver,
        )
    }

    fn report(&mut self, which: Which) -> &mut Report {
        match which {
            Which::Keyboard => &mut self.keyboard,
            Which::Mouse => &mut self.mouse,
        }
    }

    fn target(&self) -> Option<String> {
        self.hosts.chosen().map(|host| host.path.clone())
    }

    /// Open a socket for `device` and return BlueZ's end.
    pub fn acquire(&mut self, which: Which, device: &str) -> io::Result<OwnedFd> {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let (ours, theirs) = socketpair()?;
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let fd = Arc::new(ours);
        // SAFETY: the Arc keeps the fd open for as long as the AsyncFd holds it.
        let watched = unsafe { AsyncFd::register_with_interest(fd.clone(), Interest::READABLE) }
            .map_err(|failed| failed.into_parts().1)?;
        let dropped = self.dropped.clone();
        let hangup = Dropped {
            which,
            device: device.to_string(),
            id,
        };
        let watcher = tokio::spawn(async move {
            // BlueZ never writes to a notify socket, so readable means it hung up.
            let _ = watched.readable().await;
            let _ = dropped.send(hangup);
        });
        let socket = Socket {
            id,
            fd,
            watcher: Some(watcher.abort_handle()),
        };
        let report = self.report(which);
        report.sockets.insert(device.to_string(), socket);
        info!("host {device} subscribed to {}", report.path);
        Ok(theirs)
    }

    /// Forget a socket unless the host already replaced it; return whether it was forgotten.
    pub fn drop_socket(&mut self, dropped: &Dropped) -> bool {
        let report = self.report(dropped.which);
        if report
            .sockets
            .get(&dropped.device)
            .is_none_or(|socket| socket.id != dropped.id)
        {
            return false;
        }
        report.sockets.remove(&dropped.device);
        info!("host {} unsubscribed from {}", dropped.device, report.path);
        true
    }

    /// Hosts on an LE link in `le`, or connected ones when that is unknown, with no keyboard
    /// socket and not nudged within `NUDGE`.
    fn unsubscribed(
        &self,
        nudged: &HashMap<String, Instant>,
        le: Option<&HashSet<String>>,
        now: Instant,
    ) -> Vec<Host> {
        let mut hosts: Vec<Host> = self
            .hosts
            .hosts
            .values()
            .filter(|host| host.connected && !self.keyboard.sockets.contains_key(&host.path))
            .filter(|host| le.is_none_or(|le| le.contains(&host.address)))
            .filter(|host| {
                nudged
                    .get(&host.path)
                    .is_none_or(|&at| now.duration_since(at) >= NUDGE)
            })
            .cloned()
            .collect();
        hosts.sort_by(|a, b| a.path.cmp(&b.path));
        hosts
    }
}

impl bridge::Peripheral for Peripheral {
    fn notify_keyboard(&mut self, report: &[u8]) {
        let target = self.target();
        self.keyboard
            .notify(Which::Keyboard, report, target.as_deref(), &self.dropped);
    }

    fn notify_mouse(&mut self, report: &[u8]) {
        let target = self.target();
        self.mouse
            .notify(Which::Mouse, report, target.as_deref(), &self.dropped);
    }

    /// Whether input sent now reaches a host: the chosen one, or any when none is chosen.
    fn connected(&self) -> bool {
        let target = self.target();
        self.keyboard
            .sockets
            .keys()
            .any(|device| target.as_ref().is_none_or(|target| target == device))
    }

    fn pairing(&self) -> bool {
        self.confirm.is_some()
    }
}

fn socketpair() -> io::Result<(OwnedFd, OwnedFd)> {
    use std::os::fd::FromRawFd;
    let mut fds = [0; 2];
    // SAFETY: socketpair fills both fds on success, and we take ownership of them right away.
    unsafe {
        if libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        ) < 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok((OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])))
    }
}

/// Addresses with an LE link on `adapter`, asked of the kernel: BlueZ's `Connected` is also true
/// for a BR/EDR link alone, and its per-bearer state is experimental.
fn le_links(adapter: &str) -> io::Result<HashSet<String>> {
    use std::os::fd::FromRawFd;
    const BTPROTO_HCI: libc::c_int = 1;
    // _IOR('H', 212, int)
    const HCIGETCONNLIST: libc::c_ulong = 0x8004_48d4;
    const LE_LINK: u8 = 0x80;
    const MAX: usize = 32;
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct ConnInfo {
        handle: u16,
        address: [u8; 6],
        kind: u8,
        out: u8,
        state: u16,
        link_mode: u32,
    }
    #[repr(C)]
    struct ConnList {
        dev_id: u16,
        count: u16,
        info: [ConnInfo; MAX],
    }
    let dev_id = adapter
        .rsplit_once("hci")
        .and_then(|(_, id)| id.parse().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not an hci adapter"))?;
    let mut list = ConnList {
        dev_id,
        count: MAX as u16,
        info: [ConnInfo::default(); MAX],
    };
    // SAFETY: the fd is owned right away, and the ioctl writes at most `count` entries into `list`.
    unsafe {
        let fd = libc::socket(
            libc::AF_BLUETOOTH,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            BTPROTO_HCI,
        );
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = OwnedFd::from_raw_fd(fd);
        if libc::ioctl(fd.as_raw_fd(), HCIGETCONNLIST as _, &mut list) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(list.info[..usize::from(list.count).min(MAX)]
        .iter()
        .filter(|info| info.kind == LE_LINK)
        .map(|info| address(info.address))
        .collect())
}

/// BlueZ's form of a little-endian address.
fn address(bytes: [u8; 6]) -> String {
    let parts: Vec<String> = bytes.iter().rev().map(|b| format!("{b:02X}")).collect();
    parts.join(":")
}

fn random_uuid() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    bytes[6] = bytes[6] & 0x0F | 0x40;
    bytes[8] = bytes[8] & 0x3F | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

struct Service {
    uuid: &'static str,
}

#[interface(name = "org.bluez.GattService1")]
impl Service {
    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> &str {
        self.uuid
    }

    #[zbus(property)]
    fn primary(&self) -> bool {
        true
    }
}

struct Characteristic {
    uuid: String,
    service: &'static str,
    flags: &'static [&'static str],
    value: Vec<u8>,
}

#[interface(name = "org.bluez.GattCharacteristic1")]
impl Characteristic {
    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> &str {
        &self.uuid
    }

    #[zbus(property)]
    fn service(&self) -> ObjectPath<'_> {
        path(self.service)
    }

    #[zbus(property)]
    fn flags(&self) -> Vec<&str> {
        self.flags.to_vec()
    }

    #[zbus(property)]
    fn value(&self) -> Vec<u8> {
        self.value.clone()
    }

    fn read_value(&self, options: Props) -> Vec<u8> {
        read(&self.value, &options)
    }

    fn write_value(&self, value: Vec<u8>, _options: Props) {
        debug!("write to {}: {value:02x?}", self.uuid);
    }

    fn start_notify(&self) {
        info!("host subscribed to {}", self.uuid);
    }

    fn stop_notify(&self) {
        info!("host unsubscribed from {}", self.uuid);
    }
}

struct ReportCharacteristic {
    which: Which,
    shared: Shared,
}

impl ReportCharacteristic {
    fn with<R>(&self, f: impl FnOnce(&mut Report) -> R) -> R {
        f(lock(&self.shared).peripheral.report(self.which))
    }
}

#[interface(name = "org.bluez.GattCharacteristic1")]
impl ReportCharacteristic {
    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> &str {
        "2a4d"
    }

    #[zbus(property)]
    fn service(&self) -> ObjectPath<'_> {
        path(HID_SERVICE)
    }

    #[zbus(property)]
    fn flags(&self) -> Vec<&str> {
        SECURE_NOTIFY.to_vec()
    }

    #[zbus(property)]
    fn value(&self) -> Vec<u8> {
        self.with(|report| report.value.clone())
    }

    #[zbus(property)]
    fn notify_acquired(&self) -> bool {
        self.with(|report| !report.sockets.is_empty())
    }

    fn read_value(&self, options: Props) -> Vec<u8> {
        self.with(|report| read(&report.value, &options))
    }

    fn write_value(&self, value: Vec<u8>, _options: Props) {
        debug!("write to report: {value:02x?}");
    }

    fn start_notify(&self) {
        info!("host subscribed to {:?} report", self.which);
    }

    fn stop_notify(&self) {
        info!("host unsubscribed from {:?} report", self.which);
    }

    fn acquire_notify(&self, options: Props) -> zbus::fdo::Result<(zbus::zvariant::OwnedFd, u16)> {
        let device = options
            .get("device")
            .and_then(|v| OwnedObjectPath::try_from(v.try_clone().ok()?).ok())
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("no device".into()))?;
        let mtu = options
            .get("mtu")
            .and_then(|v| u16::try_from(v).ok())
            .unwrap_or(23);
        let mut bridge = lock(&self.shared);
        let theirs = bridge
            .peripheral
            .acquire(self.which, device.as_str())
            .map_err(|error| zbus::fdo::Error::IOError(error.to_string()))?;
        bridge.subscription_changed();
        Ok((theirs.into(), mtu))
    }
}

struct Descriptor {
    uuid: &'static str,
    characteristic: String,
    value: Vec<u8>,
}

#[interface(name = "org.bluez.GattDescriptor1")]
impl Descriptor {
    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> &str {
        self.uuid
    }

    #[zbus(property)]
    fn characteristic(&self) -> ObjectPath<'_> {
        path(&self.characteristic)
    }

    #[zbus(property)]
    fn flags(&self) -> Vec<&str> {
        SECURE_READ.to_vec()
    }

    fn read_value(&self, options: Props) -> Vec<u8> {
        read(&self.value, &options)
    }
}

struct Advertisement {
    name: String,
    discoverable: bool,
}

#[interface(name = "org.bluez.LEAdvertisement1")]
impl Advertisement {
    #[zbus(property, name = "Type")]
    fn type_(&self) -> &str {
        "peripheral"
    }

    #[zbus(property, name = "ServiceUUIDs")]
    fn service_uuids(&self) -> Vec<&str> {
        vec!["1812"]
    }

    #[zbus(property)]
    fn local_name(&self) -> &str {
        &self.name
    }

    #[zbus(property)]
    fn appearance(&self) -> u16 {
        APPEARANCE_KEYBOARD
    }

    #[zbus(property)]
    fn discoverable(&self) -> bool {
        self.discoverable
    }

    fn release(&self) {
        info!("advertisement released by BlueZ");
    }
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.bluez.Error")]
enum AgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Rejected(String),
}

/// Pairing agent: numeric comparison through the peripheral's `confirm`, or refuse when it is None.
///
/// Being the default agent matters even when refusing: with no agent, BlueZ
/// accepts Just Works pairing from anyone in range.
struct Agent {
    shared: Shared,
}

impl Agent {
    async fn confirm(&self, device: &OwnedObjectPath, question: String) -> Result<(), AgentError> {
        let confirm = lock(&self.shared).peripheral.confirm.clone();
        match confirm {
            None => warn!("refused pairing ({question}); not in pairing mode"),
            Some(confirm) if confirm(question).await => {
                lock(&self.shared)
                    .peripheral
                    .hosts
                    .accepted(device.as_str());
                return Ok(());
            }
            Some(_) => {}
        }
        Err(AgentError::Rejected("pairing rejected".into()))
    }
}

#[interface(name = "org.bluez.Agent1")]
impl Agent {
    async fn request_confirmation(
        &self,
        device: OwnedObjectPath,
        passkey: u32,
    ) -> Result<(), AgentError> {
        let question = format!(
            "Pair with {}? The device must show {passkey:06}.",
            device.as_str()
        );
        self.confirm(&device, question).await
    }

    async fn request_authorization(&self, device: OwnedObjectPath) -> Result<(), AgentError> {
        let question = format!("Allow {} to pair?", device.as_str());
        self.confirm(&device, question).await
    }

    fn authorize_service(&self, _device: OwnedObjectPath, _uuid: String) {}

    fn cancel(&self) {
        info!("pairing cancelled");
    }

    fn release(&self) {}
}

#[derive(Default)]
struct Registration {
    spare: bool,
    at: Option<Instant>,
}

impl Registration {
    fn recent(&self, now: Instant) -> bool {
        self.at
            .is_some_and(|at| now.duration_since(at) < RESUBSCRIBE)
    }
}

/// HID-over-GATT keyboard and mouse registered on a BlueZ adapter.
#[derive(Clone)]
pub struct Ble {
    inner: Arc<Inner>,
}

struct Inner {
    conn: Connection,
    shared: Shared,
    name: String,
    adapter: String,
    advertising: tokio::sync::Mutex<bool>,
    registration: tokio::sync::Mutex<Registration>,
    nudged: Mutex<HashMap<String, Instant>>,
}

impl Ble {
    pub async fn start(
        shared: Shared,
        dropped: UnboundedReceiver<Dropped>,
        name: &str,
        adapter: &str,
    ) -> zbus::Result<Self> {
        let conn = Connection::system().await?;
        let ble = Self {
            inner: Arc::new(Inner {
                conn,
                shared,
                name: name.to_string(),
                adapter: format!("/org/bluez/{adapter}"),
                advertising: tokio::sync::Mutex::new(false),
                registration: tokio::sync::Mutex::default(),
                nudged: Mutex::default(),
            }),
        };
        ble.export().await?;
        tokio::spawn(ble.clone().drop_sockets(dropped));
        let inner = &ble.inner;
        let agents = "org.bluez.AgentManager1";
        let agent = path(AGENT);
        ble.call(
            "/org/bluez",
            agents,
            "RegisterAgent",
            &(&agent, "DisplayYesNo"),
        )
        .await?;
        ble.call("/org/bluez", agents, "RequestDefaultAgent", &(&agent,))
            .await?;
        let adapter = PropertiesProxy::builder(&inner.conn)
            .destination(BLUEZ)?
            .path(inner.adapter.as_str())?
            .build()
            .await?;
        adapter
            .set(
                InterfaceName::from_static_str_unchecked("org.bluez.Adapter1"),
                "Powered",
                Value::from(true),
            )
            .await?;
        ble.register().await?;
        ble.watch_hosts().await?;
        ble.update_advertising().await?;
        info!("registered as {:?} on {}", inner.name, inner.adapter);
        Ok(ble)
    }

    async fn call<B>(
        &self,
        object: &str,
        interface: &str,
        member: &str,
        body: &B,
    ) -> zbus::Result<()>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        self.inner
            .conn
            .call_method(Some(BLUEZ), object, Some(interface), member, body)
            .await?;
        Ok(())
    }

    async fn register(&self) -> zbus::Result<()> {
        let options: HashMap<&str, Value> = HashMap::new();
        self.call(
            &self.inner.adapter,
            GATT_MANAGER,
            "RegisterApplication",
            &(path(ROOT), options),
        )
        .await
    }

    async fn export(&self) -> zbus::Result<()> {
        let server = self.inner.conn.object_server();
        let shared = &self.inner.shared;
        server.at(ROOT, ObjectManager).await?;
        let characteristic = |service, uuid: &str, flags, value: &[u8]| Characteristic {
            uuid: uuid.to_string(),
            service,
            flags,
            value: value.to_vec(),
        };
        server.at(HID_SERVICE, Service { uuid: "1812" }).await?;
        // HID 1.11, normally connectable
        let info = characteristic(HID_SERVICE, "2a4a", &["read"], &[0x11, 0x01, 0x00, 0x02]);
        server.at(format!("{HID_SERVICE}/char0"), info).await?;
        let map = characteristic(HID_SERVICE, "2a4b", SECURE_READ, REPORT_MAP);
        server.at(format!("{HID_SERVICE}/char1"), map).await?;
        let control = characteristic(HID_SERVICE, "2a4c", &["write-without-response"], &[]);
        server.at(format!("{HID_SERVICE}/char2"), control).await?;
        for (which, id) in [(Which::Keyboard, KEYBOARD_ID), (Which::Mouse, MOUSE_ID)] {
            let report_path = lock(shared).peripheral.report(which).path.clone();
            server
                .at(
                    report_path.as_str(),
                    ReportCharacteristic {
                        which,
                        shared: shared.clone(),
                    },
                )
                .await?;
            // Report Reference: input
            let reference = Descriptor {
                uuid: "2908",
                characteristic: report_path.clone(),
                value: vec![id, 1],
            };
            server.at(format!("{report_path}/desc0"), reference).await?;
        }
        let battery = "/org/bluff/gatt/service1";
        server.at(battery, Service { uuid: "180f" }).await?;
        let level = characteristic(battery, "2a19", &["read", "notify"], &[100]);
        server.at(format!("{battery}/char0"), level).await?;
        server.at(INFO_SERVICE, Service { uuid: "180a" }).await?;
        // PnP ID: USB vendor space, Linux Foundation VID 0x1d6b, arbitrary PID and version.
        let pnp = characteristic(
            INFO_SERVICE,
            "2a50",
            &["read"],
            &[0x02, 0x6B, 0x1D, 0x46, 0x42, 0x00, 0x01],
        );
        server.at(format!("{INFO_SERVICE}/char0"), pnp).await?;
        server
            .at(
                AGENT,
                Agent {
                    shared: shared.clone(),
                },
            )
            .await?;
        Ok(())
    }

    async fn drop_sockets(self, mut dropped: UnboundedReceiver<Dropped>) {
        while let Some(dropped) = dropped.recv().await {
            let mut bridge = lock(&self.inner.shared);
            if bridge.peripheral.drop_socket(&dropped) {
                bridge.subscription_changed();
            }
        }
    }

    /// Follow BlueZ's devices: pick up bluff's hosts, adopt new pairings, notice reconnects.
    async fn watch_hosts(&self) -> zbus::Result<()> {
        let conn = &self.inner.conn;
        let rule = MatchRule::builder()
            .msg_type(Type::Signal)
            .sender(BLUEZ)?
            .interface("org.freedesktop.DBus.Properties")?
            .member("PropertiesChanged")?
            .arg(0, DEVICE)?
            .build();
        let mut signals = MessageStream::for_match_rule(rule, conn, None).await?;
        let objects = ObjectManagerProxy::builder(conn)
            .destination(BLUEZ)?
            .path("/")?
            .build()
            .await?
            .get_managed_objects()
            .await?;
        let prefix = format!("{}/", self.inner.adapter);
        let mut blocked = Vec::new();
        {
            let mut bridge = lock(&self.inner.shared);
            let hosts = &mut bridge.peripheral.hosts;
            for (object, interfaces) in objects {
                let Some(props) = interfaces.get(DEVICE) else {
                    continue;
                };
                if !object.as_str().starts_with(&prefix) {
                    continue;
                }
                hosts.update(object.as_str(), props);
                if hosts.hosts.contains_key(object.as_str())
                    && props
                        .get("Blocked")
                        .is_some_and(|v| bool::try_from(v) == Ok(true))
                {
                    blocked.push(object.to_string());
                }
            }
            for host in hosts.hosts.values().filter(|host| host.connected) {
                self.host_connected(host.path.clone());
            }
        }
        for device in blocked {
            self.unblock(&device).await;
        }
        let ble = self.clone();
        tokio::spawn(async move {
            while let Some(Ok(message)) = signals.next().await {
                let Some(object) = message.header().path().map(|p| p.to_string()) else {
                    continue;
                };
                let Ok((_, changed, _)) =
                    message.body().deserialize::<(String, Props, Vec<String>)>()
                else {
                    continue;
                };
                let mut bridge = lock(&ble.inner.shared);
                match bridge
                    .peripheral
                    .hosts
                    .properties_changed(&object, &changed)
                {
                    Change::Ignored => {}
                    Change::Paired => {
                        tokio::spawn(ble.clone().adopt(object));
                    }
                    Change::Host { connected } => {
                        if connected {
                            ble.host_connected(object);
                        }
                        bridge.subscription_changed();
                    }
                }
            }
        });
        Ok(())
    }

    async fn unblock(&self, device: &str) {
        // Earlier versions blocked the hosts that were not chosen.
        let blocked = async {
            let props = PropertiesProxy::builder(&self.inner.conn)
                .destination(BLUEZ)?
                .path(device)?
                .build()
                .await?;
            props
                .set(
                    InterfaceName::from_static_str_unchecked(DEVICE),
                    "Blocked",
                    Value::from(false),
                )
                .await?;
            zbus::Result::Ok(())
        };
        match blocked.await {
            Ok(()) => info!("unblocked {device}"),
            Err(error) => warn!("could not unblock {device}: {error}"),
        }
    }

    async fn adopt(self, device: String) {
        let props = async {
            let proxy = PropertiesProxy::builder(&self.inner.conn)
                .destination(BLUEZ)?
                .path(device.as_str())?
                .build()
                .await?;
            Ok::<_, zbus::Error>(
                proxy
                    .get_all(InterfaceName::from_static_str_unchecked(DEVICE))
                    .await?,
            )
        };
        match props.await {
            Ok(props) => {
                let mut bridge = lock(&self.inner.shared);
                bridge.peripheral.hosts.adopt(&device, &props);
                bridge.subscription_changed();
            }
            Err(error) => warn!("could not read the new host {device}: {error}"),
        }
    }

    /// Choose the host that gets input.
    pub fn choose(&self, device: &str) {
        let mut bridge = lock(&self.inner.shared);
        bridge.peripheral.hosts.choose(device);
        bridge.subscription_changed();
    }

    /// Addresses with an LE link, or None when the kernel cannot say.
    fn le_links(&self) -> Option<HashSet<String>> {
        le_links(&self.inner.adapter)
            .inspect_err(|error| warn!("could not list LE links: {error}"))
            .ok()
    }

    fn host_connected(&self, device: String) {
        let ble = self.clone();
        tokio::spawn(async move {
            if let Err(error) = ble.resubscribe(&device).await {
                warn!("could not register again for {device}: {error}");
            }
        });
    }

    async fn resubscribe(&self, device: &str) -> zbus::Result<()> {
        // A bonded host keeps its subscription inside BlueZ across disconnects, so when it comes
        // back BlueZ sees nothing new and never calls AcquireNotify. Registering the app again
        // drops those subscriptions; connected hosts get Service Changed and subscribe again.
        sleep(RESUBSCRIBE).await;
        {
            let mut registration = self.inner.registration.lock().await;
            let le = self.le_links();
            let name = {
                let bridge = lock(&self.inner.shared);
                let Some(host) = bridge.peripheral.hosts.hosts.get(device) else {
                    return Ok(());
                };
                if !host.connected || bridge.peripheral.keyboard.sockets.contains_key(device) {
                    return Ok(());
                }
                // A dual-mode host back on BR/EDR only, say for audio, has nothing to subscribe on.
                if le.is_some_and(|le| !le.contains(&host.address)) {
                    return Ok(());
                }
                host.name.clone()
            };
            // Hosts that came back together share one registration; a second toggle would undo it.
            if registration.recent(Instant::now()) {
                return Ok(());
            }
            info!("{name:?} is back without subscribing; registering again so it does");
            self.call(
                &self.inner.adapter,
                GATT_MANAGER,
                "UnregisterApplication",
                &(path(ROOT),),
            )
            .await?;
            // A database hash the host has seen before makes it trust its old subscriptions and
            // write none, so every registration gets a spare characteristic with a new UUID.
            let server = self.inner.conn.object_server();
            if registration.spare {
                server.remove::<Characteristic, _>(SPARE).await?;
            }
            let spare = Characteristic {
                uuid: random_uuid()?,
                service: INFO_SERVICE,
                flags: &["read"],
                value: Vec::new(),
            };
            registration.spare = server.at(SPARE, spare).await?;
            self.register().await?;
            registration.at = Some(Instant::now());
        }
        sleep(RESUBSCRIBE).await;
        self.nudge_unsubscribed().await;
        Ok(())
    }

    async fn nudge_unsubscribed(&self) {
        // Android ignores Service Changed while connected and only rediscovers, and so
        // subscribes, when it reconnects. A disconnect is not a rejection: it comes right back.
        let now = Instant::now();
        // A newer registration just dropped every socket; hosts are still subscribing again,
        // and that registration nudges the ones that do not once they had the time.
        if self.inner.registration.lock().await.recent(now) {
            return;
        }
        // Disconnecting a host on BR/EDR only cuts its audio, and it comes back on BR/EDR again.
        let le = self.le_links();
        let hosts = {
            let mut nudged = self
                .inner
                .nudged
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let hosts = lock(&self.inner.shared)
                .peripheral
                .unsubscribed(&nudged, le.as_ref(), now);
            nudged.extend(hosts.iter().map(|host| (host.path.clone(), now)));
            hosts
        };
        for host in hosts {
            info!(
                "disconnecting {:?} so it reconnects and subscribes",
                host.name
            );
            if let Err(error) = self.call(&host.path, DEVICE, "Disconnect", &()).await {
                warn!("could not disconnect {:?}: {error}", host.name);
            }
        }
    }

    /// Become discoverable and accept pairing through `confirm`, or stop with None.
    pub async fn set_pairing(&self, confirm: Option<Confirm>) -> zbus::Result<()> {
        let on = confirm.is_some();
        lock(&self.inner.shared).peripheral.confirm = confirm;
        self.update_advertising().await?;
        info!("pairing mode {}", if on { "on" } else { "off" });
        Ok(())
    }

    async fn update_advertising(&self) -> zbus::Result<()> {
        let mut advertising = self.inner.advertising.lock().await;
        let server = self.inner.conn.object_server();
        if *advertising {
            self.call(
                &self.inner.adapter,
                ADVERTISING_MANAGER,
                "UnregisterAdvertisement",
                &(path(ADVERTISEMENT),),
            )
            .await?;
            server.remove::<Advertisement, _>(ADVERTISEMENT).await?;
        }
        let discoverable = lock(&self.inner.shared).peripheral.pairing();
        server
            .at(
                ADVERTISEMENT,
                Advertisement {
                    name: self.inner.name.clone(),
                    discoverable,
                },
            )
            .await?;
        let options: HashMap<&str, Value> = HashMap::new();
        self.call(
            &self.inner.adapter,
            ADVERTISING_MANAGER,
            "RegisterAdvertisement",
            &(path(ADVERTISEMENT), options),
        )
        .await?;
        *advertising = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::linux::hosts::tests::{props, temp_store};

    fn peripheral(test: &str) -> (Peripheral, UnboundedReceiver<Dropped>) {
        let store = temp_store(test);
        std::fs::write(&store, "AA\nBB\n").unwrap();
        let mut hosts = Hosts::load(store);
        hosts.update(
            "/d/a",
            &props(&[("Address", "AA".into()), ("Connected", true.into())]),
        );
        hosts.update(
            "/d/b",
            &props(&[("Address", "BB".into()), ("Connected", true.into())]),
        );
        Peripheral::new(hosts)
    }

    fn receive(fd: &OwnedFd) -> Vec<u8> {
        let mut buf = [0u8; 16];
        // SAFETY: receives into a valid buffer without blocking.
        let size = unsafe {
            libc::recv(
                fd.as_raw_fd(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                libc::MSG_DONTWAIT,
            )
        };
        buf[..size.max(0) as usize].to_vec()
    }

    #[tokio::test]
    async fn agent_refuses_pairing_outside_pairing_mode() {
        let (peripheral, _) = peripheral("refuse");
        let shared = Arc::new(Mutex::new(Bridge::new(
            peripheral,
            HashSet::new(),
            10.0,
            2.0,
        )));
        let agent = Agent { shared };
        let device = OwnedObjectPath::try_from("/d/c").unwrap();
        assert!(agent.confirm(&device, "pair?".into()).await.is_err());
    }

    #[tokio::test]
    async fn agent_asks_confirm_in_pairing_mode() {
        let (mut peripheral, _) = peripheral("confirm");
        let asked = Arc::new(Mutex::new(Vec::new()));
        let record = asked.clone();
        peripheral.confirm = Some(Arc::new(move |question| {
            record.lock().unwrap().push(question);
            Box::pin(async { true })
        }));
        let shared = Arc::new(Mutex::new(Bridge::new(
            peripheral,
            HashSet::new(),
            10.0,
            2.0,
        )));
        let agent = Agent {
            shared: shared.clone(),
        };
        agent
            .request_confirmation(OwnedObjectPath::try_from("/d/c").unwrap(), 42)
            .await
            .unwrap();
        assert_eq!(
            *asked.lock().unwrap(),
            ["Pair with /d/c? The device must show 000042."]
        );
        let paired = props(&[("Paired", true.into())]);
        assert_eq!(
            lock(&shared)
                .peripheral
                .hosts
                .properties_changed("/d/c", &paired),
            Change::Paired
        );
    }

    #[tokio::test]
    async fn reports_go_only_to_the_chosen_host() {
        let (mut peripheral, _) = peripheral("chosen");
        let a = peripheral.acquire(Which::Keyboard, "/d/a").unwrap();
        let b = peripheral.acquire(Which::Keyboard, "/d/b").unwrap();
        peripheral.hosts.choose("/d/b");
        peripheral.notify_keyboard(&[1, 2]);
        assert_eq!((receive(&a), receive(&b)), (vec![], vec![1, 2]));
        assert!(peripheral.connected());
    }

    #[tokio::test]
    async fn acquire_notify_hands_out_a_socket_until_bluez_hangs_up() {
        let (mut peripheral, mut dropped) = peripheral("hangup");
        let theirs = peripheral.acquire(Which::Mouse, "/d/a").unwrap();
        assert!(peripheral.mouse.sockets.contains_key("/d/a"));
        drop(theirs);
        let hangup = dropped.recv().await.unwrap();
        assert!(peripheral.drop_socket(&hangup));
        assert!(peripheral.mouse.sockets.is_empty());
    }

    #[tokio::test]
    async fn a_failed_send_drops_the_host_after_the_send_returns() {
        let (mut peripheral, mut dropped) = peripheral("failed");
        drop(peripheral.acquire(Which::Keyboard, "/d/a").unwrap());
        peripheral.notify_keyboard(&[1]);
        assert!(peripheral.keyboard.sockets.contains_key("/d/a"));
        assert!(peripheral.drop_socket(&dropped.recv().await.unwrap()));
        assert!(!peripheral.connected());
    }

    #[tokio::test]
    async fn a_replaced_socket_is_not_dropped_by_its_old_hangup() {
        let (mut peripheral, mut dropped) = peripheral("replaced");
        drop(peripheral.acquire(Which::Keyboard, "/d/a").unwrap());
        let old = dropped.recv().await.unwrap();
        let _theirs = peripheral.acquire(Which::Keyboard, "/d/a").unwrap();
        assert!(!peripheral.drop_socket(&old));
        assert!(peripheral.connected());
    }

    #[tokio::test]
    async fn hosts_without_a_socket_are_nudged_once() {
        let (mut peripheral, _) = peripheral("nudge");
        let _a = peripheral.acquire(Which::Keyboard, "/d/a").unwrap();
        let now = Instant::now();
        let hosts = peripheral.unsubscribed(&HashMap::new(), None, now);
        assert_eq!(
            hosts.iter().map(|h| h.path.as_str()).collect::<Vec<_>>(),
            ["/d/b"]
        );
        let nudged = HashMap::from([("/d/b".to_string(), now)]);
        assert!(
            peripheral
                .unsubscribed(&nudged, None, now + Duration::from_secs(60))
                .is_empty()
        );
        assert_eq!(peripheral.unsubscribed(&nudged, None, now + NUDGE).len(), 1);
    }

    #[test]
    fn hosts_on_br_edr_only_are_not_nudged() {
        let (peripheral, _) = peripheral("bredr");
        let le = HashSet::from(["AA".to_string()]);
        let hosts = peripheral.unsubscribed(&HashMap::new(), Some(&le), Instant::now());
        assert_eq!(
            hosts.iter().map(|h| h.path.as_str()).collect::<Vec<_>>(),
            ["/d/a"]
        );
    }

    #[test]
    fn addresses_read_most_significant_byte_first() {
        assert_eq!(
            address([0xBD, 0xE0, 0x4F, 0x16, 0xEE, 0x34]),
            "34:EE:16:4F:E0:BD"
        );
    }

    #[test]
    fn no_nudge_right_after_another_registration() {
        let now = Instant::now();
        let registration = Registration {
            spare: true,
            at: Some(now),
        };
        assert!(registration.recent(now + Duration::from_secs(5)));
        assert!(!registration.recent(now + RESUBSCRIBE));
        assert!(!Registration::default().recent(now));
    }

    #[test]
    fn random_uuids_are_version_4() {
        let uuid = random_uuid().unwrap();
        assert_eq!((uuid.len(), &uuid[14..15]), (36, "4"));
        assert_ne!(uuid, random_uuid().unwrap());
    }
}
