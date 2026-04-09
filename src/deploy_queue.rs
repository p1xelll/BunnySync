use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

#[derive(Clone)]
pub struct DeployQueue {
    inner: Arc<Mutex<HashMap<String, Arc<Semaphore>>>>,
}

impl DeployQueue {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn acquire(&self, project_id: &str) -> Option<OwnedSemaphorePermit> {
        let mut queues = self.inner.lock().await;

        let semaphore = queues
            .entry(project_id.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(1)))
            .clone();

        drop(queues);

        semaphore.try_acquire_owned().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::sleep;

    #[tokio::test]
    async fn test_deploy_queue_single_project() {
        let queue = DeployQueue::new();

        let permit = queue.acquire("project1").await;
        assert!(permit.is_some());

        let permit2 = queue.acquire("project1").await;
        assert!(permit2.is_none());

        drop(permit);

        sleep(Duration::from_millis(10)).await;

        let permit3 = queue.acquire("project1").await;
        assert!(permit3.is_some());
    }

    #[tokio::test]
    async fn test_deploy_queue_multiple_projects() {
        let queue = DeployQueue::new();

        let permit1 = queue.acquire("project1").await;
        assert!(permit1.is_some());

        let permit2 = queue.acquire("project2").await;
        assert!(permit2.is_some());

        drop(permit1);
        drop(permit2);
    }
}
