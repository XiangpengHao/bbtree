//! Bounded complete histories checked against a sequential map, including
//! real-time precedence and the complete final ordered contents.

use std::collections::HashSet;
use std::fmt::Debug;
use std::sync::Barrier;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::vec::Vec;

use super::BBTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Record([u64; 8]);

impl Record {
    fn new(token: u64) -> Self {
        Self([token, !token, token, !token, token, !token, token, !token])
    }

    fn token(self) -> u64 {
        assert_eq!(self, Self::new(self.0[0]), "torn value");
        self.0[0]
    }
}

#[derive(Clone, Copy, Debug)]
enum Operation {
    Insert,
    Remove,
    Read,
    Contains,
    Update,
    Upsert,
    GetOrInsert,
    RemoveIfEven,
}

impl Operation {
    fn numbered(number: usize) -> Self {
        match number % 8 {
            0 => Self::Insert,
            1 => Self::Remove,
            2 => Self::Read,
            3 => Self::Contains,
            4 => Self::Update,
            5 => Self::Upsert,
            6 => Self::GetOrInsert,
            _ => Self::RemoveIfEven,
        }
    }

    fn apply(self, state: &mut Option<u64>, token: u64) -> Option<u64> {
        match self {
            Self::Insert | Self::Upsert => state.replace(token),
            Self::Remove => state.take(),
            Self::Read => *state,
            Self::Contains => Some(u64::from(state.is_some())),
            Self::Update => state
                .as_mut()
                .map(|stored| std::mem::replace(stored, token)),
            Self::GetOrInsert => Some(*state.get_or_insert(token)),
            Self::RemoveIfEven => {
                if state.is_some_and(|stored| stored % 2 == 0) {
                    state.take()
                } else {
                    None
                }
            }
        }
    }

