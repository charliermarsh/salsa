use salsa::plumbing::AsId as _;

use crate::sync::{Arc, Barrier, thread};

#[salsa::input]
struct Input {
    value: u32,
}

#[salsa::db]
trait BarrierDatabase: salsa::Database {
    fn barrier(&self) -> &Barrier;
}

#[salsa::db]
#[derive(Clone)]
struct Database {
    storage: salsa::Storage<Self>,
    barrier: Arc<Barrier>,
}

impl Database {
    fn new() -> Self {
        Self {
            storage: salsa::Storage::new(None),
            barrier: Arc::new(Barrier::new(2)),
        }
    }
}

#[salsa::db]
impl salsa::Database for Database {}

#[salsa::db]
impl BarrierDatabase for Database {
    fn barrier(&self) -> &Barrier {
        &self.barrier
    }
}

#[salsa::tracked]
fn read(db: &dyn BarrierDatabase, input: Input) -> u32 {
    db.barrier().wait();
    input.value(db)
}

#[test]
fn different_pages_of_the_same_tracked_function_execute_in_parallel() {
    crate::sync::check(|| {
        let db = Database::new();
        let inputs: Vec<_> = (0..=1024).map(|value| Input::new(&db, value)).collect();
        let first = inputs[0];
        let second = inputs[1024];

        assert_eq!(second.as_id().index() - first.as_id().index(), 1024);

        let db1 = db.clone();
        let db2 = db.clone();
        let thread1 = thread::spawn(move || read(&db1, first));
        let thread2 = thread::spawn(move || read(&db2, second));

        assert_eq!(thread1.join().unwrap(), 0);
        assert_eq!(thread2.join().unwrap(), 1024);

        // Both claims were released, and the memoized values remain available.
        assert_eq!(read(&db, first), 0);
        assert_eq!(read(&db, second), 1024);
    });
}
