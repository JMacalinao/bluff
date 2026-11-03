//! The devices bluff paired with, and which one of them gets bluff's input.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use log::{info, warn};
use zbus::zvariant::OwnedValue;

pub type Props = HashMap<String, OwnedValue>;

pub fn default_store() -> PathBuf {
    let state = env::var_os("XDG_STATE_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(&env::var_os("HOME").unwrap_or_default()).join(".local/state")
        });
    state.join("bluff/hosts")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Host {
    pub path: String,
    pub address: String,
    pub name: String,
    pub connected: bool,
}

/// What a device's PropertiesChanged signal means for bluff.
#[derive(Debug, PartialEq, Eq)]
pub enum Change {
    Ignored,
    /// A device the agent let pair finished pairing; fetch its properties and `adopt` it.
    Paired,
    /// A host changed; `connected` says whether it just connected.
    Host {
        connected: bool,
    },
}

/// Paired hosts, kept in sync with BlueZ, and the one chosen to get input.
///
/// Every host may stay connected; the peripheral sends input only to the chosen
/// one. Hosts are never blocked or disconnected: a phone whose reconnects keep
/// failing gives up, and BlueZ reconnects some hosts on its own anyway.
/// bluff's addresses live in `store` because the adapter's bond list also holds
/// headphones and other devices bluff must not touch; the chosen address lives
/// next to it.
pub struct Hosts {
    store: PathBuf,
    chosen_store: PathBuf,
    pub addresses: BTreeSet<String>,
    pub chosen_address: String,
    pub hosts: HashMap<String, Host>,
    accepted: HashSet<String>,
}

fn string(props: &Props, key: &str) -> Option<String> {
    props
        .get(key)
        .and_then(|value| String::try_from(value.try_clone().ok()?).ok())
}

