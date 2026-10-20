//! Reproducers for the requested atomic whole-batch contract.

use shuttle::thread;

use super::BBTreeMap;

struct BatchReadReview;

impl BatchReadReview {
    fn check(lookup: fn(&BBTreeMap<u64, u64>) -> [Option<u64>; 2]) {
        shuttle::check_random(
            move || {
                let map = BBTreeMap::<u64, u64>::new();
                map.insert(7, 0).unwrap();
                thread::scope(|scope| {
                    let writer = scope.spawn(|| {
                        assert_eq!(map.insert(7, 1).unwrap(), Some(0));
                    });
                    let observed = lookup(&map);
                    writer.join().unwrap();
                    assert_eq!(
                        observed[0], observed[1],
                        "an atomic batch looking up one key twice must observe the same value: {observed:?}"
                    );
                });
            },
            1_000,
        );
    }
}

#[test]
#[ignore = "demonstrates that copied lookup batches are not linearizable as a whole"]
fn copied_batch_is_not_one_atomic_read() {
    BatchReadReview::check(|map| map.get_many_copied([&7, &7]));
}

#[test]
#[ignore = "demonstrates that callback lookup batches are not linearizable as a whole"]
fn callback_batch_is_not_one_atomic_read() {
    BatchReadReview::check(|map| map.read_many([&7, &7], |_, value| *value));
}

#[test]
#[ignore = "demonstrates that cloned lookup batches are not linearizable as a whole"]
fn cloned_batch_is_not_one_atomic_read() {
    BatchReadReview::check(|map| map.get_many_cloned([&7, &7]));
}
