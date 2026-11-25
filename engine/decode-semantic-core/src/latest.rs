//! The last result of a slow computation, served at once while the next one runs on a thread of its own: the
//! search servers discover their projects this way, since walking thousands of directories cost every search tens
//! of milliseconds warm and over a second cold.

use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

#[derive(Debug)]
struct State<T> {
    value: Option<Arc<T>>,
    refreshing: Option<JoinHandle<()>>,
}

/// The last computed value and the refresh under way.
#[derive(Debug)]
pub struct Latest<T> {
    state: Arc<Mutex<State<T>>>,
}

impl<T: Send + Sync + 'static> Default for Latest<T> {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                value: None,
                refreshing: None,
            })),
        }
    }
}

impl<T: Send + Sync + 'static> Latest<T> {
    /// The last value, with a refresh started in the background unless one is running; the first call computes it
    /// in place. A change shows from the call after the one that started its refresh.
    pub fn get(&self, compute: impl FnOnce() -> T + Send + 'static) -> Arc<T> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(value) = state.value.clone() else {
            let value = Arc::new(compute());
            state.value = Some(Arc::clone(&value));
            return value;
        };
        if state.refreshing.as_ref().is_none_or(JoinHandle::is_finished) {
            let shared = Arc::clone(&self.state);
            state.refreshing = Some(std::thread::spawn(move || {
                let value = Arc::new(compute());
                shared.lock().unwrap_or_else(PoisonError::into_inner).value = Some(value);
            }));
        }
        value
    }

    /// A value computed now, for a caller the last one cannot serve.
    pub fn now(&self, compute: impl FnOnce() -> T) -> Arc<T> {
        let value = Arc::new(compute());
        self.state.lock().unwrap_or_else(PoisonError::into_inner).value = Some(Arc::clone(&value));
        value
    }

    /// Blocks until the background refresh, if any, has stored its value.
    pub fn wait_refresh(&self) {
        let refreshing = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .refreshing
            .take();
        if let Some(handle) = refreshing {
            drop(handle.join());
        }
    }
}
