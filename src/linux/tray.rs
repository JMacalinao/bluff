//! Tray icon (StatusNotifierItem with a DBusMenu) and notification prompts on the session bus.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures_util::StreamExt;
use log::{info, warn};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::AbortHandle;
use tokio::time::{sleep, timeout};
use zbus::fdo::DBusProxy;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};
use zbus::{Connection, MatchRule, MessageStream, interface};

use crate::bridge::{Bridge, Peripheral as _};
use crate::icons::{Icon, icon, pixmaps, summary};
use crate::linux::ble::{Ble, Confirm, Peripheral, Shared, lock};

const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
const PAIRING: Duration = Duration::from_secs(120);
const PROMPT: Duration = Duration::from_secs(60);
const RECONNECT: Duration = Duration::from_secs(2);

pub const STATUS: i32 = 1;
pub const TOGGLE: i32 = 2;
pub const PAIR: i32 = 3;
pub const QUIT: i32 = 4;
pub const FIRST_HOST_ITEM: i32 = 100;

type Props = HashMap<String, OwnedValue>;
type Pixmaps = Vec<(i32, i32, Vec<u8>)>;

fn status(bridge: &Bridge<Peripheral>) -> &'static str {
    if bridge.peripheral.pairing() {
        "NeedsAttention"
    } else {
        "Active"
    }
}

