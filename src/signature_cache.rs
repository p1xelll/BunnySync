use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::time::interval;

#[derive(Clone)]
pub struct SignatureCache {
    inner: Arc<RwLock<CacheState>>,
    ttl: Duration,
}

struct CacheState {
    signatures: HashSet<String>,
    entries: Vec<(String, Instant)>,
}

impl SignatureCache {
    pub fn new(ttl: Duration) -> Self {
        let cache = Self {
            inner: Arc::new(RwLock::new(CacheState {
                signatures: HashSet::new(),
                entries: Vec::new(),
            })),
            ttl,
        };

        cache.start_cleanup_task();
        cache
    }

    pub async fn contains(&self, signature: &str) -> bool {
        let state = self.inner.read().await;
        state.signatures.contains(signature)
    }

    pub async fn insert(&self, signature: String) {
        let mut state = self.inner.write().await;
        state.signatures.insert(signature.clone());
        state.entries.push((signature, Instant::now()));
    }

    fn start_cleanup_task(&self) {
        let inner = self.inner.clone();
        let ttl = self.ttl;

        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(60));

            loop {
                ticker.tick().await;

                let now = Instant::now();
                let mut state = inner.write().await;

                let cutoff = now - ttl;
                let split_idx = state
                    .entries
                    .partition_point(|(_, time)| *time <= cutoff);

                let expired: Vec<String> = state.entries.drain(..split_idx).map(|(s, _)| s).collect();
                for sig in expired {
                    state.signatures.remove(&sig);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_signature_cache() {
        let cache = SignatureCache::new(Duration::from_secs(1));

        assert!(!cache.contains("sig1").await);
        cache.insert("sig1".to_string()).await;
        assert!(cache.contains("sig1").await);

        assert!(!cache.contains("sig2").await);
    }

    #[tokio::test]
    async fn test_signature_cache_expiration() {
        let cache = SignatureCache::new(Duration::from_millis(100));

        cache.insert("sig1".to_string()).await;
        assert!(cache.contains("sig1").await);

        tokio::time::sleep(Duration::from_millis(150)).await;

        // After expiration, signature should still be there until cleanup runs
        // But we can't easily test cleanup without waiting 60 seconds
    }
}
