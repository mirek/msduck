//! The process-wide application lock table shared by every session.
//!
//! A lock is identified by database, database principal and resource name;
//! it is held by owners (a session's session owner or its transaction owner)
//! with a reference count and a mode. Requests that conflict wait in a FIFO
//! queue; conversions only wait for the granted group. Locks held by the same
//! session never conflict with each other, whichever owner holds them.
use std::{
    collections::HashMap,
    sync::{Condvar, LazyLock, Mutex, MutexGuard},
    time::{Duration, Instant},
};

/// SQL Server's application lock modes. Only the first five can be requested;
/// the intent combinations arise when one owner requests several modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    IntentShared,
    Shared,
    Update,
    IntentExclusive,
    SharedIntentExclusive,
    UpdateIntentExclusive,
    Exclusive,
}

impl Mode {
    /// A requestable mode, compared case-insensitively and ignoring trailing
    /// spaces as the procedures' CASE expressions do.
    pub(crate) fn requested(text: &str) -> Option<Self> {
        let text = text.trim_end_matches(' ');
        [
            Self::Shared,
            Self::Update,
            Self::Exclusive,
            Self::IntentExclusive,
            Self::IntentShared,
        ]
        .into_iter()
        .find(|mode| mode.name().eq_ignore_ascii_case(text))
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::IntentShared => "IntentShared",
            Self::Shared => "Shared",
            Self::Update => "Update",
            Self::IntentExclusive => "IntentExclusive",
            Self::SharedIntentExclusive => "SharedIntentExclusive",
            Self::UpdateIntentExclusive => "UpdateIntentExclusive",
            Self::Exclusive => "Exclusive",
        }
    }

    /// (resource level: none, S, U, X; intent: none, IS, IX)
    fn parts(self) -> (u8, u8) {
        match self {
            Self::IntentShared => (0, 1),
            Self::Shared => (1, 0),
            Self::Update => (2, 0),
            Self::IntentExclusive => (0, 2),
            Self::SharedIntentExclusive => (1, 2),
            Self::UpdateIntentExclusive => (2, 2),
            Self::Exclusive => (3, 0),
        }
    }

    /// The mode an owner holds after also requesting `other`. It never
    /// weakens, even after some of the requests are released.
    pub(crate) fn union(self, other: Self) -> Self {
        let (a, i) = self.parts();
        let (b, j) = other.parts();
        match (a.max(b), i.max(j)) {
            (3, _) => Self::Exclusive,
            (0, 1) => Self::IntentShared,
            (0, _) => Self::IntentExclusive,
            (1, 2) => Self::SharedIntentExclusive,
            (1, _) => Self::Shared,
            (_, 2) => Self::UpdateIntentExclusive,
            _ => Self::Update,
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    /// SQL Server's lock compatibility matrix (it is symmetric).
    pub(crate) fn compatible(self, other: Self) -> bool {
        const MATRIX: [[bool; 7]; 7] = {
            const Y: bool = true;
            const N: bool = false;
            [
                [Y, Y, Y, Y, Y, Y, N],
                [Y, Y, Y, N, N, N, N],
                [Y, Y, N, N, N, N, N],
                [Y, N, N, Y, N, N, N],
                [Y, N, N, N, N, N, N],
                [Y, N, N, N, N, N, N],
                [N, N, N, N, N, N, N],
            ]
        };
        MATRIX[self.index()][other.index()]
    }
}

/// A lock owner: a session (by its process-unique token) and whether the
/// lock belongs to its transaction rather than to the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Owner {
    pub session: u64,
    pub transaction: bool,
}

/// Database (case-insensitive), principal (case-insensitive) and resource
/// (exact UTF-16 units, at most 255).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Key {
    database: String,
    principal: String,
    resource: Vec<u16>,
}

impl Key {
    pub(crate) fn new(database: &str, principal: &str, resource: &[u16]) -> Self {
        Self {
            database: database.to_lowercase(),
            principal: principal.to_lowercase(),
            resource: resource.to_vec(),
        }
    }
}