fn props<const N: usize>(pairs: [(&str, Value<'_>); N]) -> Props {
    pairs
        .into_iter()
        .map(|(k, v)| (k.to_string(), OwnedValue::try_from(v).expect("no fds")))
        .collect()
}

/// The flat menu: (item id, dbusmenu properties).
pub fn menu(bridge: &Bridge<Peripheral>) -> Vec<(i32, Props)> {
    let peripheral = &bridge.peripheral;
    let can_switch = peripheral.connected() && !bridge.active;
    let mut items = vec![
        (
            STATUS,
            props([
                ("label", summary(icon(bridge)).into()),
                ("enabled", false.into()),
            ]),
        ),
        (
            TOGGLE,
            props([
                ("label", "Switch to device".into()),
                ("enabled", can_switch.into()),
            ]),
        ),
    ];
    for (index, host) in (FIRST_HOST_ITEM..).zip(peripheral.hosts.sorted()) {
        let chosen = i32::from(host.address == peripheral.hosts.chosen_address);
        let toggle = [
            ("label", host.name.as_str().into()),
            ("toggle-type", "radio".into()),
            ("toggle-state", chosen.into()),
        ];
        items.push((index, props(toggle)));
    }
    let pair = if peripheral.pairing() {
        "Stop pairing"
    } else {
        "Pair a new device…"
    };
    items.push((PAIR, props([("label", pair.into())])));
    items.push((QUIT, props([("label", "Quit bluff".into())])));
    items
}

fn icon_pixmaps(icon: Icon) -> Pixmaps {
    pixmaps(icon)
        .into_iter()
        .map(|f| (f.width as i32, f.height as i32, f.argb.to_vec()))
        .collect()
}

/// Shows where input goes, switches on click, and runs pairing with notification prompts.
pub struct Tray {
    shared: Shared,
    ble: Ble,
    quit: watch::Sender<bool>,
    conn: Mutex<Option<Connection>>,
    prompts: Mutex<HashMap<u32, oneshot::Sender<bool>>>,
    pairing_timer: Mutex<Option<AbortHandle>>,
    revision: AtomicU32,
}

fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Start the tray, or log and return None when there is no usable session bus.
pub async fn start(shared: Shared, ble: Ble, quit: watch::Sender<bool>) -> Option<Arc<Tray>> {
    let tray = Arc::new(Tray {
        shared,
        ble,
        quit,
        conn: Mutex::new(None),
        prompts: Mutex::default(),
        pairing_timer: Mutex::new(None),
        revision: AtomicU32::new(1),
    });
    let stream = match tray.connect().await {
        Ok(stream) => stream,
        Err(error) => {
            warn!("running without a tray icon: {error}");
            return None;
        }
    };
    let (changed, mut changes) = mpsc::unbounded_channel();
    lock(&tray.shared).on_change = Box::new(move || {
        let _ = changed.send(());
    });
    let refresher = tray.clone();
    tokio::spawn(async move {
        while changes.recv().await.is_some() {
            refresher.refresh().await;
        }
    });
    tokio::spawn(tray.clone().run(stream));
    Some(tray)
}

impl Tray {
    fn clicked(self: &Arc<Self>, item: i32) {
        let mut bridge = lock(&self.shared);
        match item {
            TOGGLE if bridge.peripheral.connected() && !bridge.active => bridge.set_active(true),
            PAIR => {
                tokio::spawn(self.clone().set_pairing(!bridge.peripheral.pairing()));
            }
            QUIT => {
                self.quit.send_replace(true);
            }
            _ => {
                let hosts = bridge.peripheral.hosts.sorted();
                let index = usize::try_from(item - FIRST_HOST_ITEM).ok();
                if let Some(host) = index.and_then(|index| hosts.get(index)) {
                    let path = host.path.clone();
                    drop(bridge);
                    self.ble.choose(&path);
                }
            }
        }
    }

    async fn connect(self: &Arc<Self>) -> zbus::Result<MessageStream> {
        let name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
        let conn = zbus::connection::Builder::session()?
            .serve_at(ITEM_PATH, Item { tray: self.clone() })?
            .serve_at(MENU_PATH, Menu { tray: self.clone() })?
            .name(name.as_str())?
            .build()
            .await?;
        let stream = MessageStream::from(&conn);
        let dbus = DBusProxy::new(&conn).await?;
        let watcher = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender("org.freedesktop.DBus")?
            .member("NameOwnerChanged")?
            .arg(0, WATCHER)?
            .build();
        dbus.add_match_rule(watcher).await?;
        let prompts = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface(NOTIFICATIONS)?
            .build();
        dbus.add_match_rule(prompts).await?;
        *guard(&self.conn) = Some(conn.clone());
        register(&conn, &name).await;
        Ok(stream)
    }

    async fn run(self: Arc<Self>, mut stream: MessageStream) {
        loop {
            while let Some(message) = stream.next().await {
                let Ok(message) = message else { continue };
                self.signal(&message).await;
            }
            if *self.quit.borrow() {
                return;
            }
            // zbus ends the stream when the connection drops, without saying why.
            warn!("lost the session bus; bringing the tray back");
            *guard(&self.conn) = None;
            loop {
                sleep(RECONNECT).await;
                match self.connect().await {
                    Ok(next) => {
                        stream = next;
                        break;
                    }
                    Err(error) => warn!("tray still has no session bus: {error}"),
                }
            }
        }
    }

    async fn signal(self: &Arc<Self>, message: &zbus::Message) {
        let header = message.header();
        if header.message_type() != zbus::message::Type::Signal {
            return;
        }
        let member = header.member().map(|m| m.as_str()).unwrap_or_default();
        let interface = header.interface().map(|i| i.as_str()).unwrap_or_default();
        if member == "NameOwnerChanged" {
            let Ok((name, _, owner)) = message.body().deserialize::<(String, String, String)>()
            else {
                return;
            };
            let conn = guard(&self.conn).clone();
            if let (true, false, Some(conn)) = (name == WATCHER, owner.is_empty(), conn) {
                register(
                    &conn,
                    &format!("org.kde.StatusNotifierItem-{}-1", std::process::id()),
                )
                .await;
            }
        } else if interface == NOTIFICATIONS {
            let accepted = match member {
                "ActionInvoked" => message
                    .body()
                    .deserialize::<(u32, String)>()
                    .map(|(id, action)| (id, action == "accept")),
                "NotificationClosed" => message
                    .body()
                    .deserialize::<(u32, u32)>()
                    .map(|(id, _)| (id, false)),
                _ => return,
            };
            if let Ok((id, accepted)) = accepted {
                if let Some(prompt) = guard(&self.prompts).remove(&id) {
                    let _ = prompt.send(accepted);
                }
            }
        }
    }

    async fn refresh(&self) {
        let Some(conn) = guard(&self.conn).clone() else {
            return;
        };
        let status = status(&lock(&self.shared));
        let revision = self.revision.fetch_add(1, Ordering::Relaxed) + 1;
        let sent = async {
            let item = SignalEmitter::new(&conn, ITEM_PATH)?;
            Item::new_icon(&item).await?;
            Item::new_tool_tip(&item).await?;
            Item::new_status(&item, status).await?;
            Menu::layout_updated(&SignalEmitter::new(&conn, MENU_PATH)?, revision, 0).await
        };
        if let Err(error) = sent.await {
            warn!("could not update the tray: {error}");
        }
    }

    // Boxed because pairing and its prompt start each other.
    fn set_pairing(self: Arc<Self>, on: bool) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            if let Some(timer) = guard(&self.pairing_timer).take() {
                timer.abort();
            }
            let confirm: Option<Confirm> = on.then(|| {
                let tray = self.clone();
                Arc::new(
                    move |question: String| -> Pin<Box<dyn Future<Output = bool> + Send>> {
                        Box::pin(tray.clone().ask(question))
                    },
                ) as Confirm
            });
            if let Err(error) = self.ble.set_pairing(confirm).await {
                warn!("could not switch pairing mode: {error}");
            }
            if on {
                let tray = self.clone();
                let timer = tokio::spawn(async move {
                    sleep(PAIRING).await;
                    // Taken first so that switching off does not abort this very task.
                    guard(&tray.pairing_timer).take();
                    tray.set_pairing(false).await;
                });
                *guard(&self.pairing_timer) = Some(timer.abort_handle());
            }
            self.refresh().await;
        })
    }

    /// Show the pairing question as a notification with Accept and Reject buttons.
    async fn ask(self: Arc<Self>, question: String) -> bool {
        let Some(conn) = guard(&self.conn).clone() else {
            return false;
        };
        let hints = HashMap::from([("urgency", Value::U8(2))]);
        let actions = ["accept", "Accept", "reject", "Reject"];
        let body = (
            "bluff",
            0u32,
            "",
            "Pair with bluff?",
            question.as_str(),
            &actions[..],
            hints,
            0i32,
        );
        let path = "/org/freedesktop/Notifications";
        let reply = conn
            .call_method(
                Some(NOTIFICATIONS),
                path,
                Some(NOTIFICATIONS),
                "Notify",
                &body,
            )
            .await;
        let Ok(id) = reply.and_then(|reply| reply.body().deserialize::<u32>()) else {
            warn!("could not show the pairing question");
            return false;
        };
        let (answer, answered) = oneshot::channel();
        guard(&self.prompts).insert(id, answer);
        let accepted = matches!(timeout(PROMPT, answered).await, Ok(Ok(true)));
        guard(&self.prompts).remove(&id);
        if accepted {
            tokio::spawn(self.clone().set_pairing(false));
        }
        accepted
    }
}

