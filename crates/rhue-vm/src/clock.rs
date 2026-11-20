//! Where a VM gets the time. std's clocks by default, or whatever the host
//! hands [`Vm::set_clock`](crate::Vm::set_clock), since on
//! `wasm32-unknown-unknown` std's clocks panic.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

// std has clocks everywhere but plain wasm
#[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
mod std_clock {
    pub(super) use std::time::Instant;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    pub(super) fn wall_time() -> Option<Duration> {
        SystemTime::now().duration_since(UNIX_EPOCH).ok()
    }

    pub(super) fn instant() -> Option<Instant> {
        Some(Instant::now())
    }
}

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
mod std_clock {
    use std::time::Duration;

    #[derive(Clone, Copy)]
    pub(super) struct Instant;

    impl Instant {
        pub(super) fn elapsed(&self) -> Duration {
            Duration::ZERO
        }
    }

    pub(super) fn wall_time() -> Option<Duration> {
        None
    }

    pub(super) fn instant() -> Option<Instant> {
        None
    }
}

type HostClock = Rc<dyn Fn() -> Duration>;

enum Source {
    /// std's clocks, and when the VM was made (`None` where there aren't
    /// any, so making a VM on wasm doesn't panic)
    Std(Option<std_clock::Instant>),
    /// the host's, and its reading when it was set
    Host(HostClock, Duration),
}

pub(crate) struct Clock(Source);

impl Default for Clock {
    fn default() -> Self {
        Clock(Source::Std(std_clock::instant()))
    }
}

impl Clock {
    pub(crate) fn set(&mut self, clock: HostClock) {
        let start = clock();
        self.0 = Source::Host(clock, start);
    }

    pub(crate) fn wall_time(&self) -> Option<Duration> {
        match &self.0 {
            Source::Std(_) => std_clock::wall_time(),
            Source::Host(f, _) => Some(f()),
        }
    }

    /// Time since the VM was made, or since the host's clock was set.
    pub(crate) fn uptime(&self) -> Option<Duration> {
        match &self.0 {
            Source::Std(started) => started.map(|s| s.elapsed()),
            Source::Host(f, start) => Some(f().saturating_sub(*start)),
        }
    }

    pub(crate) fn uptime_fn(&self) -> Rc<dyn Fn() -> Option<Duration>> {
        match &self.0 {
            Source::Std(started) => {
                let started = *started;
                Rc::new(move || started.map(|s| s.elapsed()))
            }
            Source::Host(f, start) => {
                let (f, start) = (f.clone(), *start);
                // a host clock might step back (it's wall time), the task
                // scheduler needs it not to
                let last = Cell::new(Duration::ZERO);
                Rc::new(move || {
                    let now = f().saturating_sub(start).max(last.get());
                    last.set(now);
                    Some(now)
                })
            }
        }
    }
}