/// The result of a lock request, as sp_getapplock reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Acquired {
    Granted = 0,
    GrantedAfterWait = 1,
    TimedOut = -1,
    Cancelled = -2,
    DeadlockVictim = -3,
}

struct Grant {
    owner: Owner,
    mode: Mode,
    count: u32,
}

struct Waiter {
    id: u64,
    owner: Owner,
    /// The requested mode, and the mode the owner holds once granted.
    requested: Mode,
    target: Mode,
    conversion: bool,
}

#[derive(Default)]
struct Resource {
    granted: Vec<Grant>,
    queue: Vec<Waiter>,
}

impl Resource {
    fn held(&self, owner: Owner) -> Option<&Grant> {
        self.granted.iter().find(|grant| grant.owner == owner)
    }

    /// Whether a request for `target` must wait: it conflicts with a lock
    /// another session holds, or (unless it converts a held lock) with a
    /// request queued ahead of it. A conversion waits only for conversions
    /// queued ahead of it.
    fn blocked(&self, owner: Owner, target: Mode, conversion: bool, ahead: &[Waiter]) -> bool {
        let other = |session| session != owner.session;
        self.granted
            .iter()
            .any(|grant| other(grant.owner.session) && !grant.mode.compatible(target))
            || ahead.iter().any(|waiter| {
                other(waiter.owner.session)
                    && (!conversion || waiter.conversion)
                    && !waiter.target.compatible(target)
            })
    }

    fn grant(&mut self, owner: Owner, requested: Mode) {
        match self.granted.iter_mut().find(|grant| grant.owner == owner) {
            Some(grant) => {
                grant.mode = grant.mode.union(requested);
                grant.count += 1;
            }
            None => self.granted.push(Grant {
                owner,
                mode: requested,
                count: 1,
            }),
        }
    }

    /// Grant every queued request that no longer has to wait, in order.
    fn wake(&mut self, outcomes: &mut HashMap<u64, Acquired>) {
        let mut index = 0;
        while index < self.queue.len() {
            let waiter = &self.queue[index];
            if self.blocked(
                waiter.owner,
                waiter.target,
                waiter.conversion,
                &self.queue[..index],
            ) {
                index += 1;
                continue;
            }
            let waiter = self.queue.remove(index);
            self.grant(waiter.owner, waiter.requested);
            outcomes.insert(waiter.id, Acquired::GrantedAfterWait);
            index = 0;
        }
    }

    /// Sessions the request queued at `index` waits for.
    fn blockers(&self, index: usize) -> impl Iterator<Item = u64> + '_ {
        let waiter = &self.queue[index];
        let other = move |session| session != waiter.owner.session;
        self.granted
            .iter()
            .filter(move |grant| {
                other(grant.owner.session) && !grant.mode.compatible(waiter.target)
            })
            .map(|grant| grant.owner.session)
            .chain(
                self.queue[..index]
                    .iter()
                    .filter(move |ahead| {
                        other(ahead.owner.session)
                            && (!waiter.conversion || ahead.conversion)
                            && !ahead.target.compatible(waiter.target)
                    })
                    .map(|ahead| ahead.owner.session),
            )
    }
}

#[derive(Default)]
struct Table {
    resources: HashMap<Key, Resource>,
    /// Resolved waits, by waiter id, until the waiting session reads them.
    outcomes: HashMap<u64, Acquired>,
    next: u64,
}

impl Table {
    fn tidy(&mut self, key: &Key) {
        if self
            .resources
            .get(key)
            .is_some_and(|resource| resource.granted.is_empty() && resource.queue.is_empty())
        {
            self.resources.remove(key);
        }
    }

