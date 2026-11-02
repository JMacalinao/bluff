//! WinRT GATT server for a HID-over-GATT peripheral. Windows runs pairing itself.

use std::collections::{BTreeSet, HashMap};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use log::{info, warn};
use tokio::sync::mpsc::UnboundedSender;
use windows::Devices::Bluetooth::BluetoothLEDevice;
use windows::Devices::Bluetooth::GenericAttributeProfile::{
    GattCharacteristicProperties, GattCommunicationStatus, GattLocalCharacteristic,
    GattLocalCharacteristicParameters, GattLocalDescriptorParameters, GattLocalService,
    GattProtectionLevel, GattServiceProvider, GattServiceProviderAdvertisementStatus,
    GattServiceProviderAdvertisingParameters, GattSubscribedClient,
};
use windows::Foundation::TypedEventHandler;
use windows::Storage::Streams::{DataWriter, IBuffer};
use windows::core::{GUID, HSTRING};

use crate::bridge;
use crate::hid::{KEYBOARD_ID, MOUSE_ID, REPORT_MAP};

// Report reads need an encrypted link, which is what makes iPadOS bond.
const SECURE: GattProtectionLevel = GattProtectionLevel::EncryptionRequired;

pub fn default_store() -> PathBuf {
    let local = env::var_os("LOCALAPPDATA")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(&env::var_os("USERPROFILE").unwrap_or_default()).join("AppData/Local")
        });
    local.join("bluff").join("hosts")
}

fn uuid(short: u16) -> GUID {
    GUID::from_values(
        u32::from(short),
        0,
        0x1000,
        [0x80, 0x00, 0x00, 0x80, 0x5F, 0x9B, 0x34, 0xFB],
    )
}

fn buffer(data: &[u8]) -> windows::core::Result<IBuffer> {
    let writer = DataWriter::new()?;
    writer.WriteBytes(data)?;
    writer.DetachBuffer()
}

fn device_id(client: &GattSubscribedClient) -> windows::core::Result<String> {
    Ok(client.Session()?.DeviceId()?.Id()?.to_string())
}

/// The host's address, from a device id like `BluetoothLE#BluetoothLE<ours>-<host>`.
fn address(device_id: &str) -> &str {
    device_id.rsplit('-').next().unwrap_or(device_id)
}

fn write(path: &Path, text: &str) {
    let written = path
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| fs::write(path, text));
    if let Err(error) = written {
        warn!("could not write {}: {error}", path.display());
    }
}

async fn characteristic(
    service: &GattLocalService,
    short: u16,
    properties: GattCharacteristicProperties,
    value: Option<&[u8]>,
    protection: GattProtectionLevel,
) -> windows::core::Result<GattLocalCharacteristic> {
    let parameters = GattLocalCharacteristicParameters::new()?;
    parameters.SetCharacteristicProperties(properties)?;
    parameters.SetReadProtectionLevel(protection)?;
    if let Some(value) = value {
        parameters.SetStaticValue(&buffer(value)?)?;
    }
    let result = service
        .CreateCharacteristicAsync(uuid(short), &parameters)?
        .await?;
    result.Characteristic().inspect_err(|_| {
        warn!(
            "could not create characteristic {short:04x}: {:?}",
            result.Error()
        )
    })
}

async fn provider(short: u16) -> windows::core::Result<GattServiceProvider> {
    let result = GattServiceProvider::CreateAsync(uuid(short))?.await?;
    result
        .ServiceProvider()
        .inspect_err(|_| warn!("could not create service {short:04x}: {:?}", result.Error()))
}

/// Input report sent to the chosen host only, or to every subscribed host when none is chosen.
pub struct Report {
    characteristic: GattLocalCharacteristic,
    queue: mpsc::Sender<(Vec<u8>, GattSubscribedClient)>,
}

