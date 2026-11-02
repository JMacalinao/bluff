//! Notification-area icon: shows where input goes, switches on click, and runs pairing mode.

use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use log::warn;
use tokio::sync::{mpsc::unbounded_channel, watch};
use tokio::task::AbortHandle;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, PostThreadMessageW, WM_APP, WM_QUIT,
};

use crate::bridge::{Bridge, Peripheral as _, lock};
use crate::icons::{self, pixmaps, summary};
use crate::windows::ble::{self, Peripheral};

pub type Shared = Arc<Mutex<Bridge<Peripheral>>>;

const PAIRING: Duration = Duration::from_secs(120);
const REFRESH: u32 = WM_APP;
const TOGGLE: &str = "toggle";
const PAIR: &str = "pair";
const QUIT: &str = "quit";
const HOST: &str = "host:";

/// The largest frame of a tray icon; Windows scales it to the notification area.
fn image(icon: icons::Icon) -> Option<Icon> {
    let frame = pixmaps(icon).into_iter().max_by_key(|frame| frame.width)?;
    let rgba = frame
        .argb
        .chunks_exact(4)
        .flat_map(|argb| [argb[1], argb[2], argb[3], argb[0]])
        .collect();
    Icon::from_rgba(rgba, frame.width, frame.height).ok()
}

fn menu(bridge: &Bridge<Peripheral>) -> Menu {
    let peripheral = &bridge.peripheral;
    let can_switch = peripheral.connected() && !bridge.active;
    let menu = Menu::new();
    let status = MenuItem::new(summary(icons::icon(bridge)), false, None);
    let toggle = MenuItem::with_id(TOGGLE, "Switch to device", can_switch, None);
    let mut items: Vec<Box<dyn tray_icon::menu::IsMenuItem>> =
        vec![Box::new(status), Box::new(toggle)];
    for (host, _) in peripheral.hosts() {
        let name = peripheral.names.get(&host).unwrap_or(&host);
        let chosen = host == peripheral.chosen_address;
        items.push(Box::new(CheckMenuItem::with_id(
            format!("{HOST}{host}"),
            name,
            true,
            chosen,
            None,
        )));
    }
    items.push(Box::new(PredefinedMenuItem::separator()));
    let pair = if peripheral.pairing {
        "Stop pairing"
    } else {
        "Pair a new device…"
    };
    items.push(Box::new(MenuItem::with_id(PAIR, pair, true, None)));
    items.push(Box::new(MenuItem::with_id(QUIT, "Quit bluff", true, None)));
    for item in &items {
        let _ = menu.append(item.as_ref());
    }
    menu
}

/// The icon runs on its own thread; its menu reads state there and hands clicks to the event loop.
pub struct Tray {
    shared: Shared,
    quit: watch::Sender<bool>,
    thread: u32,
    icon_thread: Mutex<Option<JoinHandle<()>>>,
    pairing_timer: Mutex<Option<AbortHandle>>,
}

pub fn start(shared: Shared, quit: watch::Sender<bool>) -> Arc<Tray> {
    let (clicks, mut clicked) = unbounded_channel::<String>();
    let menu_clicks = clicks.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let _ = menu_clicks.send(event.id.0);
    }));
    // A left click on the icon switches to the device; a right click opens the menu.
    TrayIconEvent::set_event_handler(Some(move |event| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            let _ = clicks.send(TOGGLE.to_string());
        }
    }));
    let (started, thread) = mpsc::channel();
    let state = shared.clone();
    let icon_thread = thread::Builder::new()
        .name("tray".into())
        .spawn(move || run(&state, started))
        .expect("spawn the tray thread");
    let tray = Arc::new(Tray {
        shared,
        quit,
        thread: thread.recv().expect("the tray thread starts"),
        icon_thread: Mutex::new(Some(icon_thread)),
        pairing_timer: Mutex::new(None),
    });
    let refresher = tray.clone();
    lock(&tray.shared).on_change = Box::new(move || refresher.refresh());
    let handler = tray.clone();
    tokio::spawn(async move {
        while let Some(id) = clicked.recv().await {
            handler.clicked(&id);
        }
    });
    tray.refresh();
    tray
}

fn run(shared: &Shared, started: mpsc::Sender<u32>) {
    let built = TrayIconBuilder::new()
        .with_tooltip("bluff")
        .with_menu_on_left_click(false)
        .build();
    // Only now does the thread have the message queue that refreshes get posted to.
    // SAFETY: asks for this thread's id.
    let _ = started.send(unsafe { GetCurrentThreadId() });
    let icon: TrayIcon = match built {
        Ok(icon) => icon,
        Err(error) => {
            warn!("running without a tray icon: {error}");
            return;
        }
    };
    let mut message = MSG::default();
    // SAFETY: a plain message loop on this thread, which owns the icon.
    while unsafe { GetMessageW(&mut message, None::<HWND>, 0, 0) }.0 > 0 {
        if message.message == REFRESH {
            let bridge = lock(shared);
            let current = icons::icon(&bridge);
            let _ = icon.set_icon(image(current));
            let _ = icon.set_tooltip(Some(format!("bluff: {}", summary(current))));
            icon.set_menu(Some(Box::new(menu(&bridge))));
        }
        // SAFETY: dispatches the message just received.
        unsafe { DispatchMessageW(&message) };
    }
}

impl Tray {
    /// Redraw the icon and menu from the current state; safe from any thread.
    pub fn refresh(&self) {
        // SAFETY: posts a message without pointers to the tray thread.
        let _ = unsafe { PostThreadMessageW(self.thread, REFRESH, WPARAM(0), LPARAM(0)) };
    }

    /// Remove the icon and end its thread.
    pub fn stop(&self) {
        // SAFETY: posts a message without pointers to the tray thread.
        let _ = unsafe { PostThreadMessageW(self.thread, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if let Some(thread) = self
            .icon_thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = thread.join();
        }
    }

    fn clicked(self: &Arc<Self>, id: &str) {
        let mut bridge = lock(&self.shared);
        match id {
            TOGGLE => {
                if bridge.peripheral.connected() && !bridge.active {
                    bridge.set_active(true);
                }
            }
            PAIR => {
                let on = !bridge.peripheral.pairing;
                drop(bridge);
                self.clone().set_pairing(on);
            }
            QUIT => {
                self.quit.send_replace(true);
            }
            _ => {
                if let Some(host) = id.strip_prefix(HOST) {
                    bridge.peripheral.choose(host);
                    bridge.subscription_changed();
                }
            }
        }
    }

    fn set_pairing(self: Arc<Self>, on: bool) {
        let mut timer = self
            .pairing_timer
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(timer) = timer.take() {
            timer.abort();
        }
        if on {
            let tray = self.clone();
            let pairing = tokio::spawn(async move {
                tokio::time::sleep(PAIRING).await;
                // Taken first so that switching off does not abort this very task.
                tray.pairing_timer
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                tray.set_pairing(false);
            });
            *timer = Some(pairing.abort_handle());
        }
        drop(timer);
        tokio::spawn(async move {
            if let Err(error) = ble::set_pairing(&self.shared, on).await {
                warn!("could not switch pairing mode: {error}");
            }
            self.refresh();
        });
    }
}
