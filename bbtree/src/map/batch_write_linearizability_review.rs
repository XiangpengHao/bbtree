//! Review regression for the requirement that one batch is one atomic operation.

use std::sync::Arc;

use shuttle::thread;

use super::BBTreeMap;

#[test]
#[ignore = "known bug: another writer can interleave between updates to the same key in a batch"]
fn insert_many_on_one_key_has_one_linearization_point() {
    shuttle::check_random(
        || {
            let map = Arc::new(BBTreeMap::<u64, u64>::new());
            assert_eq!(map.insert(7, 0).unwrap(), None);

            let batch_map = Arc::clone(&map);
            let batch =
                thread::spawn(move || batch_map.insert_many([(7, 1), (7, 3)]).map(Result::unwrap));
            let single_map = Arc::clone(&map);
            let single = thread::spawn(move || single_map.insert(7, 2).unwrap());

            let batch_result = batch.join().unwrap();
            let single_result = single.join().unwrap();
            let final_value = map.get_copied(&7);

            let batch_then_single = batch_result == [Some(0), Some(1)]
                && single_result == Some(3)
                && final_value == Some(2);
            let single_then_batch = batch_result == [Some(2), Some(1)]
                && single_result == Some(0)
                && final_value == Some(3);
            assert!(
                batch_then_single || single_then_batch,
                "no atomic order explains batch={batch_result:?}, single={single_result:?}, final={final_value:?}"
            );
        },
        1_000,
    );
}
