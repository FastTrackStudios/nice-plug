//! Hot reload support via dioxus-devtools.

use blitz_dom::Document as _;
use crossbeam::channel::{unbounded, Receiver, Sender};
use dioxus_devtools::DevserverMsg;
use dioxus_native::DioxusDocument;

/// State for managing hot reload connections.
pub struct HotReloadState {
    receiver: Receiver<DevserverMsg>,
    sender: Sender<DevserverMsg>,
}

impl HotReloadState {
    /// Create a new hot reload state.
    pub fn new() -> Self {
        let (sender, receiver) = unbounded();
        Self { receiver, sender }
    }

    /// Connect to the dioxus devtools server.
    ///
    /// This should be called once when the editor is opened.
    pub fn connect(&self) {
        let sender = self.sender.clone();
        dioxus_devtools::connect(move |msg| {
            let _ = sender.send(msg);
        });
    }

    /// Process any pending hot reload messages.
    ///
    /// This should be called on each frame to apply hot reload updates.
    pub fn process_messages(&self, doc: &mut DioxusDocument) {
        while let Ok(msg) = self.receiver.try_recv() {
            match msg {
                DevserverMsg::HotReload(hotreload_msg) => {
                    // Apply changes to the virtual DOM
                    dioxus_devtools::apply_changes(&doc.vdom, &hotreload_msg);

                    // Reload any changed assets
                    for asset_path in &hotreload_msg.assets {
                        if let Some(url) = asset_path.to_str() {
                            doc.inner_mut().reload_resource_by_href(url);
                        }
                    }
                }
                DevserverMsg::FullReloadStart => {
                    // A full reload was requested - we could trigger a full rebuild here
                    // For now, we just continue as the devserver will handle it
                }
                DevserverMsg::FullReloadFailed => {
                    // Full reload failed - log but continue
                    eprintln!("Hot reload: Full reload failed");
                }
                _ => {
                    // Ignore other messages
                }
            }
        }
    }
}

impl Default for HotReloadState {
    fn default() -> Self {
        Self::new()
    }
}