    /// Remove a queued request (timeout, cancellation or deadlock) and let
    /// the requests behind it proceed.
    fn withdraw(&mut self, key: &Key, id: u64) {
        if let Some(resource) = self.resources.get_mut(key) {
            resource.queue.retain(|waiter| waiter.id != id);
            resource.wake(&mut self.outcomes);
        }
        self.tidy(key);
    }

    /// Where each waiting session waits: (key, queue index, waiter id).
    fn waits(&self) -> HashMap<u64, (&Key, usize, u64)> {
        let mut waits = HashMap::new();
        for (key, resource) in &self.resources {
            for (index, waiter) in resource.queue.iter().enumerate() {
                waits.insert(waiter.owner.session, (key, index, waiter.id));
            }
        }
        waits
    }

    /// If the request `id` of `session` is part of a wait-for cycle, the
    /// cycle's victim: the request that has waited longest.
    fn deadlock(&self, session: u64) -> Option<(Key, u64)> {
        let waits = self.waits();
        waits.get(&session)?;
        // Depth-first search over sessions for a path back to `session`.
        let mut path = vec![session];
        let mut stack: Vec<Vec<u64>> = vec![self.blocking(&waits, session)];
        let mut seen = vec![session];
        while let Some(next) = stack.last_mut() {
            let Some(blocker) = next.pop() else {
                stack.pop();
                path.pop();
                continue;
            };
            if blocker == session {
                let victim = path
                    .iter()
                    .filter_map(|member| waits.get(member))
                    .min_by_key(|(_, _, id)| *id)
                    .map(|(key, _, id)| ((*key).clone(), *id));
                return victim;
            }
            if seen.contains(&blocker) || !waits.contains_key(&blocker) {
                continue;
            }
            seen.push(blocker);
            path.push(blocker);
            stack.push(self.blocking(&waits, blocker));
        }
        None
    }

    fn blocking(&self, waits: &HashMap<u64, (&Key, usize, u64)>, session: u64) -> Vec<u64> {
        let Some((key, index, _)) = waits.get(&session) else {
            return Vec::new();
        };
        let mut blockers: Vec<u64> = self.resources[*key].blockers(*index).collect();
        blockers.sort_unstable();
        blockers.dedup();
        blockers
    }
}

static TABLE: LazyLock<(Mutex<Table>, Condvar)> = LazyLock::new(Default::default);

fn table() -> MutexGuard<'static, Table> {
    TABLE
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// How often a waiting request rechecks cancellation and deadlocks.
const POLL: Duration = Duration::from_millis(25);

/// Request `requested` for `owner`. `timeout` of `None` waits until the lock
/// is granted, the request is chosen as a deadlock victim, or `cancelled`
/// reports that the request was cancelled.
pub(crate) fn acquire(
    key: &Key,
    owner: Owner,
    requested: Mode,
    timeout: Option<Duration>,
    cancelled: &dyn Fn() -> bool,
) -> Acquired {
    let mut table = table();
    let id = table.next;
    table.next += 1;
    let resource = table.resources.entry(key.clone()).or_default();
    let held = resource.held(owner).map(|grant| grant.mode);
    let target = held.map_or(requested, |mode| mode.union(requested));
    let conversion = held.is_some();
    if held == Some(target) || !resource.blocked(owner, target, conversion, &resource.queue) {
        resource.grant(owner, requested);
        return Acquired::Granted;
    }
    if timeout == Some(Duration::ZERO) {
        table.tidy(key);
        return Acquired::TimedOut;
    }
    resource.queue.push(Waiter {
        id,
        owner,
        requested,
        target,
        conversion,
    });
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    loop {
        if let Some(outcome) = table.outcomes.remove(&id) {
            return outcome;
        }
        if let Some((victim_key, victim)) = table.deadlock(owner.session) {
            table.withdraw(&victim_key, victim);
            TABLE.1.notify_all();
            if victim == id {
                return Acquired::DeadlockVictim;
            }
            table.outcomes.insert(victim, Acquired::DeadlockVictim);
            continue;
        }
        let now = Instant::now();
        let expired = deadline.is_some_and(|deadline| now >= deadline);
        if expired || cancelled() {
            table.withdraw(key, id);
            TABLE.1.notify_all();
            return if expired {
                Acquired::TimedOut
            } else {
                Acquired::Cancelled
            };
        }
        let wait = deadline.map_or(POLL, |deadline| (deadline - now).min(POLL));
        table = TABLE
            .1
            .wait_timeout(table, wait)
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .0;
    }
}