impl Report {
    fn new(characteristic: GattLocalCharacteristic) -> Self {
        let (queue, queued) = mpsc::channel::<(Vec<u8>, GattSubscribedClient)>();
        let sender = characteristic.clone();
        // One notification at a time: WinRT may finish concurrent ones out of order, and a
        // key release that overtakes its press leaves the key held on the host.
        thread::spawn(move || {
            for (value, client) in queued {
                let sent = buffer(&value).and_then(|value| {
                    sender
                        .NotifyValueForSubscribedClientAsync(&value, &client)?
                        .join()
                });
                match sent.and_then(|result| result.Status()) {
                    Ok(GattCommunicationStatus::Success) => {}
                    Ok(status) => warn!("report failed: {status:?}"),
                    Err(error) => warn!("report failed: {error}"),
                }
            }
        });
        Self {
            characteristic,
            queue,
        }
    }

    /// Subscribed clients with their device ids.
    fn clients(&self) -> Vec<(GattSubscribedClient, String)> {
        let Ok(clients) = self.characteristic.SubscribedClients() else {
            return Vec::new();
        };
        clients
            .into_iter()
            .filter_map(|client| Some((client.clone(), device_id(&client).ok()?)))
            .collect()
    }

    fn notify(&self, value: &[u8], target: Option<&str>) {
        for (client, id) in self.clients() {
            if target.is_none_or(|target| target == address(&id)) {
                let _ = self.queue.send((value.to_vec(), client));
            }
        }
    }
}

/// HID-over-GATT keyboard and mouse published through WinRT.
///
/// Windows owns the adapter's name and pairing: hosts see the computer name,
/// and Windows asks the user to confirm a new pairing. Outside pairing mode
/// bluff advertises without being discoverable, so only bonded hosts find it.
/// Bluff remembers the hosts that subscribed in `store`; a host subscribing
/// for the first time gets the input from then on.
pub struct Peripheral {
    store: PathBuf,
    chosen_store: PathBuf,
    addresses: BTreeSet<String>,
    pub chosen_address: String,
    pub names: HashMap<String, String>,
    pub pairing: bool,
    keyboard: Report,
    mouse: Report,
    providers: Vec<GattServiceProvider>,
}

impl Peripheral {
    /// Publish the services and start advertising; `changed` hears when hosts subscribe or leave.
    pub async fn start(
        store: PathBuf,
        changed: UnboundedSender<()>,
    ) -> windows::core::Result<Self> {
        let hid = provider(0x1812).await?;
        let service = hid.Service()?;
        let read = GattCharacteristicProperties::Read;
        let plain = GattProtectionLevel::Plain;
        // HID 1.11, normally connectable
        characteristic(
            &service,
            0x2A4A,
            read,
            Some(&[0x11, 0x01, 0x00, 0x02]),
            plain,
        )
        .await?;
        characteristic(&service, 0x2A4B, read, Some(REPORT_MAP), SECURE).await?;
        let control = GattCharacteristicProperties::WriteWithoutResponse;
        characteristic(&service, 0x2A4C, control, None, plain).await?;
        let mut reports = Vec::new();
        for (report_id, size) in [(KEYBOARD_ID, 8), (MOUSE_ID, 7)] {
            let properties = read | GattCharacteristicProperties::Notify;
            let report =
                characteristic(&service, 0x2A4D, properties, Some(&vec![0; size]), SECURE).await?;
            let reference = GattLocalDescriptorParameters::new()?;
            reference.SetStaticValue(&buffer(&[report_id, 1])?)?; // Report Reference: input
            reference.SetReadProtectionLevel(SECURE)?;
            report
                .CreateDescriptorAsync(uuid(0x2908), &reference)?
                .await?;
            let changed = changed.clone();
            // WinRT raises this on one of its own threads.
            report.SubscribedClientsChanged(&TypedEventHandler::new(move |_, _| {
                let _ = changed.send(());
                Ok(())
            }))?;
            reports.push(Report::new(report));
        }
        let battery = provider(0x180F).await?;
        characteristic(&battery.Service()?, 0x2A19, read, Some(&[100]), plain).await?;
        // Device Information (180a) is Windows' own; it refuses to publish another.
        let chosen_store = store.with_file_name("chosen");
        let addresses = fs::read_to_string(&store).unwrap_or_default();
        let chosen = fs::read_to_string(&chosen_store).unwrap_or_default();
        let [keyboard, mouse] = <[Report; 2]>::try_from(reports).ok().expect("two reports");
        let peripheral = Self {
            addresses: addresses.split_whitespace().map(String::from).collect(),
            chosen_address: chosen.trim().to_string(),
            store,
            chosen_store,
            names: HashMap::new(),
            pairing: false,
            keyboard,
            mouse,
            providers: vec![hid, battery],
        };
        peripheral.advertise()?;
        info!("advertising as a keyboard and mouse");
        Ok(peripheral)
    }