    fn execute<K: Ord + Clone>(
        self,
        map: &BBTreeMap<K, Record>,
        key: &K,
        token: u64,
    ) -> Option<u64> {
        match self {
            Self::Insert => map
                .insert(key.clone(), Record::new(token))
                .unwrap()
                .map(Record::token),
            Self::Remove => map.remove(key).map(Record::token),
            Self::Read => map.get_copied(key).map(Record::token),
            Self::Contains => Some(u64::from(map.contains_key(key))),
            Self::Update => map.update(key, |_, stored| {
                std::mem::replace(stored, Record::new(token)).token()
            }),
            Self::Upsert => map
                .update_or_insert(key.clone(), Record::new(token), |_, stored, new| {
                    std::mem::replace(stored, new).token()
                })
                .unwrap(),
            Self::GetOrInsert => Some(
                map.get_or_insert(key.clone(), Record::new(token), |_, stored| stored.token())
                    .unwrap(),
            ),
            Self::RemoveIfEven => map
                .remove_if(key, |_, stored| stored.token() % 2 == 0)
                .map(|(_, stored)| stored.token()),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Observation {
    operation: Operation,
    key: usize,
    token: u64,
    began: usize,
    ended: usize,
    result: Option<u64>,
}

struct History {
    observations: Vec<Observation>,
    before: [Option<u64>; 3],
    after: [Option<u64>; 3],
}

impl History {
    fn is_linearizable(&self) -> bool {
        assert!(self.observations.len() < 64);
        let predecessors: Vec<u64> = self
            .observations
            .iter()
            .map(|operation| {
                self.observations
                    .iter()
                    .enumerate()
                    .fold(0, |mask, (index, other)| {
                        mask | if other.ended < operation.began {
                            1 << index
                        } else {
                            0
                        }
                    })
            })
            .collect();
        self.search(0, self.before, &predecessors, &mut HashSet::new())
    }

    fn search(
        &self,
        chosen: u64,
        state: [Option<u64>; 3],
        predecessors: &[u64],
        rejected: &mut HashSet<(u64, [Option<u64>; 3])>,
    ) -> bool {
        if chosen == (1 << self.observations.len()) - 1 {
            return state == self.after;
        }
        if rejected.contains(&(chosen, state)) {
            return false;
        }
        for (index, observation) in self.observations.iter().enumerate() {
            let bit = 1 << index;
            if chosen & bit != 0 || predecessors[index] & !chosen != 0 {
                continue;
            }
            let mut next = state;
            if observation
                .operation
                .apply(&mut next[observation.key], observation.token)
                == observation.result
                && self.search(chosen | bit, next, predecessors, rejected)
            {
                return true;
            }
        }
        rejected.insert((chosen, state));
        false
    }
}

struct Scenario<K> {
    map: BBTreeMap<K, Record>,
    hot_keys: [K; 3],
    steady: Vec<(K, u64)>,
    clock: AtomicUsize,
}

impl<K: Ord + Clone + Debug + Send + Sync> Scenario<K> {
    fn new(initial: u64, key_of: impl Fn(u64) -> K) -> Self {
        let map = BBTreeMap::new();
        let first_hot = initial.saturating_sub(2);
        let hot_keys = std::array::from_fn(|index| key_of(first_hot + index as u64));
        let mut steady = Vec::new();
        for id in 0..initial {
            let key = key_of(id);
            map.insert(key.clone(), Record::new(id + 1)).unwrap();
            if id < first_hot {
                steady.push((key, id + 1));
            }
        }
        Self {
            map,
            hot_keys,
            steady,
            clock: AtomicUsize::new(0),
        }
    }

    fn values(&self) -> [Option<u64>; 3] {
        self.hot_keys
            .each_ref()
            .map(|key| self.map.get_copied(key).map(Record::token))
    }

    fn operations(&self, round: usize, worker: usize, count: usize) -> Vec<Observation> {
        let mut random = (round as u64 + 1) * 1_000_003 + worker as u64 * 97;
        (0..count)
            .map(|step| {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                let operation = Operation::numbered(random as usize);
                let key = (random >> 8) as usize % self.hot_keys.len();
                let token = 10_000 + round as u64 * 1_000 + worker as u64 * 100 + step as u64;
                let began = self.clock.fetch_add(1, Ordering::SeqCst);
                let result = operation.execute(&self.map, &self.hot_keys[key], token);
                let ended = self.clock.fetch_add(1, Ordering::SeqCst);
                Observation {
                    operation,
                    key,
                    token,
                    began,
                    ended,
                    result,
                }
            })
            .collect()
    }

    fn check(&mut self, before: [Option<u64>; 3], observations: Vec<Observation>) {
        let history = History {
            observations,
            before,
            after: self.values(),
        };
        assert!(
            history.is_linearizable(),
            "no legal sequential history: {:#?}, before={:?}, after={:?}",
            history.observations,
            history.before,
            history.after
        );
        let mut expected = self.steady.clone();
        for (key, value) in self.hot_keys.iter().zip(history.after) {
            if let Some(value) = value {
                expected.push((key.clone(), value));
            }
        }
        expected.sort_by(|left, right| left.0.cmp(&right.0));
        let actual: Vec<_> = self
            .map
            .iter_mut()
            .map(|(key, value)| (key.clone(), value.token()))
            .collect();
        assert_eq!(actual, expected, "complete ordered contents differ");
        self.map.check_invariants(expected.len());
    }

    fn native(&mut self, rounds: usize) {
        const WORKERS: usize = 8;
        for round in 0..rounds {
            let before = self.values();
            let start = Barrier::new(WORKERS);
            let observations = thread::scope(|scope| {
                let handles: Vec<_> = (0..WORKERS)
                    .map(|worker| {
                        let scenario = &*self;
                        let start = &start;
                        scope.spawn(move || {
                            start.wait();
                            scenario.operations(round, worker, 3)
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .flat_map(|handle| handle.join().unwrap())
                    .collect()
            });
            self.check(before, observations);
        }
    }

    #[cfg(shuttle)]
    fn scheduled(&mut self, round: usize) {
        let before = self.values();
        let observations = shuttle::thread::scope(|scope| {
            let handles: Vec<_> = (0..3)
                .map(|worker| {
                    let scenario = &*self;
                    scope.spawn(move || scenario.operations(round, worker, 4))
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|handle| handle.join().unwrap())
                .collect()
        });
        self.check(before, observations);
    }
}

#[test]
fn checker_rejects_stale_reads_and_lost_updates() {
    let write = Observation {
        operation: Operation::Insert,
        key: 0,
        token: 1,
        began: 0,
        ended: 1,
        result: None,
    };
    let stale = Observation {
        operation: Operation::Read,
        key: 0,
        token: 0,
        began: 2,
        ended: 3,
        result: None,
    };
    let mut history = History {
        observations: std::vec![write, stale],
        before: [None; 3],
        after: [Some(1), None, None],
    };
    assert!(!history.is_linearizable());
    history.observations[1].began = 0;
    assert!(
        history.is_linearizable(),
        "overlapping read can precede insert"
    );
    history.after[0] = Some(2);
    assert!(!history.is_linearizable(), "final contents must match too");
    history.observations[1] = Observation {
        operation: Operation::Insert,
        key: 0,
        token: 2,
        began: 0,
        ended: 3,
        result: None,
    };
    history.after[0] = Some(2);
    assert!(
        !history.is_linearizable(),
        "two inserts cannot both replace nothing"
    );
    for observation in &mut history.observations {
        observation.operation = Operation::Remove;
        observation.result = Some(1);
    }
    history.before[0] = Some(1);
    history.after[0] = None;
    assert!(
        !history.is_linearizable(),
        "two removes cannot take one value twice"
    );
}

#[test]
#[cfg_attr(target_family = "wasm", ignore = "needs native threads")]
fn native_contended_histories_are_linearizable() {
    for initial in [0, 12, 360] {
        Scenario::new(initial, |id| id).native(128);
        Scenario::new(initial, |id| std::format!("shared/{id:08}")).native(128);
    }
}

#[cfg(shuttle)]
#[test]
fn scheduled_contended_histories_are_linearizable() {
    use shuttle::scheduler::{PctScheduler, RandomScheduler, Scheduler};
    use shuttle::{Config, Runner};
    for initial in [0, 12, 360] {
        let schedulers: [std::boxed::Box<dyn Scheduler + Send>; 2] = [
            std::boxed::Box::new(RandomScheduler::new(1_000)),
            std::boxed::Box::new(PctScheduler::new(4, 1_000)),
        ];
        for scheduler in schedulers {
            let checked = std::sync::Arc::new(AtomicUsize::new(0));
            let completed = std::sync::Arc::clone(&checked);
            let mut config = Config::new();
            config.max_time = Some(std::time::Duration::from_secs(15));
            Runner::new(scheduler, config).run(move || {
                let mut numbers = Scenario::new(initial, |id| id);
                let mut strings = Scenario::new(initial, |id| std::format!("shared/{id:08}"));
                for round in 0..4 {
                    numbers.scheduled(round);
                    strings.scheduled(round);
                    completed.fetch_add(2, Ordering::Relaxed);
                }
            });
            std::eprintln!(
                "initial={initial}: checked {} complete single-key histories",
                checked.load(Ordering::Relaxed)
            );
        }
    }
}
