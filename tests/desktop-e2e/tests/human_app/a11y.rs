// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The shipped client's window as a screen reader is handed it: its tree
//! read over AT-SPI from the platform adapter, and its controls pressed
//! through their accessible actions -- the way a person who cannot use a
//! pointer reaches them, and the only way these cases press anything.
//!
//! Needs the AT-SPI bus a screen reader would use, with accessibility
//! switched on and the registry answering, which `tools/ci/with_display.sh`
//! stands up beside the display. Required, not skipped: without it the
//! adapter exports nothing, and a case that found nothing because nobody
//! was listening would prove nothing.

use std::collections::HashMap;
use std::time::Duration;

use atspi::ObjectRefOwned;
use atspi::proxy::accessible::{AccessibleProxy, ObjectRefExt as _};
use atspi::proxy::action::ActionProxy;

use crate::common::PATIENCE;

/// The registry every application on the bus is listed under.
const REGISTRY: &str = "org.a11y.atspi.Registry";

/// One element of the tree, as read at one moment.
#[derive(Debug, Clone)]
pub(crate) struct Element {
    pub(crate) role: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) attributes: HashMap<String, String>,
    pub(crate) actions: Vec<String>,
    at: ObjectRefOwned,
}

/// The accessibility bus, connected.
pub(crate) struct Bus {
    conn: zbus::Connection,
}

impl Bus {
    /// The bus a screen reader would use: `AT_SPI_BUS_ADDRESS` when it is
    /// set, otherwise the address the session bus's `org.a11y.Bus` gives,
    /// which is how a screen reader finds it.
    pub(crate) async fn connect() -> Self {
        let address = match std::env::var("AT_SPI_BUS_ADDRESS") {
            Ok(address) => address,
            Err(_) => Self::from_session().await,
        };
        let conn = zbus::connection::Builder::address(address.as_str())
            .expect("an AT-SPI bus address")
            .build()
            .await
            .expect("the AT-SPI bus answers");
        Self { conn }
    }

    async fn from_session() -> String {
        let session = zbus::Connection::session().await.unwrap_or_else(|e| {
            panic!(
                "no session bus ({e}): the accessibility cases read the window over \
                 AT-SPI (run under tools/ci/with_display.sh)"
            )
        });
        let reply = session
            .call_method(
                Some("org.a11y.Bus"),
                "/org/a11y/bus",
                Some("org.a11y.Bus"),
                "GetAddress",
                &(),
            )
            .await
            .unwrap_or_else(|e| panic!("the session bus has no AT-SPI bus: {e}"));
        reply.body().deserialize::<String>().expect("an address")
    }

    /// The window of the client running as `pid`, once the registry lists
    /// it: several clients share the bus when cases run in parallel, so it
    /// is found by its process, never by its name.
    pub(crate) async fn window_of(&self, pid: u32, log: impl Fn() -> String) -> Window<'_> {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            if let Some(app) = self.application(pid).await {
                let window = Window { bus: self, app };
                if !window.read().await.is_empty() {
                    return window;
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the client's window (pid {pid}) never reached the AT-SPI registry, \
                 which lists {:?}: {}",
                self.listed().await,
                log()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// What the registry lists: each application's name and process.
    async fn listed(&self) -> Vec<(String, Option<u32>)> {
        let Ok(root) = self.registry().await else {
            return Vec::new();
        };
        let Ok(dbus) = zbus::fdo::DBusProxy::new(&self.conn).await else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for app in root.get_children().await.unwrap_or_default() {
            let name = match app.as_accessible_proxy(&self.conn).await {
                Ok(proxy) => proxy.name().await.unwrap_or_default(),
                Err(e) => format!("({e})"),
            };
            let pid = match app.name() {
                Some(owner) => dbus
                    .get_connection_unix_process_id(zbus::names::BusName::from(owner.clone()))
                    .await
                    .ok(),
                None => None,
            };
            out.push((name, pid));
        }
        out
    }

    async fn registry(&self) -> zbus::Result<AccessibleProxy<'_>> {
        AccessibleProxy::builder(&self.conn)
            .destination(REGISTRY)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
    }

    async fn application(&self, pid: u32) -> Option<ObjectRefOwned> {
        let root = self.registry().await.ok()?;
        let dbus = zbus::fdo::DBusProxy::new(&self.conn).await.ok()?;
        for app in root.get_children().await.ok()? {
            let Some(name) = app.name() else { continue };
            let owner = zbus::names::BusName::from(name.clone());
            if dbus.get_connection_unix_process_id(owner).await.ok() == Some(pid) {
                return Some(app);
            }
        }
        None
    }
}

/// An AT-SPI object event's body, `siiva{sv}`: its kind, two details,
/// its data -- an announcement's text -- and properties.
type Event = (
    String,
    i32,
    i32,
    zbus::zvariant::OwnedValue,
    HashMap<String, zbus::zvariant::OwnedValue>,
);

