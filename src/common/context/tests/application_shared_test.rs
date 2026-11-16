use anyhow::anyhow;
use anyhow::bail;
use anyhow::Result;
use async_trait::async_trait;
use common_context::ApplicationContext;
use common_context::IProvider;
use std::any::Any;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
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

trait ITestIngestorHistory: IProvider {
    fn records(&self) -> Vec<IngestedRecord>;
}

#[derive(Default)]
struct TwitterTestIngestor {
    records: Mutex<Vec<IngestedRecord>>,
    inits: AtomicUsize,
    closes: AtomicUsize,
    init_fail_once: AtomicBool,
    init_block_once: AtomicBool,
    close_fail_once: AtomicBool,
    close_block_once: AtomicBool,
    entered: Notify,
    label: &'static str,
    order: Option<Arc<Mutex<Vec<&'static str>>>>,
}

impl ITestIngestor for TwitterTestIngestor {
    fn ingest(&self, source_id: &str, content: &str) -> IngestedRecord {
        let record = IngestedRecord {
            source_id: source_id.to_owned(),
            text: content.trim().to_owned(),
        };
        self.records.lock().unwrap().push(record.clone());
        record
    }
}

impl ITestIngestorMetrics for TwitterTestIngestor {
    fn ingested_count(&self) -> usize {
        self.records.lock().unwrap().len()
    }
}

impl ITestIngestorHistory for TwitterTestIngestor {
    fn records(&self) -> Vec<IngestedRecord> {
        self.records.lock().unwrap().clone()
    }
}

impl TwitterTestIngestor {
    fn register(self: &Arc<Self>, ctx: &ApplicationContext, instance: &str, builds: &Arc<AtomicUsize>) -> Result<()> {
        let (ingestor, calls) = (Arc::clone(self), Arc::clone(builds));
        ctx.register::<dyn ITestIngestor, _>(instance, move || {
            let (ingestor, calls) = (Arc::clone(&ingestor), Arc::clone(&calls));
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                tokio::task::yield_now().await;
                Ok(ingestor as Arc<dyn ITestIngestor>)
            }
        })
    }

    fn metrics_view(source: Arc<dyn ITestIngestor>) -> Result<Arc<dyn ITestIngestorMetrics>> {
        let source: Arc<dyn Any + Send + Sync> = source;
        let ingestor = source
            .downcast::<Self>()
            .map_err(|_| anyhow!("unexpected source type"))?;
        Ok(ingestor)
    }

    fn history_view(source: Arc<dyn ITestIngestor>) -> Result<Arc<dyn ITestIngestorHistory>> {
        let source: Arc<dyn Any + Send + Sync> = source;
        let ingestor = source
            .downcast::<Self>()
            .map_err(|_| anyhow!("unexpected source type"))?;
        Ok(ingestor)
    }
}

#[async_trait]
impl IProvider for TwitterTestIngestor {
    async fn init(&self) -> Result<()> {
        self.inits.fetch_add(1, Ordering::SeqCst);
        if self.init_fail_once.swap(false, Ordering::SeqCst) {
            bail!("init failed");
        }
        if self.init_block_once.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
        tokio::task::yield_now().await;
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        if self.close_fail_once.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            bail!("close failed");
        }
        if self.close_block_once.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
        self.records.lock().unwrap().clear();
        if let Some(order) = &self.order {
            order.lock().unwrap().push(self.label);
        }
        Ok(())
    }
}

