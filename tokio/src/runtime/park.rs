//! Parks the runtime.
//!
//! A combination of the various resource driver park handles.

use crate::loom::sync::Arc;
use crate::loom::sync::atomic::AtomicBool;
use crate::runtime::io;

use std::sync::atomic::Ordering::SeqCst;

pub(crate) struct Parker {
    inner: Arc<Inner>,
}

pub(crate) struct Unparker {
    inner: Arc<Inner>,
}

struct Inner {
    /// Avoids entering the park if possible
    state: AtomicUsize,

    /// Used to coordinate access to the driver / condvar
    ///
    /// The state is `true` when the thread is parked in the driver.
    mutex: Mutex<bool>,

    /// Condvar to block on if the driver is unavailable.
    condvar: Condvar,

    /// Resource (I/O, time, ...) driver
    driver: Arc<Driver>,
}

const IDLE: usize = 0;
const NOTIFY: usize = 1;
const SLEEP_CONDVAR: usize = 2;
const SLEEP_DRIVER: usize = 3;

/// Synchronizes access to the shared resource drivers.
struct Driver {
    /// Coordinates access to the driver
    lock: AtomicUsize,

    /// Shared driver.
    driver: io::Driver,

    /// Unpark handle
    handle: io::Handle,
}

impl Inner {
    /// Park the current thread for at most `dur`.
    fn park(&self, timeout: Option<Duration>) -> Result<(), ParkError> {
        // If currently notified, then we skip sleeping. This is checked outside
        // of the lock to avoid acquiring a mutex if not necessary.
        match self.state.compare_and_swap(NOTIFY, IDLE, SeqCst) {
            NOTIFY => return Ok(()),
            IDLE => {}
            _ => unreachable!(),
        }

        self.park_condvar()
    }

    fn park_condvar(&self, timeout: Option<Duration>) -> Result<(), ParkError> {
        // The state is currently idle, so obtain the lock and then try to
        // transition to a sleeping state.
        let mut m = self.mutex.lock().unwrap();

        // Transition to sleeping
        match self.state.compare_and_swap(IDLE, SLEEP, SeqCst) {
            NOTIFY => {
                // Notified before we could sleep, consume the notification and
                // exit
                self.state.store(IDLE, SeqCst);
                return Ok(());
            }
            IDLE => {}
            _ => unreachable!(),
        }

        m = match timeout {
            Some(timeout) => self.condvar.wait_timeout(m, timeout).unwrap().0,
            None => self.condvar.wait(m).unwrap(),
        };

        // Transition back to idle. If the state has transitioned to `NOTIFY`,
        // this will consume that notification
        self.state.store(IDLE, SeqCst);

        // Explicitly drop the mutex guard. There is no real point in doing it
        // except that I find it helpful to make it explicit where we want the
        // mutex to unlock.
        drop(m);

        Ok(())
    }

    fn unpark(&self) {
        // First, try transitioning from IDLE -> NOTIFY, this does not require a
        // lock.
        match self.state.compare_and_swap(IDLE, NOTIFY, SeqCst) {
            IDLE | NOTIFY => return,
            SLEEP => {}
            _ => unreachable!(),
        }

        // The other half is sleeping, this requires a lock
        let _m = self.mutex.lock().unwrap();

        // Transition to NOTIFY
        match self.state.swap(NOTIFY, SeqCst) {
            SLEEP => {}
            NOTIFY => return,
            IDLE => return,
            _ => unreachable!(),
        }

        // Wakeup the sleeper
        self.condvar.notify_one();
    }

    fn unpark_condvar(&self) {
    }

    fn unpark_driver(&self) {
    }
}
