use std::sync::{
    atomic::{AtomicI64, AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;

use crate::{
    handler::{WalletBalanceSync, WalletBalanceSyncRun},
    Error,
};

use super::*;

struct FixedClock(AtomicI64);

impl UnixClock for FixedClock {
    fn now(&self) -> crate::Result<i64> {
        let value = self.0.load(Ordering::SeqCst);
        if value < 0 {
            Err(Error::Settlement("test clock failed".to_owned()))
        } else {
            Ok(value)
        }
    }
}

struct CapturingSynchronizer {
    calls: AtomicUsize,
    observed_at: AtomicI64,
}

#[async_trait]
impl WalletBalanceSync for CapturingSynchronizer {
    async fn sync(&self, observed_at: i64) -> crate::Result<WalletBalanceSyncRun> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.observed_at.store(observed_at, Ordering::SeqCst);
        Ok(WalletBalanceSyncRun {
            scanned: 2,
            observed: 2,
            skipped: 0,
            failed: 0,
        })
    }
}

#[tokio::test]
async fn delegates_one_cron_tick_with_trusted_time() {
    let synchronizer = Arc::new(CapturingSynchronizer {
        calls: AtomicUsize::new(0),
        observed_at: AtomicI64::new(0),
    });
    let job = WalletBalanceSyncJob::with_clock(synchronizer.clone(), Arc::new(FixedClock(AtomicI64::new(1_234))));

    let run = job.run_once().await.unwrap();

    assert_eq!(run.observed, 2);
    assert_eq!(synchronizer.calls.load(Ordering::SeqCst), 1);
    assert_eq!(synchronizer.observed_at.load(Ordering::SeqCst), 1_234);
}

#[tokio::test]
async fn does_not_call_component_when_clock_fails() {
    let synchronizer = Arc::new(CapturingSynchronizer {
        calls: AtomicUsize::new(0),
        observed_at: AtomicI64::new(0),
    });
    let job = WalletBalanceSyncJob::with_clock(synchronizer.clone(), Arc::new(FixedClock(AtomicI64::new(-1))));

    assert!(job.run_once().await.is_err());
    assert_eq!(synchronizer.calls.load(Ordering::SeqCst), 0);
}