/// Release one reference of `owner`'s lock; false when it holds none.
pub(crate) fn release(key: &Key, owner: Owner) -> bool {
    let mut guard = table();
    let table = &mut *guard;
    let Some(resource) = table.resources.get_mut(key) else {
        return false;
    };
    let Some(index) = resource
        .granted
        .iter()
        .position(|grant| grant.owner == owner)
    else {
        return false;
    };
    resource.granted[index].count -= 1;
    if resource.granted[index].count == 0 {
        resource.granted.remove(index);
        resource.wake(&mut table.outcomes);
        TABLE.1.notify_all();
    }
    table.tidy(key);
    true
}

/// Release every lock held by owners matching `release`, for example at the
/// end of a transaction or session.
pub(crate) fn release_all(release: impl Fn(Owner) -> bool) {
    let mut guard = table();
    let table = &mut *guard;
    let mut changed = false;
    for resource in table.resources.values_mut() {
        let before = resource.granted.len();
        resource.granted.retain(|grant| !release(grant.owner));
        if resource.granted.len() != before {
            resource.wake(&mut table.outcomes);
            changed = true;
        }
    }
    table
        .resources
        .retain(|_, resource| !resource.granted.is_empty() || !resource.queue.is_empty());
    if changed {
        TABLE.1.notify_all();
    }
}

/// The mode `owner` holds, if any.
pub(crate) fn mode(key: &Key, owner: Owner) -> Option<Mode> {
    table()
        .resources
        .get(key)
        .and_then(|resource| resource.held(owner))
        .map(|grant| grant.mode)
}

