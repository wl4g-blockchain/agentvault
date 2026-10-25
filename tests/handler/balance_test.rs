use super::*;

#[test]
fn parses_balance_without_truncating_large_values() {
    assert_eq!(parse_hex_amount("0x0").unwrap(), Amount::ZERO);
    assert_eq!(parse_hex_amount("0x3b9aca00").unwrap(), Amount::new(1_000_000_000));
    assert_eq!(
        parse_hex_amount("0x100000000000000000000000000000000").unwrap(),
        Amount::new(u128::MAX)
    );
    assert!(parse_hex_amount("1").is_err());
}

#[test]
fn requires_https_except_explicit_loopback() {
    assert!(EvmRpcBalanceSource::new(
        "http://example.com/",
        BTreeMap::new(),
        Duration::from_secs(1),
        1024,
        Amount::ZERO,
        true,
    )
    .is_err());
    assert!(EvmRpcBalanceSource::new(
        "http://127.0.0.1:8545/",
        BTreeMap::new(),
        Duration::from_secs(1),
        1024,
        Amount::ZERO,
        true,
    )
    .is_ok());
}

struct FixedClock(std::sync::atomic::AtomicI64);

impl UnixClock for FixedClock {
    fn now(&self) -> crate::Result<i64> {
        let value = self.0.load(std::sync::atomic::Ordering::SeqCst);
        if value < 0 {
            Err(Error::Settlement("test clock failed".to_owned()))
        } else {
            Ok(value)
        }
    }
}

struct CapturingSynchronizer {
    calls: std::sync::atomic::AtomicUsize,
    observed_at: std::sync::atomic::AtomicI64,
}

#[async_trait]
impl WalletBalanceSync for CapturingSynchronizer {
    async fn sync(&self, observed_at: i64) -> crate::Result<WalletBalanceSyncRun> {
        use std::sync::atomic::Ordering;
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
async fn sync_job_delegates_one_tick_with_trusted_time() {
    use std::sync::{atomic::Ordering, Arc};

    let synchronizer = Arc::new(CapturingSynchronizer {
        calls: std::sync::atomic::AtomicUsize::new(0),
        observed_at: std::sync::atomic::AtomicI64::new(0),
    });
    let job = WalletBalanceSyncJob::with_clock(
        synchronizer.clone(),
        Arc::new(FixedClock(std::sync::atomic::AtomicI64::new(1_234))),
    );

    let run = job.run_once().await.unwrap();
    assert_eq!(run.observed, 2);
    assert_eq!(synchronizer.calls.load(Ordering::SeqCst), 1);
    assert_eq!(synchronizer.observed_at.load(Ordering::SeqCst), 1_234);
}

#[tokio::test]
async fn sync_job_does_not_call_component_when_clock_fails() {
    use std::sync::{atomic::Ordering, Arc};

    let synchronizer = Arc::new(CapturingSynchronizer {
        calls: std::sync::atomic::AtomicUsize::new(0),
        observed_at: std::sync::atomic::AtomicI64::new(0),
    });
    let job = WalletBalanceSyncJob::with_clock(
        synchronizer.clone(),
        Arc::new(FixedClock(std::sync::atomic::AtomicI64::new(-1))),
    );

    assert!(job.run_once().await.is_err());
    assert_eq!(synchronizer.calls.load(Ordering::SeqCst), 0);
}
