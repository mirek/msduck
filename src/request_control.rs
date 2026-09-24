//! Request-owned native cancellation effects. The transport drives pulses until
//! the worker disarms; a single interrupt before native entry is insufficient.
use msduck_tds::request_lifecycle::RequestId;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Cancelled,
    Busy,
    Retired,
    Poisoned,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Armed,
    Retired,
}

struct State {
    phase: Phase,
    cancelled: bool,
}

/// Exactly one controller belongs to one admitted request on one connection.
/// Retire it before admitting the next request. Never reuse it for a successor.
pub struct Control {
    id: RequestId,
    state: Mutex<State>,
    interrupt: Box<dyn Fn() + Send + Sync>,
}

impl Control {
    pub fn new(id: RequestId, handle: Arc<duckdb::InterruptHandle>) -> Self {
        Self::with_interrupt(id, move || handle.interrupt())
    }

    fn with_interrupt(id: RequestId, interrupt: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            id,
            state: Mutex::new(State {
                phase: Phase::Idle,
                cancelled: false,
            }),
            interrupt: Box::new(interrupt),
        }
    }

    pub fn id(&self) -> RequestId {
        self.id
    }

    fn lock(&self) -> Result<MutexGuard<'_, State>, Error> {
        self.state.lock().map_err(|_| Error::Poisoned)
    }

    pub fn is_cancelled(&self) -> Result<bool, Error> {
        Ok(self.lock()?.cancelled)
    }

    /// Execute one scoped native operation. R (including any native error) is
    /// preserved exactly. Cancellation observed later does not rewrite its cause.
    pub fn run_native<R>(&self, work: impl FnOnce() -> R) -> Result<R, Error> {
        {
            let mut state = self.lock()?;
            match state.phase {
                Phase::Retired => return Err(Error::Retired),
                Phase::Armed => return Err(Error::Busy),
                Phase::Idle => {}
            }
            if state.cancelled {
                return Err(Error::Cancelled);
            }
            state.phase = Phase::Armed;
        }
        let guard = NativeCall(self);
        let result = work();
        // Disarm waits for any in-flight interrupt call before returning control
        // to cleanup SQL, even when work returned a native error.
        drop(guard);
        Ok(result)
    }

    /// Latch cancellation and attempt an immediate interrupt if armed. Returns
    /// whether an interrupt was issued, not whether native execution stopped.
    pub fn cancel(&self) -> Result<bool, Error> {
        let mut state = self.lock()?;
        if state.phase == Phase::Retired {
            return Ok(false);
        }
        state.cancelled = true;
        Ok(self.interrupt_armed(&state))
    }

    /// The root event loop/timer must keep servicing a cancelled armed call.
    /// This closes the race where the first interrupt preceded native entry.
    /// After disarm or retirement, stale pulses cannot touch the connection.
    pub fn pulse(&self) -> Result<bool, Error> {
        let state = self.lock()?;
        Ok(self.interrupt_armed(&state))
    }

    fn interrupt_armed(&self, state: &State) -> bool {
        if state.phase == Phase::Armed && state.cancelled {
            // Hold only the short control fence, never the native query itself.
            // NativeCall's disarm cannot complete until this effect returns.
            (self.interrupt)();
            true
        } else {
            false
        }
    }

    /// Called after execution and cleanup. Busy means the worker cannot yet be
    /// acknowledged as quiescent. Retirement permanently revokes this capability.
    pub fn retire(&self) -> Result<(), Error> {
        let mut state = self.lock()?;
        if state.phase == Phase::Armed {
            return Err(Error::Busy);
        }
        state.phase = Phase::Retired;
        Ok(())
    }
}