async fn register(conn: &Connection, name: &str) {
    let registered = conn
        .call_method(
            Some(WATCHER),
            "/StatusNotifierWatcher",
            Some(WATCHER),
            "RegisterStatusNotifierItem",
            &(name,),
        )
        .await;
    match registered {
        Ok(_) => info!("tray icon registered"),
        Err(error) => warn!("no tray to show the icon in yet ({error}); waiting for one"),
    }
}

struct Item {
    tray: Arc<Tray>,
}

#[interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    #[zbus(property)]
    fn category(&self) -> &str {
        "Hardware"
    }

    #[zbus(property)]
    fn id(&self) -> &str {
        "bluff"
    }

    #[zbus(property)]
    fn title(&self) -> &str {
        "bluff"
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        status(&lock(&self.tray.shared))
    }

    #[zbus(property)]
    fn icon_name(&self) -> &str {
        ""
    }

    #[zbus(property)]
    fn icon_pixmap(&self) -> Pixmaps {
        icon_pixmaps(icon(&lock(&self.tray.shared)))
    }

    #[zbus(property)]
    fn tool_tip(&self) -> (String, Pixmaps, String, String) {
        let bridge = lock(&self.tray.shared);
        (
            String::new(),
            icon_pixmaps(icon(&bridge)),
            "bluff".into(),
            summary(icon(&bridge)).into(),
        )
    }

    #[zbus(property)]
    fn menu(&self) -> ObjectPath<'_> {
        ObjectPath::from_static_str_unchecked(MENU_PATH)
    }

    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }

    fn activate(&self, _x: i32, _y: i32) {
        self.tray.clicked(TOGGLE);
    }

    fn secondary_activate(&self, _x: i32, _y: i32) {}

    fn context_menu(&self, _x: i32, _y: i32) {}

    fn scroll(&self, _delta: i32, _orientation: String) {}

    #[zbus(signal)]
    async fn new_icon(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_tool_tip(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_status(emitter: &SignalEmitter<'_>, status: &str) -> zbus::Result<()>;
}

/// The com.canonical.dbusmenu protocol for a flat menu.
struct Menu {
    tray: Arc<Tray>,
}

impl Menu {
    fn items(&self) -> Vec<(i32, Props)> {
        menu(&lock(&self.tray.shared))
    }
}

#[interface(name = "com.canonical.dbusmenu")]
impl Menu {
    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }

    #[zbus(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }

    fn get_layout(
        &self,
        _parent: i32,
        _depth: i32,
        _names: Vec<String>,
    ) -> (u32, (i32, Props, Vec<OwnedValue>)) {
        let children = self
            .items()
            .into_iter()
            .map(|(item, props)| {
                OwnedValue::try_from(Value::from((item, props, Vec::<OwnedValue>::new())))
                    .expect("no fds")
            })
            .collect();
        let root = props([("children-display", "submenu".into())]);
        (
            self.tray.revision.load(Ordering::Relaxed),
            (0, root, children),
        )
    }

    fn get_group_properties(&self, ids: Vec<i32>, _names: Vec<String>) -> Vec<(i32, Props)> {
        self.items()
            .into_iter()
            .filter(|(item, _)| ids.is_empty() || ids.contains(item))
            .collect()
    }

    fn get_property(&self, id: i32, name: String) -> zbus::fdo::Result<OwnedValue> {
        let items = self.items();
        let props = items
            .into_iter()
            .find(|(item, _)| *item == id)
            .map(|(_, props)| props)
            .unwrap_or_default();
        props
            .get(&name)
            .and_then(|value| value.try_clone().ok())
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("no property {name} on {id}")))
    }

    fn event(&self, id: i32, event: String, _data: OwnedValue, _timestamp: u32) {
        if event == "clicked" {
            self.tray.clicked(id);
        }
    }

    fn event_group(&self, events: Vec<(i32, String, OwnedValue, u32)>) -> Vec<i32> {
        for (id, event, _, _) in events {
            if event == "clicked" {
                self.tray.clicked(id);
            }
        }
        Vec::new()
    }

    fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    fn about_to_show_group(&self, _ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        (Vec::new(), Vec::new())
    }

    #[zbus(signal)]
    async fn layout_updated(
        emitter: &SignalEmitter<'_>,
        revision: u32,
        parent: i32,
    ) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::linux::hosts::Hosts;
    use crate::linux::hosts::tests::{props as device, temp_store};

    fn bridge(test: &str) -> Bridge<Peripheral> {
        let store = temp_store(test);
        std::fs::write(&store, "A\nB\n").unwrap();
        let (peripheral, _) = Peripheral::new(Hosts::load(store));
        Bridge::new(peripheral, HashSet::from([1]), 10.0, 2.0)
    }

    fn item(bridge: &Bridge<Peripheral>, id: i32) -> Props {
        menu(bridge)
            .into_iter()
            .find(|(item, _)| *item == id)
            .unwrap()
            .1
    }

    #[tokio::test]
    async fn icon_and_menu_follow_connection() {
        let mut bridge = bridge("icon");
        assert_eq!(icon(&bridge), Icon::Disconnected);
        assert_eq!(item(&bridge, TOGGLE)["enabled"], OwnedValue::from(false));
        let _theirs = bridge
            .peripheral
            .acquire(crate::linux::ble::Which::Keyboard, "/a")
            .unwrap();
        assert_eq!(icon(&bridge), Icon::Laptop);
        assert_eq!(item(&bridge, TOGGLE)["enabled"], OwnedValue::from(true));
    }

    #[test]
    fn hosts_show_as_radio_items() {
        let mut bridge = bridge("radio");
        let hosts = &mut bridge.peripheral.hosts;
        hosts.update(
            "/b",
            &device(&[
                ("Address", "B".into()),
                ("Paired", true.into()),
                ("Alias", "iPad".into()),
            ]),
        );
        hosts.update(
            "/a",
            &device(&[
                ("Address", "A".into()),
                ("Paired", true.into()),
                ("Alias", "Fold".into()),
                ("Connected", true.into()),
            ]),
        );
        hosts.chosen_address = "A".into();
        let first = item(&bridge, FIRST_HOST_ITEM);
        assert_eq!(
            first["label"],
            OwnedValue::try_from(Value::from("Fold")).unwrap()
        );
        assert_eq!(first["toggle-state"], OwnedValue::from(1i32));
        assert_eq!(
            item(&bridge, FIRST_HOST_ITEM + 1)["toggle-state"],
            OwnedValue::from(0i32)
        );
    }

    #[tokio::test]
    async fn connection_means_the_chosen_host_is_subscribed() {
        let mut bridge = bridge("subscribed");
        let hosts = &mut bridge.peripheral.hosts;
        hosts.update(
            "/a",
            &device(&[
                ("Address", "A".into()),
                ("Paired", true.into()),
                ("Connected", true.into()),
            ]),
        );
        hosts.update(
            "/b",
            &device(&[
                ("Address", "B".into()),
                ("Paired", true.into()),
                ("Connected", true.into()),
            ]),
        );
        hosts.chosen_address = "A".into();
        let _b = bridge
            .peripheral
            .acquire(crate::linux::ble::Which::Keyboard, "/b")
            .unwrap();
        assert_eq!(icon(&bridge), Icon::Disconnected);
        let _a = bridge
            .peripheral
            .acquire(crate::linux::ble::Which::Keyboard, "/a")
            .unwrap();
        assert_eq!(icon(&bridge), Icon::Laptop);
    }
}
