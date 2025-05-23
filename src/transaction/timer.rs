use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        atomic::{AtomicU64, Ordering},
        RwLock,
    },
    time::{Duration, Instant},
};

#[derive(Debug, PartialOrd, PartialEq, Eq, Clone)]
struct TimerKey {
    task_id: u64,
    execute_at: Instant,
}

impl Ord for TimerKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.execute_at.cmp(&other.execute_at)
    }
}

pub struct Timer<T> {
    tasks: RwLock<BTreeMap<TimerKey, T>>,
    id_to_tasks: RwLock<HashMap<u64, Instant>>,
    last_task_id: AtomicU64,
}

impl<T> Timer<T> {
    pub fn new() -> Self {
        Timer {
            tasks: RwLock::new(BTreeMap::new()),
            id_to_tasks: RwLock::new(HashMap::new()),
            last_task_id: AtomicU64::new(1),
        }
    }

    pub fn len(&self) -> usize {
        self.tasks.read().unwrap().len()
    }

    pub fn timeout(&self, duration: Duration, value: T) -> u64 {
        self.timeout_at(Instant::now() + duration, value)
    }

    pub fn timeout_at(&self, execute_at: Instant, value: T) -> u64 {
        let task_id = self.last_task_id.fetch_add(1, Ordering::Relaxed);
        self.tasks.write().unwrap().insert(
            TimerKey {
                task_id,
                execute_at,
            },
            value,
        );

        self.id_to_tasks
            .write()
            .unwrap()
            .insert(task_id, execute_at);
        task_id
    }

    pub fn cancel(&self, task_id: u64) -> Option<T> {
        let position = { self.id_to_tasks.write().unwrap().remove(&task_id) };
        if let Some(execute_at) = position {
            self.tasks.write().unwrap().remove(&TimerKey {
                task_id,
                execute_at,
            })
        } else {
            None
        }
    }

    pub fn poll(&self, now: Instant) -> Vec<T> {
        let mut result = Vec::new();
        let keys_to_remove = {
            let mut tasks = self.tasks.write().unwrap();
            let keys_to_remove = tasks
                .range(
                    ..=TimerKey {
                        task_id: 0,
                        execute_at: now,
                    },
                )
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();

            if keys_to_remove.len() == 0 {
                return result;
            }
            result.reserve(keys_to_remove.len());
            for key in keys_to_remove.iter() {
                tasks.remove(&key).map(|value| result.push(value));
            }
            keys_to_remove
        };
        {
            let mut id_to_tasks = self.id_to_tasks.write().unwrap();
            for key in keys_to_remove {
                id_to_tasks.remove(&key.task_id);
            }
        }
        result
    }
}

#[test]
fn test_timer() {
    use std::time::Duration;
    let timer = Timer::new();
    let now = Instant::now();
    let task_id = timer.timeout_at(now, "task1");
    assert_eq!(task_id, 1);
    assert_eq!(timer.cancel(task_id), Some("task1"));
    assert_eq!(timer.cancel(task_id), None);

    timer.timeout_at(now, "task2");
    let must_hass_task_2 = timer.poll(now + Duration::from_secs(1));
    assert_eq!(must_hass_task_2.len(), 1);

    timer.timeout_at(now + Duration::from_millis(1001), "task3");
    let non_tasks = timer.poll(now + Duration::from_secs(1));
    assert_eq!(non_tasks.len(), 0);
    assert_eq!(timer.len(), 1);
}
