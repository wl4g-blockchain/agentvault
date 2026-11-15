use anyhow::Result;
use async_trait::async_trait;
use common_context::ApplicationContext;
use common_context::IProvider;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::sync::Barrier;

trait ITestIngestor: IProvider {
    fn source(&self) -> &'static str;
}

#[derive(Default)]
struct WebTestIngestor {
    inits: AtomicUsize,
    closes: AtomicUsize,
}

impl ITestIngestor for WebTestIngestor {
    fn source(&self) -> &'static str {
        "web"
    }
}

#[async_trait]
impl IProvider for WebTestIngestor {
    async fn init(&self) -> Result<()> {
        self.inits.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        self.closes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct IngestionModule;

impl IngestionModule {
    fn register(ingestor: Arc<dyn ITestIngestor>) -> Result<()> {
        ApplicationContext::global().register::<dyn ITestIngestor, _>("web-default", move || {
            let ingestor = Arc::clone(&ingestor);
            async move { Ok(ingestor) }
        })
    }
}

struct ConsumerModule;

impl ConsumerModule {
    async fn ingestor() -> Result<Arc<dyn ITestIngestor>> {
        ApplicationContext::global().get("web-default").await
    }
}

// This integration test has its own process so shutting down the singleton
// cannot affect the independent contexts exercised by other test targets.
#[tokio::test]
async fn global_context_is_shared_across_threads_and_modules() {
    let barrier = Arc::new(Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                ApplicationContext::global()
            })
        })
        .collect();
    let contexts: Vec<_> = threads.into_iter().map(|thread| thread.join().unwrap()).collect();
    let ctx = contexts[0];
    assert!(contexts.iter().all(|context| std::ptr::eq(*context, ctx)));

    let concrete = Arc::new(WebTestIngestor::default());
    let ingestor: Arc<dyn ITestIngestor> = concrete.clone();
    IngestionModule::register(Arc::clone(&ingestor)).unwrap();
    assert_eq!(concrete.inits.load(Ordering::SeqCst), 0);
    assert!(ConsumerModule::ingestor().await.is_err());
    ctx.init().await.unwrap();
    let consumer = ConsumerModule::ingestor().await.unwrap();
    assert!(Arc::ptr_eq(&consumer, &ingestor));
    assert_eq!(consumer.source(), "web");
    assert_eq!(concrete.inits.load(Ordering::SeqCst), 1);

    let isolated = ApplicationContext::new();
    isolated.init().await.unwrap();
    assert!(isolated.get::<dyn ITestIngestor>("web-default").await.is_err());
    isolated.shutdown().await.unwrap();
    assert!(ConsumerModule::ingestor().await.is_ok());

    ctx.shutdown().await.unwrap();
    ctx.shutdown().await.unwrap();
    assert_eq!(concrete.closes.load(Ordering::SeqCst), 1);
    assert!(std::ptr::eq(ApplicationContext::global(), ctx));
    assert!(ApplicationContext::global().init().await.is_err());
    assert!(ConsumerModule::ingestor().await.is_err());
}