fn boolean(props: &Props, key: &str) -> Option<bool> {
    props.get(key).and_then(|value| bool::try_from(value).ok())
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

impl Hosts {
    pub fn load(store: PathBuf) -> Self {
        let chosen_store = store.with_file_name("chosen");
        let addresses = fs::read_to_string(&store).unwrap_or_default();
        let chosen = fs::read_to_string(&chosen_store).unwrap_or_default();
        Self {
            addresses: addresses.split_whitespace().map(String::from).collect(),
            chosen_address: chosen.trim().to_string(),
            store,
            chosen_store,
            hosts: HashMap::new(),
            accepted: HashSet::new(),
        }
    }

    pub fn chosen(&self) -> Option<&Host> {
        self.hosts
            .values()
            .find(|host| host.address == self.chosen_address)
    }

    /// Hosts sorted the way the tray lists them.
    pub fn sorted(&self) -> Vec<&Host> {
        let mut hosts: Vec<&Host> = self.hosts.values().collect();
        hosts.sort_by(|a, b| (&a.name, &a.path).cmp(&(&b.name, &b.path)));
        hosts
    }

    /// Remember a device the agent let pair; it becomes a host once BlueZ reports it paired.
    pub fn accepted(&mut self, path: &str) {
        self.accepted.insert(path.to_string());
    }

    /// Take in a device from GetManagedObjects or a signal; only stored addresses become hosts.
    pub fn update(&mut self, path: &str, props: &Props) {
        if !self.hosts.contains_key(path) {
            let address = string(props, "Address").unwrap_or_default();
            // BlueZ can keep a second, unpaired object for a host it first saw at a private
            // address. It shares the address, never subscribes, and disconnecting it drops the host.
            if !self.addresses.contains(&address) || boolean(props, "Paired") != Some(true) {
                return;
            }
            let host = Host {
                path: path.to_string(),
                address,
                name: String::new(),
                connected: false,
            };
            self.hosts.insert(path.to_string(), host);
        }
        let host = self.hosts.get_mut(path).expect("inserted above");
        if let Some(name) = string(props, "Alias") {
            host.name = name;
        }
        if let Some(connected) = boolean(props, "Connected") {
            host.connected = connected;
        }
    }

    pub fn properties_changed(&mut self, path: &str, changed: &Props) -> Change {
        if self.accepted.contains(path) && boolean(changed, "Paired") == Some(true) {
            self.accepted.remove(path);
            return Change::Paired;
        }
        let Some(was_connected) = self.hosts.get(path).map(|host| host.connected) else {
            return Change::Ignored;
        };
        self.update(path, changed);
        Change::Host {
            connected: self.hosts[path].connected && !was_connected,
        }
    }

    /// Store a newly paired device's address and choose it.
    pub fn adopt(&mut self, path: &str, props: &Props) {
        self.addresses
            .insert(string(props, "Address").unwrap_or_default());
        let text: String = self
            .addresses
            .iter()
            .map(|address| format!("{address}\n"))
            .collect();
        write(&self.store, &text);
        self.update(path, props);
        info!("new host {:?}", self.hosts[path].name);
        self.choose(path);
    }

    /// Send input only to the host at `path` from now on.
    pub fn choose(&mut self, path: &str) {
        let Some(host) = self.hosts.get(path) else {
            return;
        };
        self.chosen_address = host.address.clone();
        write(&self.chosen_store, &format!("{}\n", host.address));
        info!("input is for {:?} from now on", host.name);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use zbus::zvariant::Value;

    use super::*;

    pub fn props(pairs: &[(&str, Value<'static>)]) -> Props {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), OwnedValue::try_from(v.clone()).unwrap()))
            .collect()
    }

    pub fn temp_store(test: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("bluff-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("hosts")
    }

    fn hosts(test: &str, addresses: &str) -> Hosts {
        let store = temp_store(test);
        fs::write(&store, addresses).unwrap();
        Hosts::load(store)
    }

    #[test]
    fn only_stored_addresses_become_hosts() {
        let mut hosts = hosts("stored", "AA\n");
        hosts.update(
            "/d/a",
            &props(&[
                ("Address", "AA".into()),
                ("Paired", true.into()),
                ("Alias", "iPad".into()),
            ]),
        );
        hosts.update(
            "/d/b",
            &props(&[("Address", "BB".into()), ("Alias", "Headphones".into())]),
        );
        assert_eq!(hosts.hosts.keys().collect::<Vec<_>>(), ["/d/a"]);
        assert_eq!(hosts.hosts["/d/a"].name, "iPad");
    }

    #[test]
    fn an_unpaired_object_with_a_host_address_is_no_host() {
        let mut hosts = hosts("unpaired", "AA\n");
        hosts.update(
            "/d/a",
            &props(&[("Address", "AA".into()), ("Paired", true.into())]),
        );
        hosts.update(
            "/d/rpa",
            &props(&[("Address", "AA".into()), ("Paired", false.into())]),
        );
        assert_eq!(hosts.hosts.keys().collect::<Vec<_>>(), ["/d/a"]);
    }

    #[test]
    fn choose_stores_the_choice() {
        let mut hosts = hosts("choose", "AA\n");
        hosts.update(
            "/d/a",
            &props(&[("Address", "AA".into()), ("Paired", true.into())]),
        );
        hosts.choose("/d/a");
        assert_eq!(hosts.chosen().unwrap().path, "/d/a");
        assert_eq!(Hosts::load(hosts.store.clone()).chosen_address, "AA");
    }

    #[test]
    fn reconnect_is_reported() {
        let mut hosts = hosts("reconnect", "AA\n");
        hosts.update(
            "/d/a",
            &props(&[
                ("Address", "AA".into()),
                ("Paired", true.into()),
                ("Connected", false.into()),
            ]),
        );
        let change = hosts.properties_changed("/d/a", &props(&[("Connected", true.into())]));
        assert_eq!(change, Change::Host { connected: true });
        let change = hosts.properties_changed("/d/a", &props(&[("Alias", "iPad".into())]));
        assert_eq!(change, Change::Host { connected: false });
    }

    #[test]
    fn signals_update_known_hosts_only() {
        let mut hosts = hosts("signals", "AA\n");
        let change = hosts.properties_changed(
            "/d/b",
            &props(&[("Address", "AA".into()), ("Connected", true.into())]),
        );
        assert_eq!(change, Change::Ignored);
        assert!(hosts.hosts.is_empty());
    }

    #[test]
    fn new_pairing_is_stored_and_chosen() {
        let mut hosts = hosts("pairing", "");
        hosts.accepted("/d/c");
        assert_eq!(
            hosts.properties_changed("/d/c", &props(&[("Paired", true.into())])),
            Change::Paired
        );
        hosts.adopt(
            "/d/c",
            &props(&[
                ("Address", "CC".into()),
                ("Paired", true.into()),
                ("Alias", "Fold".into()),
                ("Connected", true.into()),
            ]),
        );
        assert_eq!(hosts.chosen().unwrap().name, "Fold");
        let stored = Hosts::load(hosts.store.clone());
        assert_eq!(
            (
                stored.addresses.into_iter().collect::<Vec<_>>(),
                stored.chosen_address
            ),
            (vec!["CC".into()], "CC".into())
        );
    }
}