    fn target(&self) -> Option<&str> {
        Some(self.chosen_address.as_str()).filter(|address| !address.is_empty())
    }

    /// Addresses of the hosts subscribed now, with their device ids.
    pub fn hosts(&self) -> Vec<(String, String)> {
        let clients = self.keyboard.clients();
        clients
            .into_iter()
            .map(|(_, id)| (address(&id).to_string(), id))
            .collect()
    }

    /// Send input only to the host with address `host` from now on.
    pub fn choose(&mut self, host: &str) {
        self.chosen_address = host.to_string();
        write(&self.chosen_store, &format!("{host}\n"));
        info!(
            "input is for {:?} from now on",
            self.names.get(host).map_or(host, String::as_str)
        );
    }

    /// Advertise as discoverable so a new host can pair, or stop.
    pub fn set_pairing(&mut self, on: bool) -> windows::core::Result<()> {
        self.pairing = on;
        self.advertise()?;
        info!("pairing mode {}", if on { "on" } else { "off" });
        Ok(())
    }

    fn advertise(&self) -> windows::core::Result<()> {
        let parameters = GattServiceProviderAdvertisingParameters::new()?;
        parameters.SetIsConnectable(true)?;
        parameters.SetIsDiscoverable(self.pairing)?;
        for provider in &self.providers {
            // Stopping a provider that never started raises.
            if provider.AdvertisementStatus()? != GattServiceProviderAdvertisementStatus::Created {
                provider.StopAdvertising()?;
            }
            provider.StartAdvertisingWithParameters(&parameters)?;
        }
        Ok(())
    }

    /// Take in the subscribed hosts; return the addresses and device ids of hosts not named yet.
    pub fn clients_changed(&mut self) -> Vec<(String, String)> {
        let mut unnamed = Vec::new();
        for (host, id) in self.hosts() {
            if !self.names.contains_key(&host) {
                self.names.insert(host.clone(), host.clone());
                unnamed.push((host.clone(), id));
            }
            if self.addresses.insert(host.clone()) {
                let text: String = self
                    .addresses
                    .iter()
                    .map(|address| format!("{address}\n"))
                    .collect();
                write(&self.store, &text);
                info!("new host {host}");
                self.choose(&host);
            }
        }
        let names: Vec<&str> = self
            .hosts()
            .iter()
            .map(|(host, _)| self.names[host].as_str())
            .collect();
        info!(
            "subscribed hosts: {}",
            if names.is_empty() {
                "none".into()
            } else {
                names.join(", ")
            }
        );
        unnamed
    }
}

/// The name Windows knows a host by.
pub async fn name(device_id: &str) -> Option<String> {
    let device = BluetoothLEDevice::FromIdAsync(&HSTRING::from(device_id))
        .ok()?
        .await
        .ok()?;
    Some(device.Name().ok()?.to_string()).filter(|name| !name.is_empty())
}

impl bridge::Peripheral for Peripheral {
    fn notify_keyboard(&mut self, report: &[u8]) {
        self.keyboard.notify(report, self.target());
    }

    fn notify_mouse(&mut self, report: &[u8]) {
        self.mouse.notify(report, self.target());
    }

    /// Whether input sent now reaches a host: the chosen one, or any when none is chosen.
    fn connected(&self) -> bool {
        let target = self.target();
        self.hosts()
            .iter()
            .any(|(host, _)| target.is_none_or(|target| target == host))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_is_the_end_of_the_device_id() {
        assert_eq!(
            address("BluetoothLE#BluetoothLE00:11:22:33:44:55-aa:bb:cc:dd:ee:ff"),
            "aa:bb:cc:dd:ee:ff"
        );
    }

    #[test]
    fn short_uuids_use_the_bluetooth_base() {
        assert_eq!(
            format!("{:?}", uuid(0x1812)),
            "00001812-0000-1000-8000-00805F9B34FB"
        );
    }
}
