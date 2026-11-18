use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

/// 発行されたイベントの種別とプロパティ。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusEvent {
    /// イベント種別を表す文字列。
    pub event_type: String,
    /// イベントに付随する JSON プロパティ。
    pub properties: serde_json::Value,
}

/// イベント種別を定義し、購読・発行時の識別に使うハンドル。
pub struct BusEventDef {
    /// イベント種別の静的文字列。
    pub event_type: &'static str,
}

impl BusEventDef {
    /// 指定したイベント種別で定義を作成する（const コンテキストで利用可能）。
    pub const fn new(event_type: &'static str) -> Self {
        Self { event_type }
    }
}

type BoxedCallback = Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>;

struct Subscription {
    id: u64,
    callback: BoxedCallback,
}

/// イベントの発行とコールバック／チャネルによる購読を仲介するバス。
pub struct Bus {
    next_id: Arc<RwLock<u64>>,
    subscribers: Arc<RwLock<HashMap<String, Vec<Subscription>>>>,
    wildcard_subscribers: Arc<RwLock<Vec<Subscription>>>,
    tx: broadcast::Sender<BusEvent>,
}

impl Bus {
    /// 空のイベントバスを生成する。
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self {
            next_id: Arc::new(RwLock::new(0)),
            subscribers: Arc::new(RwLock::new(HashMap::new())),
            wildcard_subscribers: Arc::new(RwLock::new(Vec::new())),
            tx,
        }
    }

    /// イベントを発行し、該当するコールバックとチャネル購読者へ届ける。
    pub async fn publish(&self, def: &BusEventDef, properties: serde_json::Value) {
        tracing::info!(event_type = def.event_type, "publishing event");

        let event = BusEvent {
            event_type: def.event_type.to_string(),
            properties: properties.clone(),
        };

        let _ = self.tx.send(event.clone());

        // コールバック実行前に read guard を解放し、コールバックからの
        // subscribe/unsubscribe による write lock 取得でデッドロックしないようにする。
        let callbacks: Vec<BoxedCallback> = {
            let subscribers = self.subscribers.read().await;
            subscribers
                .get(def.event_type)
                .map(|subs| subs.iter().map(|s| Arc::clone(&s.callback)).collect())
                .unwrap_or_default()
        };
        for callback in callbacks {
            callback(def.event_type, properties.clone());
        }

        let wildcard: Vec<BoxedCallback> = {
            let wildcard = self.wildcard_subscribers.read().await;
            wildcard.iter().map(|s| Arc::clone(&s.callback)).collect()
        };
        for callback in wildcard {
            callback(def.event_type, properties.clone());
        }
    }

    /// 指定したイベント種別にコールバックを登録し、登録 ID を返す。
    pub async fn subscribe<F>(&self, def: &BusEventDef, callback: F) -> u64
    where
        F: Fn(&str, serde_json::Value) + Send + Sync + 'static,
    {
        let id = {
            let mut next = self.next_id.write().await;
            *next += 1;
            *next
        };

        let mut subscribers = self.subscribers.write().await;
        let subs = subscribers
            .entry(def.event_type.to_string())
            .or_insert_with(Vec::new);
        subs.push(Subscription {
            id,
            callback: Arc::new(callback),
        });

        id
    }

    /// 指定したイベント種別から、登録 ID でコールバックを解除する。
    pub async fn unsubscribe(&self, event_type: &str, id: u64) {
        let mut subscribers = self.subscribers.write().await;
        if let Some(subs) = subscribers.get_mut(event_type) {
            subs.retain(|s| s.id != id);
        }
    }

    /// すべてのイベント種別を受け取るコールバックを登録し、登録 ID を返す。
    pub async fn subscribe_all<F>(&self, callback: F) -> u64
    where
        F: Fn(&str, serde_json::Value) + Send + Sync + 'static,
    {
        let id = {
            let mut next = self.next_id.write().await;
            *next += 1;
            *next
        };

        let mut wildcard = self.wildcard_subscribers.write().await;
        wildcard.push(Subscription {
            id,
            callback: Arc::new(callback),
        });

        id
    }

    /// 登録 ID で、全イベント購読のコールバックを解除する。
    pub async fn unsubscribe_all(&self, id: u64) {
        let mut wildcard = self.wildcard_subscribers.write().await;
        wildcard.retain(|s| s.id != id);
    }

    /// 発行されたイベントを受信する broadcast レシーバーを作成する。
    pub fn subscribe_channel(&self) -> broadcast::Receiver<BusEvent> {
        self.tx.subscribe()
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

/// 指定したイベント種別の [`BusEventDef`] を生成する。
pub fn define_event(event_type: &'static str) -> BusEventDef {
    BusEventDef::new(event_type)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    #[test]
    fn bus_event_def_new_and_define_event_keep_event_type() {
        let def = BusEventDef::new("new.event");
        assert_eq!(def.event_type, "new.event");

        let def = define_event("defined.event");
        assert_eq!(def.event_type, "defined.event");
    }

    #[tokio::test]
    async fn publish_invokes_matching_callback_with_event_type_and_properties() {
        let bus = Bus::new();
        let def = define_event("demo.event");

        let calls: Arc<Mutex<Vec<(String, serde_json::Value)>>> = Arc::new(Mutex::new(Vec::new()));
        let calls_in_cb = Arc::clone(&calls);
        bus.subscribe(&def, move |event_type, properties| {
            calls_in_cb
                .lock()
                .unwrap()
                .push((event_type.to_string(), properties));
        })
        .await;

        bus.publish(&def, json!({"value": 42})).await;

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "demo.event");
        assert_eq!(recorded[0].1, json!({"value": 42}));
    }

    #[tokio::test]
    async fn publish_does_not_invoke_callbacks_for_other_event_types() {
        let bus = Bus::new();
        let target = define_event("target.event");
        let other = define_event("other.event");

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_in_cb = Arc::clone(&calls);
        bus.subscribe(&target, move |_, _| {
            calls_in_cb.fetch_add(1, Ordering::SeqCst);
        })
        .await;

        bus.publish(&other, json!({})).await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        bus.publish(&target, json!({})).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unsubscribe_stops_callback_delivery() {
        let bus = Bus::new();
        let def = define_event("unsub.event");

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_in_cb = Arc::clone(&calls);
        let id = bus
            .subscribe(&def, move |_, _| {
                calls_in_cb.fetch_add(1, Ordering::SeqCst);
            })
            .await;
        assert_eq!(id, 1);

        bus.publish(&def, json!({})).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        bus.unsubscribe(def.event_type, id).await;
        bus.publish(&def, json!({})).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn subscribe_channel_receives_published_event() {
        let bus = Bus::new();
        let def = define_event("channel.event");
        let mut rx = bus.subscribe_channel();

        bus.publish(&def, json!({"n": 7})).await;

        let event = rx
            .try_recv()
            .expect("channel should receive the published event");
        assert_eq!(event.event_type, "channel.event");
        assert_eq!(event.properties, json!({"n": 7}));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn subscribe_all_receives_every_event_type_and_unsubscribe_all_stops_it() {
        let bus = Bus::new();
        let first = define_event("first.event");
        let second = define_event("second.event");

        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_in_cb = Arc::clone(&seen);
        let id = bus
            .subscribe_all(move |event_type, _| {
                seen_in_cb.lock().unwrap().push(event_type.to_string());
            })
            .await;
        assert_eq!(id, 1);

        bus.publish(&first, json!({})).await;
        bus.publish(&second, json!({})).await;
        {
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 2);
            assert_eq!(seen[0], "first.event");
            assert_eq!(seen[1], "second.event");
        }

        bus.unsubscribe_all(id).await;
        bus.publish(&first, json!({})).await;
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn targeted_and_wildcard_callbacks_both_receive_the_event() {
        let bus = Bus::new();
        let def = define_event("both.event");

        let targeted = Arc::new(AtomicUsize::new(0));
        let targeted_cb = Arc::clone(&targeted);
        let wildcard = Arc::new(AtomicUsize::new(0));
        let wildcard_cb = Arc::clone(&wildcard);

        bus.subscribe(&def, move |_, _| {
            targeted_cb.fetch_add(1, Ordering::SeqCst);
        })
        .await;
        bus.subscribe_all(move |_, _| {
            wildcard_cb.fetch_add(1, Ordering::SeqCst);
        })
        .await;

        bus.publish(&def, json!({})).await;

        assert_eq!(targeted.load(Ordering::SeqCst), 1);
        assert_eq!(wildcard.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn callback_can_subscribe_without_deadlocking_publish() {
        // L-01: publish must release its read guard before invoking callbacks;
        // otherwise a callback that subscribes (write lock) would deadlock.
        let bus = Arc::new(Bus::new());
        let def = define_event("reentrant.event");

        let spawned: Arc<Mutex<Vec<tokio::task::JoinHandle<u64>>>> =
            Arc::new(Mutex::new(Vec::new()));
        let spawned_in_cb = Arc::clone(&spawned);
        let bus_in_cb = Arc::clone(&bus);

        bus.subscribe(&def, move |_, _| {
            let bus = Arc::clone(&bus_in_cb);
            let handle = tokio::spawn(async move {
                bus.subscribe(&BusEventDef::new("nested.event"), |_, _| {})
                    .await
            });
            spawned_in_cb.lock().unwrap().push(handle);
        })
        .await;

        bus.publish(&def, json!({})).await;

        let handles: Vec<_> = spawned.lock().unwrap().drain(..).collect();
        assert_eq!(handles.len(), 1);
        for handle in handles {
            let id = tokio::time::timeout(Duration::from_secs(2), handle)
                .await
                .expect("subscription from inside a callback deadlocked")
                .expect("spawned subscription task failed");
            assert!(id > 0);
        }
    }
}
