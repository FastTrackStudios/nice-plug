//! Resource loading for the editor's document.
//!
//! Blitz asks a `NetProvider` for every resource a document references —
//! `<img src>` included — and the default one answers nothing at all. So a
//! plugin editor renders no images until something is installed here, which
//! is why this module exists.
//!
//! # Why this provider does not do networking
//!
//! It serves `data:` and `file:` only, and refuses everything else. That is a
//! deliberate limit, not an unfinished one:
//!
//! - **A plugin editor has no business making network requests.** It runs
//!   inside someone else's DAW, on their machine, often in a studio that has
//!   opinions about what phones home. A GUI framework that quietly fetches
//!   whatever a document mentions is a capability the host never agreed to.
//! - **It needs no async runtime.** `blitz-net` spawns onto tokio, and an
//!   editor window is driven by the host's UI thread with no runtime in
//!   scope. Serving local schemes is synchronous, so there is nothing to
//!   spawn and nothing to fail at runtime in a way the developer only
//!   discovers inside a DAW.
//! - **Application data already has a path in.** An app that wants remote
//!   content fetches it through its own transport and hands the bytes to the
//!   document as a `data:` URI, which is what it must do anyway to stay
//!   inside its own permissions and error handling.
//!
//! An application that genuinely wants HTTP can still install its own
//! provider on the document; this is only what you get for free.

use std::sync::Arc;

use blitz_traits::net::{Bytes, NetHandler, NetProvider, NetWaker, Request};

/// Serves `data:` and `file:` URLs; refuses every other scheme.
pub struct LocalAssets {
    /// Woken after each resource resolves, so the shell redraws with it. The
    /// document itself is not repainted by the handler — without this the
    /// image lands in the DOM and is not seen until the next unrelated frame.
    waker: Option<Arc<dyn NetWaker>>,
}

impl LocalAssets {
    /// A provider that wakes `waker` when a resource resolves.
    pub fn new(waker: Option<Arc<dyn NetWaker>>) -> Self {
        Self { waker }
    }

    /// As an `Arc`, ready for `DocumentConfig::net_provider`.
    pub fn shared(waker: Option<Arc<dyn NetWaker>>) -> Arc<dyn NetProvider> {
        Arc::new(Self::new(waker))
    }
}

/// A provider that flags `needs_redraw` when a resource lands.
///
/// The editor paints on demand, so a document that resolves an image between
/// frames would otherwise sit there holding it until something unrelated
/// caused a repaint — an image that appears when you wiggle the mouse.
pub fn redraw_provider(
    needs_redraw: Arc<std::sync::atomic::AtomicBool>,
) -> Arc<dyn NetProvider> {
    LocalAssets::shared(Some(Arc::new(move |_doc_id: usize| {
        needs_redraw.store(true, std::sync::atomic::Ordering::Relaxed);
    })))
}

impl NetProvider for LocalAssets {
    fn fetch(&self, doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        let url = request.url;
        let bytes = match url.scheme() {
            "data" => match data_url::DataUrl::process(url.as_str()) {
                Ok(data) => data.decode_to_vec().ok().map(|(body, _)| Bytes::from(body)),
                Err(_) => None,
            },
            "file" => url
                .to_file_path()
                .ok()
                .and_then(|path| std::fs::read(path).ok())
                .map(Bytes::from),
            // Anything else is a request this provider will not make. Say so
            // once: a silently missing image is a long afternoon.
            other => {
                nice_plug_core::nice_log!(
                    "nice-plug-dioxus: refusing to fetch a `{}:` resource — the \
                     editor's provider serves data: and file: only",
                    other
                );
                None
            }
        };

        let Some(bytes) = bytes else {
            // Dropping the handler is how a failed fetch is reported; the
            // document leaves the resource unresolved.
            return;
        };

        handler.bytes(url.to_string(), bytes);
        if let Some(waker) = &self.waker {
            waker.wake(doc_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Default)]
    struct Captured {
        bytes: std::sync::Mutex<Option<Bytes>>,
    }

    struct Handler(Arc<Captured>);

    impl NetHandler for Handler {
        fn bytes(self: Box<Self>, _url: String, bytes: Bytes) {
            *self.0.bytes.lock().unwrap() = Some(bytes);
        }
    }

    fn fetch(provider: &LocalAssets, url: &str) -> Option<Bytes> {
        let captured = Arc::new(Captured::default());
        let request = Request::get(url.parse().unwrap());
        provider.fetch(0, request, Box::new(Handler(captured.clone())));
        let taken = captured.bytes.lock().unwrap().clone();
        taken
    }

    #[test]
    fn a_data_url_resolves_to_its_bytes() {
        let provider = LocalAssets::new(None);
        // "hi" as base64, the shape an app hands over for an image.
        let bytes = fetch(&provider, "data:image/png;base64,aGk=");
        assert_eq!(bytes.as_deref(), Some(&b"hi"[..]));
    }

    #[test]
    fn a_file_url_resolves_to_the_file() {
        let dir = std::env::temp_dir().join("nice-plug-net-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("asset.txt");
        std::fs::write(&path, b"on disk").unwrap();

        let provider = LocalAssets::new(None);
        let url = blitz_traits::net::Url::from_file_path(&path).unwrap();
        assert_eq!(fetch(&provider, url.as_str()).as_deref(), Some(&b"on disk"[..]));
    }

    /// The property this provider exists to have.
    #[test]
    fn http_is_refused() {
        let provider = LocalAssets::new(None);
        assert!(fetch(&provider, "https://example.com/tracker.png").is_none());
        assert!(fetch(&provider, "http://192.168.1.1/admin").is_none());
    }

    #[test]
    fn a_missing_file_resolves_to_nothing_rather_than_panicking() {
        let provider = LocalAssets::new(None);
        assert!(fetch(&provider, "file:///definitely/not/here.png").is_none());
    }

    #[test]
    fn the_shell_is_woken_when_a_resource_lands() {
        struct Counter(Arc<AtomicUsize>);
        impl NetWaker for Counter {
            fn wake(&self, _doc_id: usize) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let count = Arc::new(AtomicUsize::new(0));
        let provider = LocalAssets::new(Some(Arc::new(Counter(count.clone()))));

        fetch(&provider, "data:text/plain;base64,aGk=");
        assert_eq!(count.load(Ordering::Relaxed), 1);

        // A refused fetch resolves nothing, so there is nothing to redraw.
        fetch(&provider, "https://example.com/x.png");
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }
}