#[tokio::test]
async fn aliases_share_lazy_construction_initialization_and_shutdown() {
    timeout(DEADLINE, async {
        let ctx = Arc::new(ApplicationContext::new());
        let ingestor = Arc::new(TwitterTestIngestor::default());
        let builds = Arc::new(AtomicUsize::new(0));
        ingestor.register(&ctx, "shared", &builds).unwrap();
        ctx.register_with::<dyn ITestIngestorMetrics, dyn ITestIngestor>("shared", TwitterTestIngestor::metrics_view)
            .unwrap();
        ctx.register_with::<dyn ITestIngestorHistory, dyn ITestIngestor>("shared", TwitterTestIngestor::history_view)
            .unwrap();
        assert_eq!(builds.load(Ordering::SeqCst), 0);
        assert_eq!(ingestor.inits.load(Ordering::SeqCst), 0);
        assert!(ctx.get::<dyn ITestIngestorMetrics>("shared").await.is_err());
        ctx.init().await.unwrap();

        let mut jobs = Vec::new();
        for index in 0..16 {
            let ctx = Arc::clone(&ctx);
            jobs.push(tokio::spawn(async move {
                if index % 2 == 0 {
                    ctx.get::<dyn ITestIngestor>("shared").await.unwrap() as Arc<dyn IProvider>
                } else {
                    ctx.get::<dyn ITestIngestorMetrics>("shared").await.unwrap() as Arc<dyn IProvider>
                }
            }));
        }
        for job in jobs {
            let alias = job.await.unwrap();
            assert_eq!(Arc::as_ptr(&alias).cast::<()>(), Arc::as_ptr(&ingestor).cast::<()>());
        }
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        assert_eq!(ingestor.inits.load(Ordering::SeqCst), 1);
        let records = ctx.get::<dyn ITestIngestor>("shared").await.unwrap();
        let record = records.ingest("tweet-42", "  Market update\n  ");
        assert_eq!(
            record,
            IngestedRecord {
                source_id: "tweet-42".to_owned(),
                text: "Market update".to_owned(),
            }
        );
        let metrics = ctx.get::<dyn ITestIngestorMetrics>("shared").await.unwrap();
        assert_eq!(metrics.ingested_count(), 1);
        let history = ctx.get::<dyn ITestIngestorHistory>("shared").await.unwrap();
        assert_eq!(history.records(), [record]);
        ctx.close().await.unwrap();
        ctx.shutdown().await.unwrap();
        assert_eq!(ingestor.closes.load(Ordering::SeqCst), 1);
        assert_eq!(ingestor.records(), [] as [IngestedRecord; 0]);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn closing_any_view_invalidates_all_views_without_rebinding_old_aliases() {
    timeout(DEADLINE, async {
        for close_alias in [true, false] {
            let ctx = ApplicationContext::new();
            let original = Arc::new(TwitterTestIngestor::default());
            let builds = Arc::new(AtomicUsize::new(0));
            original.register(&ctx, "shared", &builds).unwrap();
            ctx.register_with::<dyn ITestIngestorMetrics, dyn ITestIngestor>(
                "shared",
                TwitterTestIngestor::metrics_view,
            )
            .unwrap();
            ctx.register_with::<dyn ITestIngestorHistory, dyn ITestIngestor>(
                "shared",
                TwitterTestIngestor::history_view,
            )
            .unwrap();
            ctx.init().await.unwrap();
            ctx.get::<dyn ITestIngestorMetrics>("shared").await.unwrap();
            ctx.get::<dyn ITestIngestorHistory>("shared").await.unwrap();
            if close_alias {
                ctx.close_for::<dyn ITestIngestorMetrics>("shared").await.unwrap();
            } else {
                ctx.close_for::<dyn ITestIngestor>("shared").await.unwrap();
            }
            assert!(ctx.get::<dyn ITestIngestor>("shared").await.is_err());
            assert!(ctx.get::<dyn ITestIngestorMetrics>("shared").await.is_err());
            assert!(ctx.get::<dyn ITestIngestorHistory>("shared").await.is_err());
            assert_eq!(original.inits.load(Ordering::SeqCst), 1);
            assert_eq!(original.closes.load(Ordering::SeqCst), 1);
            ctx.close_for::<dyn ITestIngestor>("shared").await.unwrap();

            let replacement = Arc::new(TwitterTestIngestor::default());
            replacement.register(&ctx, "shared", &builds).unwrap();
            let current = ctx.get::<dyn ITestIngestor>("shared").await.unwrap();
            assert_eq!(
                Arc::as_ptr(&current).cast::<()>(),
                Arc::as_ptr(&replacement).cast::<()>()
            );
            assert!(ctx.get::<dyn ITestIngestorHistory>("shared").await.is_err());
            assert_eq!(builds.load(Ordering::SeqCst), 2);
            ctx.close().await.unwrap();
            assert_eq!(original.closes.load(Ordering::SeqCst), 1);
            assert_eq!(replacement.closes.load(Ordering::SeqCst), 1);
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn alias_registration_validates_source_duplicates_and_cycles_without_building() {
    timeout(DEADLINE, async {
        let ctx = ApplicationContext::new();
        for instance in ["missing", "", " \t"] {
            assert!(ctx
                .register_with::<dyn ITestIngestorMetrics, dyn ITestIngestor>(
                    instance,
                    TwitterTestIngestor::metrics_view
                )
                .is_err());
        }
        let ingestor = Arc::new(TwitterTestIngestor::default());
        let builds = Arc::new(AtomicUsize::new(0));
        ingestor.register(&ctx, "shared", &builds).unwrap();
        ctx.register_with::<dyn ITestIngestorMetrics, dyn ITestIngestor>("shared", TwitterTestIngestor::metrics_view)
            .unwrap();
        assert!(ctx
            .register_with::<dyn ITestIngestorMetrics, dyn ITestIngestor>("shared", TwitterTestIngestor::metrics_view)
            .is_err());
        assert!(ctx
            .register_with::<dyn ITestIngestor, dyn ITestIngestor>("shared", Ok)
            .is_err());
        assert!(ctx
            .register_with::<dyn ITestIngestor, dyn ITestIngestorMetrics>("shared", |_| unreachable!())
            .is_err());
        ctx.init().await.unwrap();
        ctx.register_with::<dyn ITestIngestorHistory, dyn ITestIngestor>("shared", TwitterTestIngestor::history_view)
            .unwrap();
        ctx.close().await.unwrap();
        assert!(ctx
            .register_with::<dyn ITestIngestorMetrics, dyn ITestIngestor>("shared", TwitterTestIngestor::metrics_view)
            .is_err());
        assert_eq!(builds.load(Ordering::SeqCst), 0);
        assert_eq!(ingestor.inits.load(Ordering::SeqCst), 0);
        assert_eq!(ingestor.closes.load(Ordering::SeqCst), 0);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn initialization_failure_through_alias_is_cleaned_and_retried_by_owner() {
    timeout(DEADLINE, async {
        let ctx = ApplicationContext::new();
        let ingestor = Arc::new(TwitterTestIngestor::default());
        let builds = Arc::new(AtomicUsize::new(0));
        ingestor.init_fail_once.store(true, Ordering::SeqCst);
        ingestor.register(&ctx, "shared", &builds).unwrap();
        ctx.register_with::<dyn ITestIngestorMetrics, dyn ITestIngestor>("shared", TwitterTestIngestor::metrics_view)
            .unwrap();
        ctx.init().await.unwrap();
        assert!(ctx.get::<dyn ITestIngestorMetrics>("shared").await.is_err());
        assert_eq!(ingestor.inits.load(Ordering::SeqCst), 1);
        assert_eq!(ingestor.closes.load(Ordering::SeqCst), 1);
        ctx.get::<dyn ITestIngestor>("shared").await.unwrap();
        let alias = ctx.get::<dyn ITestIngestorMetrics>("shared").await.unwrap();
        assert_eq!(Arc::as_ptr(&alias).cast::<()>(), Arc::as_ptr(&ingestor).cast::<()>());
        assert_eq!(builds.load(Ordering::SeqCst), 2);
        assert_eq!(ingestor.inits.load(Ordering::SeqCst), 2);
        ctx.close().await.unwrap();
        assert_eq!(ingestor.closes.load(Ordering::SeqCst), 2);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cancelled_alias_initialization_is_cleaned_before_either_view_retries() {
    timeout(DEADLINE, async {
        for retry_alias in [true, false] {
            let ctx = Arc::new(ApplicationContext::new());
            let ingestor = Arc::new(TwitterTestIngestor::default());
            let builds = Arc::new(AtomicUsize::new(0));
            ingestor.init_block_once.store(true, Ordering::SeqCst);
            ingestor.register(&ctx, "shared", &builds).unwrap();
            ctx.register_with::<dyn ITestIngestorMetrics, dyn ITestIngestor>(
                "shared",
                TwitterTestIngestor::metrics_view,
            )
            .unwrap();
            ctx.init().await.unwrap();
            let cloned = Arc::clone(&ctx);
            let job = tokio::spawn(async move { cloned.get::<dyn ITestIngestorMetrics>("shared").await });
            ingestor.entered.notified().await;
            job.abort();
            assert!(matches!(job.await, Err(error) if error.is_cancelled()));
            assert_eq!(builds.load(Ordering::SeqCst), 1);
            assert_eq!(ingestor.inits.load(Ordering::SeqCst), 1);
            assert_eq!(ingestor.closes.load(Ordering::SeqCst), 0);
            if retry_alias {
                ctx.get::<dyn ITestIngestorMetrics>("shared").await.unwrap();
            } else {
                ctx.get::<dyn ITestIngestor>("shared").await.unwrap();
            }
            let owner = ctx.get::<dyn ITestIngestor>("shared").await.unwrap();
            let alias = ctx.get::<dyn ITestIngestorMetrics>("shared").await.unwrap();
            assert_eq!(Arc::as_ptr(&owner).cast::<()>(), Arc::as_ptr(&alias).cast::<()>());
            assert_eq!(builds.load(Ordering::SeqCst), 2);
            assert_eq!(ingestor.inits.load(Ordering::SeqCst), 2);
            assert_eq!(ingestor.closes.load(Ordering::SeqCst), 1);
            ctx.close().await.unwrap();
            assert_eq!(ingestor.closes.load(Ordering::SeqCst), 2);
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn failed_or_cancelled_alias_close_retains_shared_owner_for_retry() {
    timeout(DEADLINE, async {
        for cancel in [false, true] {
            let ctx = Arc::new(ApplicationContext::new());
            let ingestor = Arc::new(TwitterTestIngestor::default());
            let builds = Arc::new(AtomicUsize::new(0));
            ingestor.close_fail_once.store(!cancel, Ordering::SeqCst);
            ingestor.close_block_once.store(cancel, Ordering::SeqCst);
            ingestor.register(&ctx, "shared", &builds).unwrap();
            ctx.register_with::<dyn ITestIngestorMetrics, dyn ITestIngestor>(
                "shared",
                TwitterTestIngestor::metrics_view,
            )
            .unwrap();
            ctx.register_with::<dyn ITestIngestorHistory, dyn ITestIngestor>(
                "shared",
                TwitterTestIngestor::history_view,
            )
            .unwrap();
            ctx.init().await.unwrap();
            ctx.get::<dyn ITestIngestorMetrics>("shared").await.unwrap();
            let cloned = Arc::clone(&ctx);
            let job = tokio::spawn(async move { cloned.close_for::<dyn ITestIngestorMetrics>("shared").await });
            ingestor.entered.notified().await;
            if cancel {
                job.abort();
                assert!(matches!(job.await, Err(error) if error.is_cancelled()));
            } else {
                assert!(job.await.unwrap().is_err());
            }
            assert!(ctx.get::<dyn ITestIngestor>("shared").await.is_err());
            assert!(ctx.get::<dyn ITestIngestorMetrics>("shared").await.is_err());
            assert!(ctx.get::<dyn ITestIngestorHistory>("shared").await.is_err());
            assert_eq!(ingestor.closes.load(Ordering::SeqCst), 1);
            ctx.close_for::<dyn ITestIngestorMetrics>("shared").await.unwrap();
            assert_eq!(ingestor.closes.load(Ordering::SeqCst), 2);
            ctx.close().await.unwrap();
            assert_eq!(ingestor.closes.load(Ordering::SeqCst), 2);
            assert_eq!(builds.load(Ordering::SeqCst), 1);
            assert_eq!(ingestor.inits.load(Ordering::SeqCst), 1);
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn projection_failure_does_not_close_or_reinitialize_the_owner() {
    timeout(DEADLINE, async {
        let ctx = ApplicationContext::new();
        let ingestor = Arc::new(TwitterTestIngestor::default());
        let builds = Arc::new(AtomicUsize::new(0));
        ingestor.register(&ctx, "shared", &builds).unwrap();
        let fail_once = AtomicBool::new(true);
        ctx.register_with::<dyn ITestIngestorMetrics, dyn ITestIngestor>("shared", move |source| {
            if fail_once.swap(false, Ordering::SeqCst) {
                bail!("projection failed");
            }
            TwitterTestIngestor::metrics_view(source)
        })
        .unwrap();
        ctx.init().await.unwrap();
        let error = ctx.get::<dyn ITestIngestorMetrics>("shared").await.err().unwrap();
        assert!(format!("{error:#}").contains("projection failed"));
        ctx.get::<dyn ITestIngestor>("shared").await.unwrap();
        ctx.get::<dyn ITestIngestorMetrics>("shared").await.unwrap();
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        assert_eq!(ingestor.inits.load(Ordering::SeqCst), 1);
        assert_eq!(ingestor.closes.load(Ordering::SeqCst), 0);
        ctx.close().await.unwrap();
        assert_eq!(ingestor.closes.load(Ordering::SeqCst), 1);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn projecting_a_different_object_is_rejected_without_affecting_the_owner() {
    timeout(DEADLINE, async {
        let ctx = ApplicationContext::new();
        let ingestor = Arc::new(TwitterTestIngestor::default());
        let different = Arc::new(TwitterTestIngestor::default());
        let builds = Arc::new(AtomicUsize::new(0));
        ingestor.register(&ctx, "shared", &builds).unwrap();
        let unrelated = Arc::clone(&different);
        ctx.register_with::<dyn ITestIngestorMetrics, dyn ITestIngestor>("shared", move |_| {
            Ok(Arc::clone(&unrelated) as Arc<dyn ITestIngestorMetrics>)
        })
        .unwrap();
        ctx.init().await.unwrap();
        assert!(ctx.get::<dyn ITestIngestorMetrics>("shared").await.is_err());
        let owner = ctx.get::<dyn ITestIngestor>("shared").await.unwrap();
        assert_eq!(Arc::as_ptr(&owner).cast::<()>(), Arc::as_ptr(&ingestor).cast::<()>());
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        assert_eq!(ingestor.inits.load(Ordering::SeqCst), 1);
        assert_eq!(ingestor.closes.load(Ordering::SeqCst), 0);
        ctx.close().await.unwrap();
        assert_eq!(ingestor.closes.load(Ordering::SeqCst), 1);
        assert_eq!(different.inits.load(Ordering::SeqCst), 0);
        assert_eq!(different.closes.load(Ordering::SeqCst), 0);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn alias_inherits_the_owners_shutdown_phase() {
    timeout(DEADLINE, async {
        let ctx = ApplicationContext::new();
        let order = Arc::new(Mutex::new(Vec::new()));
        let shared = Arc::new(TwitterTestIngestor {
            label: "shared",
            order: Some(Arc::clone(&order)),
            ..TwitterTestIngestor::default()
        });
        let dependent = Arc::new(TwitterTestIngestor {
            label: "dependent",
            order: Some(Arc::clone(&order)),
            ..TwitterTestIngestor::default()
        });
        for (instance, phase, ingestor) in [("shared", -10, Arc::clone(&shared)), ("dependent", -5, dependent)] {
            ctx.register_with_shutdown_phase::<dyn ITestIngestor, _>(instance, phase, move || {
                let ingestor = Arc::clone(&ingestor);
                async move { Ok(ingestor as Arc<dyn ITestIngestor>) }
            })
            .unwrap();
        }
        ctx.register_with::<dyn ITestIngestorMetrics, dyn ITestIngestor>("shared", TwitterTestIngestor::metrics_view)
            .unwrap();
        ctx.init().await.unwrap();
        ctx.get::<dyn ITestIngestorMetrics>("shared").await.unwrap();
        ctx.get::<dyn ITestIngestor>("dependent").await.unwrap();
        ctx.close().await.unwrap();
        assert_eq!(*order.lock().unwrap(), ["dependent", "shared"]);
        assert_eq!(shared.closes.load(Ordering::SeqCst), 1);
    })
    .await
    .unwrap();
}
