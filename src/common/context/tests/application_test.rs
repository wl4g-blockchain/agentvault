use anyhow::bail;
use anyhow::Result;
use async_trait::async_trait;
use common_context::ApplicationContext;
use common_context::IProvider;
use std::future::poll_fn;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex;
use std::task::Poll;
use std::time::Duration;
use tokio::sync::Barrier;
use tokio::sync::Notify;
use tokio::time::timeout;

const DEADLINE: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Eq, PartialEq)]
struct IngestedRecord {
    source_id: String,
    text: String,
}

trait ITestIngestor: IProvider {
    fn ingest(&self, source_id: &str, content: &str) -> IngestedRecord;
}

trait ITestIngestorMetrics: IProvider {
    fn ingested_count(&self) -> usize;
}

#[derive(Default)]
struct WebTestIngestor {
    records: Mutex<Vec<IngestedRecord>>,
    inits: AtomicUsize,
    closes: AtomicUsize,
    init_fail_once: AtomicBool,
    close_fail_once: AtomicBool,
    barrier: Option<Arc<Barrier>>,
    label: &'static str,
    order: Option<Arc<Mutex<Vec<&'static str>>>>,
}

impl WebTestIngestor {
    fn register(self: &Arc<Self>, ctx: &ApplicationContext, instance: &str) -> Result<()> {
        let value = Arc::clone(self);
        ctx.register::<Self, _>(instance, move || {
            let value = Arc::clone(&value);
            async move { Ok(value) }
        })
    }
}

impl ITestIngestor for WebTestIngestor {
    fn ingest(&self, source_id: &str, content: &str) -> IngestedRecord {
        let record = IngestedRecord {
            source_id: source_id.to_owned(),
            text: content.split_whitespace().collect::<Vec<_>>().join(" "),
        };
        self.records.lock().unwrap().push(record.clone());
        record
    }
}

impl ITestIngestorMetrics for WebTestIngestor {
    fn ingested_count(&self) -> usize {
        self.records.lock().unwrap().len()
    }
}

#[async_trait]
impl IProvider for WebTestIngestor {
    async fn init(&self) -> Result<()> {
        self.inits.fetch_add(1, Ordering::SeqCst);
        if self.init_fail_once.swap(false, Ordering::SeqCst) {
            bail!("init failed");
        }
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        if self.close_fail_once.swap(false, Ordering::SeqCst) {
            bail!("close failed");
        }
        if let Some(barrier) = &self.barrier {
            barrier.wait().await;
        }
        self.records.lock().unwrap().clear();
        if let Some(order) = &self.order {
            order.lock().unwrap().push(self.label);
        }
        Ok(())
    }
}

#[tokio::test]
async fn lazy_singleton_and_context_lifecycle() {
    let ctx = Arc::new(ApplicationContext::new());
    let constructed = Arc::new(AtomicUsize::new(0));
    let ingestor = Arc::new(WebTestIngestor::default());
    let (made, value) = (Arc::clone(&constructed), Arc::clone(&ingestor));
    ctx.register::<WebTestIngestor, _>("a", move || {
        let (made, value) = (Arc::clone(&made), Arc::clone(&value));
        async move {
            made.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            Ok(value)
        }
    })
    .unwrap();
    assert_eq!(constructed.load(Ordering::SeqCst), 0);
    assert!(ctx.get::<WebTestIngestor>("a").await.is_err());
    ctx.init().await.unwrap();
    ctx.init().await.unwrap();

    let mut jobs = Vec::new();
    for _ in 0..16 {
        let ctx = Arc::clone(&ctx);
        jobs.push(tokio::spawn(
            async move { ctx.get::<WebTestIngestor>("a").await.unwrap() },
        ));
    }
    for job in jobs {
        let value = timeout(DEADLINE, job).await.unwrap().unwrap();
        assert!(Arc::ptr_eq(&value, &ingestor));
    }
    assert_eq!(constructed.load(Ordering::SeqCst), 1);
    assert_eq!(ingestor.inits.load(Ordering::SeqCst), 1);
    let record = ingestor.ingest("https://example.test/news/42", "  Market   update\n published  ");
    assert_eq!(record.source_id, "https://example.test/news/42");
    assert_eq!(record.text, "Market update published");
    assert_eq!(ingestor.ingested_count(), 1);
    ctx.close().await.unwrap();
    ctx.close().await.unwrap();
    ctx.shutdown().await.unwrap();
    assert_eq!(ingestor.closes.load(Ordering::SeqCst), 1);
    assert_eq!(ingestor.ingested_count(), 0);
    assert!(ctx.get::<WebTestIngestor>("a").await.is_err());
    assert!(ctx.init().await.is_err());
    assert!(ingestor.register(&ctx, "b").is_err());
}

