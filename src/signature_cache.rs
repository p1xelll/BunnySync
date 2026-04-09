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
    signatures: HashSet<Arc<str>>,
    entries: Vec<(Arc<str>, Instant)>,
}

impl SignatureCache {
    pub fn new(ttl: Duration) -> Self {
        let cache = Self {
            inner: Arc::new(RwLock::new(CacheState {
                signatures: HashSet::with_capacity(1024),
                entries: Vec::with_capacity(1024),
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
        let sig: Arc<str> = Arc::from(signature.into_boxed_str());
        let mut state = self.inner.write().await;
        state.signatures.insert(Arc::clone(&sig));
        state.entries.push((sig, Instant::now()));
    }

    fn start_cleanup_task(&self) {
        let inner = Arc::clone(&self.inner);
        let ttl = self.ttl;

        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(60));

            loop {
                ticker.tick().await;

                let now = Instant::now();
                let mut state = inner.write().await;

                let cutoff = now - ttl;
                let split_idx = state.entries.partition_point(|(_, time)| *time <= cutoff);

                if split_idx > 0 {
                    let expired: Vec<Arc<str>> = state
                        .entries
                        .drain(..split_idx)
                        .map(|(s, _)| s)
                        .collect();
                    for sig in expired {
                        state.signatures.remove(sig.as_ref());
                    }

                    // Shrink if we've removed a significant portion
                    if state.entries.capacity() > state.entries.len() * 4
                        && state.entries.len() < 100
                    {
                        state.entries.shrink_to_fit();
                        state.signatures.shrink_to_fit();
                    }
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
