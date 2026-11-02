//! Windows: WinRT GATT, low-level input hooks, a notification-area icon.

pub mod ble;
pub mod hooks;
pub mod touchpad;
pub mod tray;

use std::sync::{Arc, Mutex};

use log::info;
use tokio::sync::{mpsc, watch};

use crate::bridge::{self, Bridge, lock};
use crate::cli::{Args, Error};
use crate::input::{Input, Kind};
use ble::{Peripheral, default_store};
use hooks::Grab;
use touchpad::UNITS_PER_MM;
use tray::Shared;

pub async fn run(args: Args) -> Result<(), Error> {
    let (changed, mut changes) = mpsc::unbounded_channel();
    let peripheral = Peripheral::start(default_store(), changed)
        .await
        .map_err(|error| format!("could not publish the keyboard: {error}"))?;
    let shared: Shared = Arc::new(Mutex::new(Bridge::new(
        peripheral,
        args.hotkey,
        args.pixels_per_mm,
        args.mm_per_tick,
    )));
    if args.pair {
        lock(&shared).peripheral.set_pairing(true)?;
        info!("pick this computer in the device's Bluetooth settings; Windows asks to confirm");
    }
    let grab = Arc::new(Grab::default());
    let indices = {
        let mut bridge = lock(&shared);
        hooks::devices(&grab).map(|device| {
            let touchpad = device.info().kind == Kind::Touchpad;
            bridge.add(Box::new(device), if touchpad { UNITS_PER_MM } else { 0 })
        })
    };
    let (events, received) = mpsc::unbounded_channel();
    hooks::start(grab, indices, events);
    let (quit, quitting) = watch::channel(false);
    let tray = tray::start(shared.clone(), quit);
    let hosts = shared.clone();
    tokio::spawn(async move {
        while changes.recv().await.is_some() {
            let unnamed = {
                let mut bridge = lock(&hosts);
                let unnamed = bridge.peripheral.clients_changed();
                bridge.subscription_changed();
                unnamed
            };
            for (host, device_id) in unnamed {
                let named = hosts.clone();
                tokio::spawn(async move {
                    if let Some(name) = ble::name(&device_id).await {
                        let mut bridge = lock(&named);
                        bridge.peripheral.names.insert(host, name);
                        bridge.subscription_changed();
                    }
                });
            }
        }
    });
    info!("press and release the hotkey to switch");
    bridge::pump(&shared, received, quitting).await;
    // Hooks die with the process, but the touchpad's scrolling setting has to be put back.
    {
        let mut bridge = lock(&shared);
        if bridge.active {
            bridge.set_active(false);
        }
    }
    tray.stop();
    Ok(())
}