struct NativeCall<'a>(&'a Control);
impl Drop for NativeCall<'_> {
    fn drop(&mut self) {
        // Even an unwinding interrupt callback must leave no armed capability.
        // Poison remains visible to callers; recovery does not authorize reuse.
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.phase = Phase::Idle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use msduck_tds::request_lifecycle::{Event, Lifecycle};
    use std::{
        sync::{
            Barrier,
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        thread,
        time::{Duration, Instant},
    };

    fn id() -> RequestId {
        let mut lifecycle = Lifecycle::default();
        lifecycle.advance(Event::Admit).unwrap();
        lifecycle.active().unwrap()
    }

    #[test]
    fn cancellation_before_entry_rejects_work_and_stale_retired_pulses() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let c = Control::with_interrupt(id(), move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        assert!(!c.cancel().unwrap());
        assert_eq!(
            c.run_native(|| panic!("cancelled work ran")),
            Err::<(), _>(Error::Cancelled)
        );
        assert!(!c.pulse().unwrap());
        c.retire().unwrap();
        assert!(!c.cancel().unwrap());
        assert!(!c.pulse().unwrap());
        assert_eq!(c.run_native(|| ()), Err(Error::Retired));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn native_errors_remain_distinct_from_cancellation_and_busy_entry() {
        let c = Control::with_interrupt(id(), || {});
        let native = c
            .run_native(|| {
                assert_eq!(c.run_native(|| ()), Err(Error::Busy));
                assert_eq!(c.retire(), Err(Error::Busy));
                c.cancel().unwrap();
                Err::<(), _>("independent native failure")
            })
            .unwrap();
        assert_eq!(native, Err("independent native failure"));
        assert!(c.is_cancelled().unwrap());
        assert_eq!(c.run_native(|| ()), Err(Error::Cancelled));
        c.retire().unwrap();
    }

    #[test]
    fn disarm_waits_for_interrupt_effect_before_cleanup_can_begin() {
        let inside = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let a = inside.clone();
        let b = release.clone();
        let c = Arc::new(Control::with_interrupt(id(), move || {
            a.wait();
            b.wait();
        }));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (return_tx, return_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let worker_control = c.clone();
        let worker = thread::spawn(move || {
            worker_control
                .run_native(|| {
                    entered_tx.send(()).unwrap();
                    return_rx.recv().unwrap();
                })
                .unwrap();
            finished_tx.send(()).unwrap();
        });
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let cancel_control = c.clone();
        let cancel = thread::spawn(move || cancel_control.cancel().unwrap());
        inside.wait();
        return_tx.send(()).unwrap();
        assert!(finished_rx.recv_timeout(Duration::from_millis(30)).is_err());
        release.wait();
        assert!(cancel.join().unwrap());
        finished_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        worker.join().unwrap();
        c.retire().unwrap();
        assert!(!c.pulse().unwrap());
    }

    #[test]
    fn cancelled_native_entry_race_is_serviced_and_connection_remains_reusable() {
        let db = duckdb::Connection::open_in_memory().unwrap();
        db.execute_batch("SET threads=1").unwrap();
        let c = Arc::new(Control::new(id(), db.interrupt_handle()));
        let worker_control = c.clone();
        let release = Arc::new(Barrier::new(2));
        let worker_release = release.clone();
        let (armed_tx, armed_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let result=worker_control.run_native(||{
                armed_tx.send(()).unwrap();
                worker_release.wait();
                db.query_row("SELECT sum(CAST(a.i AS DOUBLE)*b.i) FROM range(1000000) a(i), range(1000000) b(i)",[],|r|r.get::<_,f64>(0))
            }).unwrap();
            done_tx.send(()).unwrap();
            (db, result)
        });
        // Deliberately cancel after arming but BEFORE native entry. The first
        // interrupt alone cannot establish that a query was interrupted.
        armed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(c.cancel().unwrap());
        release.wait();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            c.pulse().unwrap();
            if done_rx.recv_timeout(Duration::from_millis(5)).is_ok() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "native cancellation failed to quiesce"
            );
        }
        let (db, result) = worker.join().unwrap();
        let error = result.unwrap_err();
        assert!(
            error.to_string().to_ascii_lowercase().contains("interrupt"),
            "{error}"
        );
        assert_eq!(c.run_native(|| ()), Err(Error::Cancelled));
        c.retire().unwrap();
        // Old controls may still be held by a delayed timer after retirement.
        for _ in 0..10 {
            assert!(!c.cancel().unwrap());
            assert!(!c.pulse().unwrap());
        }
        assert_eq!(
            db.query_row("SELECT 42", [], |r| r.get::<_, i32>(0))
                .unwrap(),
            42
        );
        let next = Control::new(id(), db.interrupt_handle());
        assert_eq!(
            next.run_native(|| db.query_row("SELECT 43", [], |r| r.get::<_, i32>(0)))
                .unwrap()
                .unwrap(),
            43
        );
        next.retire().unwrap();
    }
    #[test]
    fn interrupted_transaction_cleanup_is_not_targeted_by_delayed_pulses() {
        use std::sync::atomic::AtomicBool;
        let db = duckdb::Connection::open_in_memory().unwrap();
        db.execute_batch("SET threads=1; CREATE TABLE cancellation_probe(n INTEGER)")
            .unwrap();
        let observer = db.try_clone().unwrap();
        db.execute_batch("BEGIN TRANSACTION; INSERT INTO cancellation_probe VALUES(1)")
            .unwrap();
        let control = Arc::new(Control::new(id(), db.interrupt_handle()));
        let worker_control = control.clone();
        let release = Arc::new(Barrier::new(2));
        let worker_release = release.clone();
        let (armed_tx, armed_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let result=worker_control.run_native(|| {
                armed_tx.send(()).unwrap();
                worker_release.wait();
                db.query_row("SELECT sum(CAST(a.i AS DOUBLE)*b.i) FROM range(1000000) a(i), range(1000000) b(i)",[],|r|r.get::<_,f64>(0))
            }).unwrap();
            done_tx.send(()).unwrap();
            (db, result)
        });
        armed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(control.cancel().unwrap());
        release.wait();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            control.pulse().unwrap();
            if done_rx.recv_timeout(Duration::from_millis(5)).is_ok() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "transaction query failed to quiesce"
            );
        }
        let (db, result) = worker.join().unwrap();
        let error = result.unwrap_err();
        assert!(
            error.to_string().to_ascii_lowercase().contains("interrupt"),
            "{error}"
        );
        let retained = db.query_row("SELECT count(*) FROM cancellation_probe", [], |r| {
            r.get::<_, i64>(0)
        });
        eprintln!("Interrupted explicit transaction read: {retained:?}");
        assert_eq!(
            observer
                .query_row("SELECT count(*) FROM cancellation_probe", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let stopped = Arc::new(AtomicBool::new(false));
        let inactive_pulses = Arc::new(AtomicUsize::new(0));
        let background_control = control.clone();
        let background_stopped = stopped.clone();
        let pulses = inactive_pulses.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let driver = thread::spawn(move || {
            assert!(!background_control.pulse().unwrap());
            pulses.fetch_add(1, Ordering::SeqCst);
            started_tx.send(()).unwrap();
            while !background_stopped.load(Ordering::SeqCst) {
                assert!(!background_control.pulse().unwrap());
                pulses.fetch_add(1, Ordering::SeqCst);
                thread::yield_now();
            }
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        // Keep servicing the old request during cleanup, before retirement.
        let rollback = db.execute_batch("ROLLBACK");
        let reusable = db.query_row("SELECT 42", [], |r| r.get::<_, i32>(0));
        control.retire().unwrap();
        stopped.store(true, Ordering::SeqCst);
        driver.join().unwrap();
        rollback.unwrap();
        assert_eq!(reusable.unwrap(), 42);
        assert!(inactive_pulses.load(Ordering::SeqCst) > 0);
        assert_eq!(
            observer
                .query_row("SELECT count(*) FROM cancellation_probe", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