/// What a screen reader is told to say: the `Announcement` events one
/// client's live regions raise, heard on the bus as they are sent.
pub(crate) struct Announcements {
    stream: zbus::MessageStream,
    from: String,
    heard: Vec<String>,
}

impl Announcements {
    /// Wait until `text` is announced, and return everything heard so far.
    pub(crate) async fn until(&mut self, text: &str, log: impl Fn() -> String) -> Vec<String> {
        use futures_util::StreamExt as _;
        let deadline = tokio::time::Instant::now() + PATIENCE;
        while !self.heard.iter().any(|h| h == text) {
            let next = tokio::time::timeout_at(deadline, self.stream.next()).await;
            let Ok(Some(Ok(message))) = next else {
                panic!(
                    "never announced: {text:?}; heard {:?}\n{}",
                    self.heard,
                    log()
                );
            };
            let header = message.header();
            if header.sender().map(zbus::names::UniqueName::as_str) != Some(self.from.as_str()) {
                continue;
            }
            if let Ok((_, _, _, said, _)) = message.body().deserialize::<Event>()
                && let Ok(said) = String::try_from(said)
            {
                self.heard.push(said);
            }
        }
        self.heard.clone()
    }
}

/// One client's window on the bus.
pub(crate) struct Window<'b> {
    bus: &'b Bus,
    app: ObjectRefOwned,
}

impl Window<'_> {
    /// Start hearing this client's announcements: only those sent from
    /// now on.
    pub(crate) async fn announcements(&self) -> Announcements {
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface("org.a11y.atspi.Event.Object")
            .expect("an interface")
            .member("Announcement")
            .expect("a member")
            .build();
        let stream = zbus::MessageStream::for_match_rule(rule, &self.bus.conn, None)
            .await
            .expect("the bus takes the match");
        Announcements {
            stream,
            from: self.app.name().expect("a bus name").to_string(),
            heard: Vec::new(),
        }
    }

    /// Every element under the application, depth first. An element that
    /// goes away while it is read is left out: the tree is live.
    pub(crate) async fn read(&self) -> Vec<Element> {
        let mut out = Vec::new();
        let mut stack = vec![self.app.clone()];
        while let Some(at) = stack.pop() {
            let Ok(node) = at.as_accessible_proxy(&self.bus.conn).await else {
                continue;
            };
            let Ok(children) = node.get_children().await else {
                continue;
            };
            if let Some(element) = self.element(&node, at.clone()).await {
                out.push(element);
            }
            stack.extend(children.into_iter().rev());
        }
        out
    }

    async fn element(&self, node: &AccessibleProxy<'_>, at: ObjectRefOwned) -> Option<Element> {
        let action = ActionProxy::builder(&self.bus.conn)
            .destination(at.name()?.clone())
            .ok()?
            .path(at.path().clone())
            .ok()?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
            .ok()?;
        // GetActions, the one call: the adapter does not serve the
        // NActions property, so asking it reads every element as having
        // none.
        let actions = action
            .get_actions()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|a| a.name)
            .collect();
        Some(Element {
            // The role's name as a screen reader computes it: the adapter
            // answers GetRole and not GetRoleName.
            role: node.get_role().await.ok()?.name().to_owned(),
            name: node.name().await.ok()?,
            description: node.description().await.unwrap_or_default(),
            attributes: node.get_attributes().await.unwrap_or_default(),
            actions,
            at,
        })
    }

    /// Wait until the tree holds an element `found` accepts, and return it.
    pub(crate) async fn until(
        &self,
        what: &str,
        found: impl Fn(&Element) -> bool,
        log: impl Fn() -> String,
    ) -> Element {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            let tree = self.read().await;
            if let Some(element) = tree.iter().find(|e| found(e)) {
                return element.clone();
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "never in the tree: {what}\n{}\n{}",
                describe(&tree),
                log()
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Press `element` through its named accessible action, as a screen
    /// reader's activate command does.
    pub(crate) async fn activate(&self, element: &Element, action: &str) {
        let index = element
            .actions
            .iter()
            .position(|a| a == action)
            .unwrap_or_else(|| panic!("{element:?} offers no {action:?} action"));
        let proxy = ActionProxy::builder(&self.bus.conn)
            .destination(element.at.name().expect("a bus name").clone())
            .expect("a destination")
            .path(element.at.path().clone())
            .expect("a path")
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
            .expect("an action proxy");
        let index = i32::try_from(index).expect("a small index");
        assert!(
            proxy.do_action(index).await.expect("the action is sent"),
            "{element:?} accepted {action:?}"
        );
    }
}

/// The tree, one element a line, for a failure's message.
pub(crate) fn describe(tree: &[Element]) -> String {
    tree.iter()
        .map(|e| {
            format!(
                "{} {:?} desc={:?} actions={:?} attrs={:?}",
                e.role, e.name, e.description, e.actions, e.attributes
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
