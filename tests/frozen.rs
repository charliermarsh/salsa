#![cfg(all(feature = "inventory", not(feature = "persistence")))]

use salsa::plumbing::HasStorage;
use salsa::{Database, DatabaseImpl, Setter};

#[salsa::input]
struct Input {
    #[returns(copy)]
    value: u32,
}

#[salsa::tracked(returns(copy))]
fn inner(db: &dyn Database, input: Input) -> u32 {
    input.value(db)
}

#[salsa::tracked(returns(copy))]
fn outer(db: &dyn Database, input: Input) -> u32 {
    inner(db, input)
}

#[test]
fn revalidate_memos_after_freeze() {
    let mut db = DatabaseImpl::new();
    let input = Input::new(&db, 1);
    assert_eq!(outer(&db, input), 1);
    input.set_value(&mut db).to(2);
    assert!(db.storage_mut().try_freeze());
    // Refresh the inner memo before checking the validity of the older outer memo.
    assert_eq!(inner(&db, input), 2);
    assert_eq!(outer(&db.clone(), input), 2);
    assert_eq!(outer(&db, Input::new(&db, 3)), 3);
    db.trigger_cancellation();
    assert_eq!(outer(&db, input), 2);
}

#[test]
#[should_panic(expected = "cannot start a new revision after freezing")]
fn setting_new_inputs_is_also_forbidden() {
    let mut db = DatabaseImpl::new();
    assert!(db.storage_mut().try_freeze());
    let input = Input::new(&db, 1);
    input.set_value(&mut db).to(2);
}

#[test]
#[should_panic(expected = "cannot start a new revision after freezing")]
fn writes_remain_forbidden_after_cloning() {
    let mut db = DatabaseImpl::new();
    assert!(db.storage_mut().try_freeze());
    let mut db2 = db.clone();
    drop(db);
    db2.synthetic_write(salsa::Durability::LOW);
}

#[salsa::tracked]
struct Item<'db> {
    #[returns(copy)]
    value: (),
}

#[salsa::tracked(returns(copy), specify)]
fn specified(db: &dyn Database, item: Item<'_>) -> u32 {
    item.value(db);
    0
}

#[salsa::tracked(returns(copy), cycle_initial = initial)]
fn cycle(db: &dyn Database) -> Option<(Item<'_>, Item<'_>)> {
    let (a, b) = cycle(db).unwrap_or_else(|| (Item::new(db, ()), Item::new(db, ())));
    specified::specify(db, a, 1);
    specified::specify(db, b, 2);
    assert_eq!(specified(db, a), 1);
    assert_eq!(specified(db, b), 2);
    Some((a, b))
}

fn initial<'db>(
    _db: &'db dyn Database,
    _id: salsa::Id,
    _last: Option<&Option<(Item<'db>, Item<'db>)>>,
) -> Option<(Item<'db>, Item<'db>)> {
    None
}

#[test]
fn cycle_reuses_distinct_tracked_ids() {
    let mut db = DatabaseImpl::new();
    assert!(db.storage_mut().try_freeze());
    let (a, b) = cycle(&db).unwrap();
    assert!(a != b);
    assert_eq!(specified(&db, a), 1);
    assert_eq!(specified(&db, b), 2);
}

#[salsa::tracked(returns(copy))]
fn plain_specify(db: &dyn Database) -> Item<'_> {
    let item = Item::new(db, ());
    specified::specify(db, item, 42);
    item
}

#[test]
fn specified_outputs_remain_accessible() {
    let mut db = DatabaseImpl::new();
    assert!(db.storage_mut().try_freeze());
    assert_eq!(specified(&db, plain_specify(&db)), 42);
}

#[cfg(feature = "accumulator")]
mod accumulated {
    use super::*;
    use salsa::Accumulator;
    #[salsa::accumulator]
    struct Log(u32);
    #[salsa::tracked(returns(copy))]
    fn produce(db: &dyn Database, input: Input) {
        Log(input.value(db)).accumulate(db);
    }
    #[salsa::tracked(returns(copy))]
    fn forward(db: &dyn Database, input: Input) {
        produce(db, input);
    }
    #[test]
    fn accumulated_values_reach_callers() {
        let mut db = DatabaseImpl::new();
        assert!(db.storage_mut().try_freeze());
        let input = Input::new(&db, 2);
        let logs = forward::accumulated::<Log>(&db, input);
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].0, 2);
    }
}

#[test]
fn freeze_is_exclusive_and_idempotent() {
    let mut db = DatabaseImpl::new();
    let handle = db.clone();
    assert!(!db.storage_mut().try_freeze());
    drop(handle);
    assert!(db.storage_mut().try_freeze());
    let _handle = db.clone();
    assert!(db.storage_mut().try_freeze());
}

#[test]
#[should_panic(expected = "cannot evict values after freezing")]
fn reject_eviction() {
    let mut db = DatabaseImpl::new();
    assert!(db.storage_mut().try_freeze());
    db.trigger_lru_eviction();
}

#[test]
fn frozen_queries_still_honor_local_cancellation() {
    let mut db = DatabaseImpl::new();
    assert!(db.storage_mut().try_freeze());
    let input = Input::new(&db, 1);
    inner(&db, input);
    let clone = db.clone();
    let token = clone.cancellation_token();
    token.cancel();
    assert!(salsa::Cancelled::catch(|| inner(&clone, input)).is_err());
    assert_eq!(inner(&db, input), 1);
}

#[salsa::interned(revisions = 1)]
struct Interned<'db> {
    #[returns(copy)]
    value: u32,
}

#[salsa::tracked(returns(copy))]
fn intern(db: &dyn Database, input: Input) -> Interned<'_> {
    Interned::new(db, input.value(db))
}

#[test]
fn read_interned_values_after_a_final_edit() {
    let mut db = DatabaseImpl::new();
    let input = Input::new(&db, 13);
    let stable = Input::new(&db, 2);
    for value in 14..32 {
        assert_eq!(intern(&db, input).value(&db), value - 1);
        assert_eq!(intern(&db, stable).value(&db), 2);
        input.set_value(&mut db).to(value);
    }
    assert!(db.storage_mut().try_freeze());
    assert_eq!(intern(&db, input).value(&db), 31);
    assert_eq!(intern(&db, stable).value(&db), 2);
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let db = db.clone();
            scope.spawn(move || {
                assert_eq!(intern(&db, input).value(&db), 31);
                for i in 0..200 {
                    let key = Interned::new(&db, i);
                    assert!(key == Interned::new(&db, i));
                    assert_eq!(key.value(&db), i);
                }
            });
        }
    });
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, salsa::SalsaValue)]
struct Collision(u32);

impl std::hash::Hash for Collision {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write_u32(1);
    }
}

#[salsa::interned]
struct Key<'db> {
    value: Collision,
}

#[test]
fn a_cached_hash_is_only_a_hint() {
    let mut db = DatabaseImpl::new();
    assert!(db.storage_mut().try_freeze());
    let a = Key::new(&db, Collision(0));
    assert!(a == Key::new(&db, Collision(0)));
    let b = Key::new(&db, Collision(1));
    assert!(b != a);
    assert!(b == Key::new(&db, Collision(1)));
    assert!(a == Key::new(&db, Collision(0)));
}