/// Whether a request for `requested` would be granted without waiting.
pub(crate) fn test(key: &Key, owner: Owner, requested: Mode) -> bool {
    let table = table();
    let Some(resource) = table.resources.get(key) else {
        return true;
    };
    let held = resource.held(owner).map(|grant| grant.mode);
    let target = held.map_or(requested, |mode| mode.union(requested));
    held == Some(target) || !resource.blocked(owner, target, held.is_some(), &resource.queue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use Mode::*;
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
        thread,
    };

    const MODES: [Mode; 7] = [
        IntentShared,
        Shared,
        Update,
        IntentExclusive,
        SharedIntentExclusive,
        UpdateIntentExclusive,
        Exclusive,
    ];

    static NEXT: AtomicU64 = AtomicU64::new(1 << 40);

    fn session() -> Owner {
        Owner {
            session: NEXT.fetch_add(1, Ordering::Relaxed),
            transaction: false,
        }
    }

    fn key(name: &str) -> Key {
        Key::new(
            "unit",
            "public",
            &format!("{name}-{}", NEXT.fetch_add(1, Ordering::Relaxed))
                .encode_utf16()
                .collect::<Vec<_>>(),
        )
    }

    const NEVER: &dyn Fn() -> bool = &|| false;

    #[test]
    fn unions_match_sql_server() {
        // reference/gaps-applock.json, "union *" observations.
        let cases = [
            (IntentShared, Shared, Shared),
            (IntentShared, IntentExclusive, IntentExclusive),
            (Shared, Update, Update),
            (Shared, IntentExclusive, SharedIntentExclusive),
            (Update, IntentExclusive, UpdateIntentExclusive),
            (IntentExclusive, Update, UpdateIntentExclusive),
            (Exclusive, IntentShared, Exclusive),
            (SharedIntentExclusive, Update, UpdateIntentExclusive),
            (IntentShared, IntentShared, IntentShared),
        ];
        for (a, b, union) in cases {
            assert_eq!(a.union(b), union, "{a:?} + {b:?}");
            assert_eq!(b.union(a), union, "{b:?} + {a:?}");
        }
        for a in MODES {
            assert_eq!(a.union(a), a);
        }
    }

    #[test]
    fn compatibility_is_symmetric_and_matches_sql_server() {
        for a in MODES {
            for b in MODES {
                assert_eq!(a.compatible(b), b.compatible(a));
            }
        }
        // Held mode, then the requestable modes IS, S, U, IX, X.
        let expected = [
            (IntentShared, [true, true, true, true, false]),
            (Shared, [true, true, true, false, false]),
            (Update, [true, true, false, false, false]),
            (IntentExclusive, [true, false, false, true, false]),
            (SharedIntentExclusive, [true, false, false, false, false]),
            (UpdateIntentExclusive, [true, false, false, false, false]),
            (Exclusive, [false; 5]),
        ];
        for (held, row) in expected {
            for (request, compatible) in [IntentShared, Shared, Update, IntentExclusive, Exclusive]
                .into_iter()
                .zip(row)
            {
                assert_eq!(
                    held.compatible(request),
                    compatible,
                    "{held:?} vs {request:?}"
                );
            }
        }
    }

    #[test]
    fn requested_modes_parse_like_the_procedure() {
        assert_eq!(Mode::requested("exclusive "), Some(Exclusive));
        assert_eq!(Mode::requested("IntentShared"), Some(IntentShared));
        assert_eq!(Mode::requested("SharedIntentExclusive"), None);
        assert_eq!(Mode::requested(" Shared"), None);
    }

    #[test]
    fn references_are_counted_and_modes_never_weaken() {
        let (key, a) = (key("count"), session());
        assert_eq!(acquire(&key, a, Shared, None, NEVER), Acquired::Granted);
        assert_eq!(
            acquire(&key, a, IntentExclusive, None, NEVER),
            Acquired::Granted
        );
        assert_eq!(mode(&key, a), Some(SharedIntentExclusive));
        assert!(release(&key, a));
        assert_eq!(mode(&key, a), Some(SharedIntentExclusive));
        assert!(release(&key, a));
        assert_eq!(mode(&key, a), None);
        assert!(!release(&key, a));
    }

    #[test]
    fn other_sessions_conflict_and_owners_of_one_session_do_not() {
        let (key, a, b) = (key("conflict"), session(), session());
        let a_transaction = Owner {
            transaction: true,
            ..a
        };
        assert_eq!(acquire(&key, a, Exclusive, None, NEVER), Acquired::Granted);
        assert_eq!(
            acquire(&key, a_transaction, Exclusive, Some(Duration::ZERO), NEVER),
            Acquired::Granted
        );
        assert_eq!(
            acquire(&key, b, IntentShared, Some(Duration::ZERO), NEVER),
            Acquired::TimedOut
        );
        assert!(!test(&key, b, IntentShared));
        let started = Instant::now();
        assert_eq!(
            acquire(&key, b, Shared, Some(Duration::from_millis(120)), NEVER),
            Acquired::TimedOut
        );
        assert!(started.elapsed() >= Duration::from_millis(120));
        assert!(!release(&key, b));
        release_all(|owner| owner.session == a.session);
        assert!(test(&key, b, Exclusive));
    }

    #[test]
    fn waiters_are_granted_after_release() {
        let (key, a, b) = (key("wait"), session(), session());
        assert_eq!(acquire(&key, a, Exclusive, None, NEVER), Acquired::Granted);
        let waiter = {
            let key = key.clone();
            thread::spawn(move || acquire(&key, b, Exclusive, Some(Duration::from_secs(10)), NEVER))
        };
        thread::sleep(Duration::from_millis(100));
        assert!(release(&key, a));
        assert_eq!(waiter.join().unwrap(), Acquired::GrantedAfterWait);
        assert_eq!(mode(&key, b), Some(Exclusive));
        release_all(|owner| owner.session == b.session);
    }

    #[test]
    fn queued_requests_block_later_compatible_requests() {
        let (key, a, b, c) = (key("fifo"), session(), session(), session());
        assert_eq!(acquire(&key, a, Shared, None, NEVER), Acquired::Granted);
        let waiter = {
            let key = key.clone();
            thread::spawn(move || {
                acquire(&key, b, Exclusive, Some(Duration::from_millis(400)), NEVER)
            })
        };
        thread::sleep(Duration::from_millis(100));
        assert_eq!(
            acquire(&key, c, Shared, Some(Duration::ZERO), NEVER),
            Acquired::TimedOut
        );
        assert!(!test(&key, c, Shared));
        assert_eq!(waiter.join().unwrap(), Acquired::TimedOut);
        assert!(test(&key, c, Shared));
        release_all(|owner| owner.session == a.session);
    }

    #[test]
    fn conversions_wait_for_the_granted_group_only() {
        let (key, a, b) = (key("convert"), session(), session());
        assert_eq!(acquire(&key, a, Shared, None, NEVER), Acquired::Granted);
        assert_eq!(acquire(&key, b, Shared, None, NEVER), Acquired::Granted);
        assert_eq!(
            acquire(&key, a, Exclusive, Some(Duration::ZERO), NEVER),
            Acquired::TimedOut
        );
        assert_eq!(mode(&key, a), Some(Shared));
        let converter = {
            let key = key.clone();
            thread::spawn(move || acquire(&key, a, Exclusive, Some(Duration::from_secs(10)), NEVER))
        };
        thread::sleep(Duration::from_millis(100));
        assert!(release(&key, b));
        assert_eq!(converter.join().unwrap(), Acquired::GrantedAfterWait);
        assert_eq!(mode(&key, a), Some(Exclusive));
        assert!(release(&key, a));
        assert_eq!(mode(&key, a), Some(Exclusive));
        assert!(release(&key, a));
    }

    #[test]
    fn deadlocks_choose_the_longest_waiting_request() {
        let (one, two, a, b) = (key("d1"), key("d2"), session(), session());
        assert_eq!(acquire(&one, a, Exclusive, None, NEVER), Acquired::Granted);
        assert_eq!(acquire(&two, b, Exclusive, None, NEVER), Acquired::Granted);
        let first = {
            let two = two.clone();
            thread::spawn(move || acquire(&two, a, Exclusive, None, NEVER))
        };
        thread::sleep(Duration::from_millis(100));
        let second = {
            let one = one.clone();
            thread::spawn(move || acquire(&one, b, Exclusive, Some(Duration::from_secs(10)), NEVER))
        };
        assert_eq!(first.join().unwrap(), Acquired::DeadlockVictim);
        // The victim keeps its locks until it releases them.
        assert_eq!(mode(&one, a), Some(Exclusive));
        assert!(release(&one, a));
        assert_eq!(second.join().unwrap(), Acquired::GrantedAfterWait);
        release_all(|owner| owner.session == b.session);
    }

    #[test]
    fn cancellation_withdraws_the_request() {
        let (key, a, b) = (key("cancel"), session(), session());
        assert_eq!(acquire(&key, a, Exclusive, None, NEVER), Acquired::Granted);
        let flag = Arc::new(AtomicBool::new(false));
        let waiter = {
            let (key, flag) = (key.clone(), flag.clone());
            thread::spawn(move || acquire(&key, b, Shared, None, &|| flag.load(Ordering::Relaxed)))
        };
        thread::sleep(Duration::from_millis(100));
        flag.store(true, Ordering::Relaxed);
        assert_eq!(waiter.join().unwrap(), Acquired::Cancelled);
        release_all(|owner| owner.session == a.session);
        assert!(!table().resources.contains_key(&key));
    }
}
