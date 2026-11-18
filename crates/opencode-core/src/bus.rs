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