#[tokio::test]
async fn registration_validates_ids_duplicates_and_missing_keys() {
    let ctx = ApplicationContext::new();
    let ingestor = Arc::new(WebTestIngestor::default());
    for instance in ["", " \t", "\n"] {
        assert!(ingestor.register(&ctx, instance).is_err());
    }
    ingestor.register(&ctx, "a").unwrap();
    assert!(ingestor.register(&ctx, "a").is_err());
    ctx.init().await.unwrap();
    assert!(ctx.get::<WebTestIngestor>("missing").await.is_err());
    ctx.close_for::<WebTestIngestor>("missing").await.unwrap();
    ctx.close().await.unwrap();
    assert_eq!(ingestor.inits.load(Ordering::SeqCst), 0);
    assert_eq!(ingestor.closes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn runtime_registration_separates_instances() {
    let ctx = ApplicationContext::new();
    ctx.init().await.unwrap();
    let first = Arc::new(WebTestIngestor::default());
    let second = Arc::new(WebTestIngestor::default());
    let third = Arc::new(WebTestIngestor::default());
    first.register(&ctx, "a").unwrap();
    second.register(&ctx, "b").unwrap();
    third.register(&ctx, "c").unwrap();
    for (instance, expected) in [("a", &first), ("b", &second), ("c", &third)] {
        let ingestor = ctx.get::<WebTestIngestor>(instance).await.unwrap();
        assert!(Arc::ptr_eq(&ingestor, expected));
        let source_id = format!("https://example.test/{instance}");
        let record = ingestor.ingest(&source_id, "  Latest   news  ");
        assert_eq!(record.source_id, source_id);
        assert_eq!(record.text, "Latest news");
    }
    ctx.close().await.unwrap();
    for value in [first, second, third] {
        assert_eq!(value.inits.load(Ordering::SeqCst), 1);
        assert_eq!(value.closes.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn independent_keys_construct_concurrently() {
    let ctx = ApplicationContext::new();
    let barrier = Arc::new(Barrier::new(2));
    for instance in ["a", "b"] {
        let barrier = Arc::clone(&barrier);
        ctx.register::<WebTestIngestor, _>(instance, move || {
            let barrier = Arc::clone(&barrier);
            async move {
                barrier.wait().await;
                Ok(Arc::new(WebTestIngestor::default()))
            }
        })
        .unwrap();
    }
    ctx.init().await.unwrap();
    let (first, second) = timeout(DEADLINE, async {
        tokio::join!(ctx.get::<WebTestIngestor>("a"), ctx.get::<WebTestIngestor>("b"))
    })
    .await
    .unwrap();
    let (first, second) = (first.unwrap(), second.unwrap());
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(first.inits.load(Ordering::SeqCst), 1);
    assert_eq!(second.inits.load(Ordering::SeqCst), 1);
    ctx.close().await.unwrap();
}

#[tokio::test]
async fn closing_unused_registrations_never_constructs_them() {
    let ctx = ApplicationContext::new();
    for instance in ["individual", "global"] {
        ctx.register::<WebTestIngestor, _>(instance, || async { panic!("unused factories must not run") })
            .unwrap();
    }
    ctx.close_for::<WebTestIngestor>("individual").await.unwrap();
    ctx.close().await.unwrap();
    ctx.close().await.unwrap();
    assert!(ctx.init().await.is_err());
}

#[tokio::test]
async fn shutdown_closes_each_phase_concurrently_in_descending_order() {
    let ctx = ApplicationContext::new();
    let order = Arc::new(Mutex::new(Vec::new()));
    let barrier = Arc::new(Barrier::new(2));
    for (name, phase) in [("a", 10), ("b", 10), ("c", 0), ("d", -10)] {
        let value = Arc::new(WebTestIngestor {
            barrier: (phase == 10).then(|| Arc::clone(&barrier)),
            label: name,
            order: Some(Arc::clone(&order)),
            ..WebTestIngestor::default()
        });
        ctx.register_with_shutdown_phase::<WebTestIngestor, _>(name, phase, move || {
            let value = Arc::clone(&value);
            async move { Ok(value) }
        })
        .unwrap();
    }
    ctx.init().await.unwrap();
    for name in ["a", "b", "c", "d"] {
        ctx.get::<WebTestIngestor>(name).await.unwrap();
    }
    timeout(DEADLINE, ctx.close()).await.unwrap().unwrap();
    let history = order.lock().unwrap();
    assert_eq!(history.len(), 4);
    assert!(history[..2].contains(&"a") && history[..2].contains(&"b"));
    assert_eq!(&history[2..], &["c", "d"]);
}

#[tokio::test]
async fn failed_shutdown_retains_only_failures_and_can_be_retried() {
    let ctx = ApplicationContext::new();
    let first = Arc::new(WebTestIngestor::default());
    let second = Arc::new(WebTestIngestor::default());
    let successful = Arc::new(WebTestIngestor::default());
    for (name, value) in [("first", &first), ("second", &second), ("successful", &successful)] {
        value.register(&ctx, name).unwrap();
    }
    first.close_fail_once.store(true, Ordering::SeqCst);
    second.close_fail_once.store(true, Ordering::SeqCst);
    ctx.init().await.unwrap();
    for name in ["first", "second", "successful"] {
        ctx.get::<WebTestIngestor>(name).await.unwrap();
    }
    let error = format!("{:#}", ctx.close().await.unwrap_err());
    assert!(error.contains("first") && error.contains("second"));
    assert!(ctx.get::<WebTestIngestor>("first").await.is_err());
    assert!(successful.register(&ctx, "new").is_err());
    assert!(ctx.init().await.is_err());
    ctx.shutdown().await.unwrap();
    assert_eq!(first.closes.load(Ordering::SeqCst), 2);
    assert_eq!(second.closes.load(Ordering::SeqCst), 2);
    assert_eq!(successful.closes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn constructor_failure_can_be_retried() {
    let ctx = ApplicationContext::new();
    let attempts = Arc::new(AtomicUsize::new(0));
    let ingestor = Arc::new(WebTestIngestor::default());
    let (calls, value) = (Arc::clone(&attempts), Arc::clone(&ingestor));
    ctx.register::<WebTestIngestor, _>("a", move || {
        let (calls, value) = (Arc::clone(&calls), Arc::clone(&value));
        async move {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                bail!("constructor failed");
            }
            Ok(value)
        }
    })
    .unwrap();
    ctx.init().await.unwrap();
    assert!(ctx.get::<WebTestIngestor>("a").await.is_err());
    assert_eq!(ingestor.inits.load(Ordering::SeqCst), 0);
    assert_eq!(ingestor.closes.load(Ordering::SeqCst), 0);
    assert!(Arc::ptr_eq(&ctx.get::<WebTestIngestor>("a").await.unwrap(), &ingestor));
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    ctx.close().await.unwrap();
}

#[tokio::test]
async fn failed_initialization_is_cleaned_before_retry() {
    let ctx = ApplicationContext::new();
    let ingestor = Arc::new(WebTestIngestor::default());
    ingestor.init_fail_once.store(true, Ordering::SeqCst);
    ingestor.register(&ctx, "a").unwrap();
    ctx.init().await.unwrap();
    assert!(ctx.get::<WebTestIngestor>("a").await.is_err());
    assert_eq!(ingestor.closes.load(Ordering::SeqCst), 1);
    ctx.get::<WebTestIngestor>("a").await.unwrap();
    assert_eq!(ingestor.inits.load(Ordering::SeqCst), 2);
    ctx.close().await.unwrap();
    assert_eq!(ingestor.closes.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn failed_initialization_cleanup_is_retained_until_cleanup_succeeds() {
    let ctx = ApplicationContext::new();
    let builds = Arc::new(AtomicUsize::new(0));
    let ingestor = Arc::new(WebTestIngestor::default());
    ingestor.init_fail_once.store(true, Ordering::SeqCst);
    ingestor.close_fail_once.store(true, Ordering::SeqCst);
    let (made, value) = (Arc::clone(&builds), Arc::clone(&ingestor));
    ctx.register::<WebTestIngestor, _>("a", move || {
        let (made, value) = (Arc::clone(&made), Arc::clone(&value));
        async move {
            made.fetch_add(1, Ordering::SeqCst);
            Ok(value)
        }
    })
    .unwrap();
    ctx.init().await.unwrap();
    let error = ctx.get::<WebTestIngestor>("a").await.err().unwrap();
    let message = format!("{error:#}");
    assert!(message.contains("init failed") && message.contains("close failed"));
    ingestor.close_fail_once.store(true, Ordering::SeqCst);
    assert!(ctx.get::<WebTestIngestor>("a").await.is_err());
    assert_eq!(builds.load(Ordering::SeqCst), 1);
    assert_eq!(ingestor.inits.load(Ordering::SeqCst), 1);
    ctx.get::<WebTestIngestor>("a").await.unwrap();
    assert_eq!(builds.load(Ordering::SeqCst), 2);
    assert_eq!(ingestor.closes.load(Ordering::SeqCst), 3);
    ctx.close().await.unwrap();
    assert_eq!(ingestor.closes.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn failed_initialization_cleanup_can_be_closed_directly() {
    for individual in [true, false] {
        let ctx = ApplicationContext::new();
        let ingestor = Arc::new(WebTestIngestor::default());
        let builds = Arc::new(AtomicUsize::new(0));
        ingestor.init_fail_once.store(true, Ordering::SeqCst);
        ingestor.close_fail_once.store(true, Ordering::SeqCst);
        let (value, calls) = (Arc::clone(&ingestor), Arc::clone(&builds));
        ctx.register::<WebTestIngestor, _>("a", move || {
            let (value, calls) = (Arc::clone(&value), Arc::clone(&calls));
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(value)
            }
        })
        .unwrap();
        ctx.init().await.unwrap();
        assert!(ctx.get::<WebTestIngestor>("a").await.is_err());
        assert_eq!(ingestor.closes.load(Ordering::SeqCst), 1);
        if individual {
            timeout(DEADLINE, ctx.close_for::<WebTestIngestor>("a"))
                .await
                .unwrap()
                .unwrap();
            assert!(ctx.get::<WebTestIngestor>("a").await.is_err());
        }
        timeout(DEADLINE, ctx.close()).await.unwrap().unwrap();
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        assert_eq!(ingestor.inits.load(Ordering::SeqCst), 1);
        assert_eq!(ingestor.closes.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn domain_types_are_isolated_and_individual_close_allows_replacement() {
    let ctx = ApplicationContext::new();
    let first = Arc::new(WebTestIngestor::default());
    let second = Arc::new(WebTestIngestor::default());
    let value = Arc::clone(&first);
    ctx.register::<dyn ITestIngestor, _>("default", move || {
        let value = Arc::clone(&value);
        async move { Ok(value as Arc<dyn ITestIngestor>) }
    })
    .unwrap();
    let value = Arc::clone(&second);
    ctx.register::<dyn ITestIngestorMetrics, _>("default", move || {
        let value = Arc::clone(&value);
        async move { Ok(value as Arc<dyn ITestIngestorMetrics>) }
    })
    .unwrap();
    ctx.init().await.unwrap();
    let ingestor = ctx.get::<dyn ITestIngestor>("default").await.unwrap();
    let record = ingestor.ingest("https://example.test/news/42", "  Market   update  ");
    assert_eq!(record.text, "Market update");
    second.ingest("https://example.test/news/43", "Another update");
    let metrics = ctx.get::<dyn ITestIngestorMetrics>("default").await.unwrap();
    assert_eq!(metrics.ingested_count(), 1);
    ctx.close_for::<dyn ITestIngestor>("default").await.unwrap();
    assert_eq!(first.closes.load(Ordering::SeqCst), 1);
    assert!(ctx.get::<dyn ITestIngestor>("default").await.is_err());
    assert!(ctx.get::<dyn ITestIngestorMetrics>("default").await.is_ok());
    assert_eq!(metrics.ingested_count(), 1);
    let replacement = Arc::new(WebTestIngestor::default());
    let value = Arc::clone(&replacement);
    ctx.register::<dyn ITestIngestor, _>("default", move || {
        let value = Arc::clone(&value);
        async move { Ok(value as Arc<dyn ITestIngestor>) }
    })
    .unwrap();
    ctx.get::<dyn ITestIngestor>("default").await.unwrap();
    ctx.close().await.unwrap();
    assert_eq!(first.closes.load(Ordering::SeqCst), 1);
    assert_eq!(second.closes.load(Ordering::SeqCst), 1);
    assert_eq!(replacement.closes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn failed_individual_close_blocks_get_and_replacement_until_retry() {
    let ctx = ApplicationContext::new();
    let ingestor = Arc::new(WebTestIngestor::default());
    ingestor.register(&ctx, "a").unwrap();
    ctx.init().await.unwrap();
    ctx.get::<WebTestIngestor>("a").await.unwrap();
    ingestor.close_fail_once.store(true, Ordering::SeqCst);
    assert!(ctx.close_for::<WebTestIngestor>("a").await.is_err());
    assert!(ctx.get::<WebTestIngestor>("a").await.is_err());
    assert!(ingestor.register(&ctx, "a").is_err());
    ctx.close_for::<WebTestIngestor>("a").await.unwrap();
    ctx.close_for::<WebTestIngestor>("a").await.unwrap();
    assert_eq!(ingestor.closes.load(Ordering::SeqCst), 2);
    let replacement = Arc::new(WebTestIngestor::default());
    replacement.register(&ctx, "a").unwrap();
    ctx.get::<WebTestIngestor>("a").await.unwrap();
    ctx.close().await.unwrap();
}

// Models an ingestor whose source connection or worker cleanup can be interrupted.
struct WarmingTestIngestor {
    block_init: bool,
    block_close_once: AtomicBool,
    entered: Arc<Notify>,
    closes: Arc<AtomicUsize>,
}

#[async_trait]
impl IProvider for WarmingTestIngestor {
    async fn init(&self) -> Result<()> {
        if self.block_init {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        if self.block_close_once.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(())
    }
}

#[tokio::test]
async fn cancelled_initialization_is_cleaned_before_retry() {
    let ctx = Arc::new(ApplicationContext::new());
    let made = Arc::new(AtomicUsize::new(0));
    let closes = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Notify::new());
    let (count, stops, signal) = (Arc::clone(&made), Arc::clone(&closes), Arc::clone(&entered));
    ctx.register::<WarmingTestIngestor, _>("a", move || {
        let (count, stops, signal) = (Arc::clone(&count), Arc::clone(&stops), Arc::clone(&signal));
        async move {
            Ok(Arc::new(WarmingTestIngestor {
                block_init: count.fetch_add(1, Ordering::SeqCst) == 0,
                block_close_once: AtomicBool::new(false),
                entered: signal,
                closes: stops,
            }))
        }
    })
    .unwrap();
    ctx.init().await.unwrap();
    let cloned = Arc::clone(&ctx);
    let job = tokio::spawn(async move { cloned.get::<WarmingTestIngestor>("a").await });
    timeout(DEADLINE, entered.notified()).await.unwrap();
    job.abort();
    assert!(matches!(job.await, Err(error) if error.is_cancelled()));
    timeout(DEADLINE, ctx.get::<WarmingTestIngestor>("a"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(made.load(Ordering::SeqCst), 2);
    assert_eq!(closes.load(Ordering::SeqCst), 1);
    ctx.close().await.unwrap();
    assert_eq!(closes.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn cancelled_initialization_can_be_closed_directly() {
    for individual in [true, false] {
        let ctx = Arc::new(ApplicationContext::new());
        let entered = Arc::new(Notify::new());
        let closes = Arc::new(AtomicUsize::new(0));
        let builds = Arc::new(AtomicUsize::new(0));
        let value = Arc::new(WarmingTestIngestor {
            block_init: true,
            block_close_once: AtomicBool::new(false),
            entered: Arc::clone(&entered),
            closes: Arc::clone(&closes),
        });
        let calls = Arc::clone(&builds);
        ctx.register::<WarmingTestIngestor, _>("a", move || {
            let (value, calls) = (Arc::clone(&value), Arc::clone(&calls));
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(value)
            }
        })
        .unwrap();
        ctx.init().await.unwrap();
        let cloned = Arc::clone(&ctx);
        let job = tokio::spawn(async move { cloned.get::<WarmingTestIngestor>("a").await });
        timeout(DEADLINE, entered.notified()).await.unwrap();
        job.abort();
        assert!(matches!(job.await, Err(error) if error.is_cancelled()));
        assert_eq!(closes.load(Ordering::SeqCst), 0);
        if individual {
            timeout(DEADLINE, ctx.close_for::<WarmingTestIngestor>("a"))
                .await
                .unwrap()
                .unwrap();
            assert!(ctx.get::<WarmingTestIngestor>("a").await.is_err());
        }
        timeout(DEADLINE, ctx.close()).await.unwrap().unwrap();
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        assert_eq!(closes.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn cancelled_individual_close_preserves_instance_for_retry() {
    let ctx = Arc::new(ApplicationContext::new());
    let entered = Arc::new(Notify::new());
    let closes = Arc::new(AtomicUsize::new(0));
    let value = Arc::new(WarmingTestIngestor {
        block_init: false,
        block_close_once: AtomicBool::new(true),
        entered: Arc::clone(&entered),
        closes: Arc::clone(&closes),
    });
    ctx.register::<WarmingTestIngestor, _>("a", move || {
        let value = Arc::clone(&value);
        async move { Ok(value) }
    })
    .unwrap();
    ctx.init().await.unwrap();
    ctx.get::<WarmingTestIngestor>("a").await.unwrap();
    let cloned = Arc::clone(&ctx);
    let job = tokio::spawn(async move { cloned.close_for::<WarmingTestIngestor>("a").await });
    timeout(DEADLINE, entered.notified()).await.unwrap();
    job.abort();
    assert!(matches!(job.await, Err(error) if error.is_cancelled()));
    assert!(ctx.get::<WarmingTestIngestor>("a").await.is_err());
    timeout(DEADLINE, ctx.close_for::<WarmingTestIngestor>("a"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(closes.load(Ordering::SeqCst), 2);
    ctx.close().await.unwrap();
    assert_eq!(closes.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn cancelled_shutdown_preserves_instances_and_blocks_new_work() {
    let ctx = Arc::new(ApplicationContext::new());
    let entered = Arc::new(Notify::new());
    let closes = Arc::new(AtomicUsize::new(0));
    let value = Arc::new(WarmingTestIngestor {
        block_init: false,
        block_close_once: AtomicBool::new(true),
        entered: Arc::clone(&entered),
        closes: Arc::clone(&closes),
    });
    ctx.register::<WarmingTestIngestor, _>("a", move || {
        let value = Arc::clone(&value);
        async move { Ok(value) }
    })
    .unwrap();
    ctx.init().await.unwrap();
    ctx.get::<WarmingTestIngestor>("a").await.unwrap();
    let cloned = Arc::clone(&ctx);
    let job = tokio::spawn(async move { cloned.close().await });
    timeout(DEADLINE, entered.notified()).await.unwrap();
    job.abort();
    assert!(matches!(job.await, Err(error) if error.is_cancelled()));
    assert!(ctx.get::<WarmingTestIngestor>("a").await.is_err());
    assert!(Arc::new(WebTestIngestor::default()).register(&ctx, "b").is_err());
    timeout(DEADLINE, ctx.shutdown()).await.unwrap().unwrap();
    assert_eq!(closes.load(Ordering::SeqCst), 2);
}

#[derive(Default)]
// Models an ingestor waiting for active ingestion tasks to drain before closing.
struct DrainingTestIngestor {
    entered: Notify,
    release: Notify,
    closes: AtomicUsize,
}

impl DrainingTestIngestor {
    async fn poll_once<F: Future>(mut future: Pin<&mut F>) -> Poll<F::Output> {
        poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await
    }

    fn register(self: &Arc<Self>, ctx: &ApplicationContext) -> Result<()> {
        let value = Arc::clone(self);
        ctx.register::<Self, _>("a", move || {
            let value = Arc::clone(&value);
            async move { Ok(value) }
        })
    }
}

#[async_trait]
impl IProvider for DrainingTestIngestor {
    async fn close(&self) -> Result<()> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
}

#[tokio::test]
async fn stale_individual_close_cannot_remove_replacement() {
    let ctx = ApplicationContext::new();
    let original = Arc::new(DrainingTestIngestor::default());
    original.register(&ctx).unwrap();
    ctx.init().await.unwrap();
    ctx.get::<DrainingTestIngestor>("a").await.unwrap();
    let mut first = Box::pin(ctx.close_for::<DrainingTestIngestor>("a"));
    assert!(DrainingTestIngestor::poll_once(first.as_mut()).await.is_pending());
    let mut stale = Box::pin(ctx.close_for::<DrainingTestIngestor>("a"));
    assert!(DrainingTestIngestor::poll_once(stale.as_mut()).await.is_pending());
    assert_eq!(original.closes.load(Ordering::SeqCst), 1);
    original.release.notify_one();
    timeout(DEADLINE, first).await.unwrap().unwrap();

    let replacement = Arc::new(DrainingTestIngestor::default());
    replacement.register(&ctx).unwrap();
    assert!(Arc::ptr_eq(
        &ctx.get::<DrainingTestIngestor>("a").await.unwrap(),
        &replacement
    ));
    timeout(DEADLINE, stale).await.unwrap().unwrap();
    assert!(Arc::ptr_eq(
        &ctx.get::<DrainingTestIngestor>("a").await.unwrap(),
        &replacement
    ));
    replacement.release.notify_one();
    timeout(DEADLINE, ctx.close()).await.unwrap().unwrap();
    assert_eq!(original.closes.load(Ordering::SeqCst), 1);
    assert_eq!(replacement.closes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn queued_get_cannot_reconstruct_an_entry_closed_during_shutdown() {
    let ctx = ApplicationContext::new();
    let draining_ingestor = Arc::new(DrainingTestIngestor::default());
    let builds = Arc::new(AtomicUsize::new(0));
    let (value, calls) = (Arc::clone(&draining_ingestor), Arc::clone(&builds));
    ctx.register::<DrainingTestIngestor, _>("a", move || {
        let (value, calls) = (Arc::clone(&value), Arc::clone(&calls));
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(value)
        }
    })
    .unwrap();
    ctx.init().await.unwrap();
    ctx.get::<DrainingTestIngestor>("a").await.unwrap();
    let mut individual = Box::pin(ctx.close_for::<DrainingTestIngestor>("a"));
    assert!(DrainingTestIngestor::poll_once(individual.as_mut()).await.is_pending());
    let mut queued = Box::pin(ctx.get::<DrainingTestIngestor>("a"));
    assert!(DrainingTestIngestor::poll_once(queued.as_mut()).await.is_pending());
    let mut shutdown = Box::pin(ctx.close());
    assert!(DrainingTestIngestor::poll_once(shutdown.as_mut()).await.is_pending());

    draining_ingestor.release.notify_one();
    timeout(DEADLINE, individual).await.unwrap().unwrap();
    assert!(timeout(DEADLINE, queued).await.unwrap().is_err());
    timeout(DEADLINE, shutdown).await.unwrap().unwrap();
    assert_eq!(builds.load(Ordering::SeqCst), 1);
    assert_eq!(draining_ingestor.closes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn concurrent_shutdown_calls_serialize_and_close_once() {
    let ctx = Arc::new(ApplicationContext::new());
    let draining_ingestor = Arc::new(DrainingTestIngestor::default());
    let value = Arc::clone(&draining_ingestor);
    ctx.register::<DrainingTestIngestor, _>("a", move || {
        let value = Arc::clone(&value);
        async move { Ok(value) }
    })
    .unwrap();
    ctx.init().await.unwrap();
    ctx.get::<DrainingTestIngestor>("a").await.unwrap();
    let cloned = Arc::clone(&ctx);
    let first = tokio::spawn(async move { cloned.close().await });
    timeout(DEADLINE, draining_ingestor.entered.notified()).await.unwrap();
    let second_context = Arc::clone(&ctx);
    let second_started = Arc::new(Notify::new());
    let signal = Arc::clone(&second_started);
    let second = tokio::spawn(async move {
        signal.notify_one();
        second_context.shutdown().await
    });
    timeout(DEADLINE, second_started.notified()).await.unwrap();
    assert!(!first.is_finished());
    assert!(!second.is_finished());
    assert!(ctx.get::<DrainingTestIngestor>("a").await.is_err());
    draining_ingestor.release.notify_one();
    timeout(DEADLINE, first).await.unwrap().unwrap().unwrap();
    timeout(DEADLINE, second).await.unwrap().unwrap().unwrap();
    assert_eq!(draining_ingestor.closes.load(Ordering::SeqCst), 1);
}
