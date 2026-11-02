//! Linux: BlueZ over the system bus, evdev input, a StatusNotifierItem tray.

pub mod ble;
pub mod evdev;
pub mod hosts;
pub mod tray;

use std::io::{self, BufRead, Write};
use std::sync::{Arc, Mutex};

use log::{info, warn};
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, sleep_until};

use crate::bridge::Bridge;
use crate::cli::{Args, Error};
use crate::input::{ABS_X, Input, Kind};
use ble::{Ble, Confirm, Peripheral, Shared, lock};
use evdev::{Device, discover};
use hosts::{Hosts, default_store};

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
    let (events, mut received) = mpsc::unbounded_channel();
    for info in discover()? {
        let name = info.name.clone();
        let opened = Device::open(info).and_then(|device| Ok((device.reader()?, device)));
        match opened {
            Ok((reader, device)) => {
                let touchpad = device.info().kind == Kind::Touchpad;
                let resolution = if touchpad {
                    device.resolution(ABS_X)
                } else {
                    0
                };
                reader(
                    lock(&shared).add(Box::new(device), resolution),
                    events.clone(),
                );
            }
            Err(error) => warn!("could not open {name}: {error}"),
        }
    }
    let (quit, mut quitting) = watch::channel(false);
    tray::start(shared.clone(), ble, quit).await;
    if args.pair {
        info!("pick {:?} in the device's Bluetooth settings", args.name);
    }
    info!("press and release the hotkey to switch");
    loop {
        let flush_at = lock(&shared).flush_at();
        let deadline = flush_at.map_or_else(Instant::now, Instant::from_std);
        tokio::select! {
            Some((device, type_, code, value)) = received.recv() => lock(&shared).event(device, type_, code, value),
            () = sleep_until(deadline), if flush_at.is_some() => lock(&shared).flush(),
            _ = quitting.changed() => return Ok(()),
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
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
