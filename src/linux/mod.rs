//! Linux: BlueZ over the system bus, evdev input, a StatusNotifierItem tray.

pub mod ble;
pub mod evdev;
pub mod hosts;
pub mod tray;

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, Write};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use log::{info, warn};
use tokio::sync::{mpsc, watch};
use tokio::time::sleep;

use crate::bridge::{self, Bridge};
use crate::cli::{Args, Error};
use crate::input::{ABS_X, Input, Kind};
use ble::{Ble, Confirm, Peripheral, Shared, lock};
use evdev::{Device, discover};
use hosts::{Hosts, default_store};

/// How often to look for input devices plugged in, or created again, after start.
const RESCAN: Duration = Duration::from_secs(2);

pub async fn run(args: Args) -> Result<(), Error> {
    let (peripheral, dropped) = Peripheral::new(Hosts::load(default_store()));
    let bridge = Bridge::new(
        peripheral,
        args.hotkey,
        args.pixels_per_mm,
        args.mm_per_tick,
    );
    let shared: Shared = Arc::new(Mutex::new(bridge));
    let ble = Ble::start(shared.clone(), dropped, &args.name, &args.adapter)
        .await
        .map_err(|error| format!("could not register with BlueZ: {error}"))?;
    if args.pair {
        ble.set_pairing(Some(ask_terminal())).await?;
    }
    let (events, received) = mpsc::unbounded_channel();
    let mut known = HashMap::new();
    open_new(&shared, &events, &mut known)?;
    tokio::spawn({
        let shared = shared.clone();
        let events = events.clone();
        async move {
            loop {
                sleep(RESCAN).await;
                if let Err(error) = open_new(&shared, &events, &mut known) {
                    warn!("could not look for new input devices: {error}");
                }
            }
        }
    });
    let (quit, quitting) = watch::channel(false);
    tray::start(shared.clone(), ble, quit).await;
    if args.pair {
        info!("pick {:?} in the device's Bluetooth settings", args.name);
    }
    info!("press and release the hotkey to switch");
    bridge::pump(&shared, received, quitting).await;
    Ok(())
}

/// Open input devices not open yet, and hand them to the bridge.
///
/// `known` maps each node's path to its inode and whether it opened: a node the kernel removed
/// and created again, say when a dock resets over suspend, keeps its path but gets a new inode.
/// A node that failed to open is tried again every `RESCAN`, because udev grants the `input`
/// group access a moment after the node appears, but its warning is logged once.
fn open_new(
    shared: &Shared,
    events: &mpsc::UnboundedSender<bridge::Event>,
    known: &mut HashMap<PathBuf, (u64, bool)>,
) -> io::Result<()> {
    for info in discover()? {
        let Ok(inode) = fs::metadata(&info.path).map(|meta| meta.ino()) else {
            continue;
        };
        let warned = match known.get(&info.path) {
            Some(&(seen, opened)) if seen == inode => {
                if opened {
                    continue;
                }
                true
            }
            _ => false,
        };
        let path = info.path.clone();
        let name = info.name.clone();
        let opened = Device::open(info).and_then(|device| Ok((device.reader()?, device)));
        known.insert(path, (inode, opened.is_ok()));
        match opened {
            Ok((reader, device)) => {
                let touchpad = device.info().kind == Kind::Touchpad;
                let resolution = if touchpad {
                    device.resolution(ABS_X)
                } else {
                    0
                };
                reader(
                    lock(shared).add(Box::new(device), resolution),
                    events.clone(),
                );
            }
            Err(error) if !warned => warn!("could not open {name}: {error}; trying again"),
            Err(_) => {}
        }
    }
    Ok(())
}

fn ask_terminal() -> Confirm {
    Arc::new(|question| {
        Box::pin(async move {
            let answer = tokio::task::spawn_blocking(move || {
                print!("{question} [y/N] ");
                io::stdout().flush()?;
                let mut answer = String::new();
                io::stdin().lock().read_line(&mut answer)?;
                io::Result::Ok(answer)
            });
            matches!(answer.await, Ok(Ok(answer)) if answer.trim().eq_ignore_ascii_case("y"))
        })
    })
}
