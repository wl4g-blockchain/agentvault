use anyhow::Result;
use async_trait::async_trait;
use common_context::{ApplicationContext, IProvider};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

trait IService: IProvider {}
struct TestService(Arc<AtomicUsize>);
impl IService for TestService {}
#[async_trait]
impl IProvider for TestService {
    async fn close(&self) -> Result<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn repeated_registration_is_lazy_and_concurrent_get_has_one_owner() -> Result<()> {
    let context = ApplicationContext::new();
    let builds = Arc::new(AtomicUsize::new(0));
    let closes = Arc::new(AtomicUsize::new(0));
    let (count, shutdown_counter) = (builds.clone(), closes.clone());
    assert!(context.register_if_absent::<dyn IService, _>("same", move || {
        let (count, shutdown_counter) = (count.clone(), shutdown_counter.clone());
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(TestService(shutdown_counter)) as Arc<dyn IService>)
        }
    })?);
    assert!(!context.register_if_absent::<dyn IService, _>("same", || async {
        panic!("losing constructor must never run");
    })?);
    assert_eq!(builds.load(Ordering::SeqCst), 0);
    context.init().await?;
    let (first, second) = tokio::join!(context.get::<dyn IService>("same"), context.get::<dyn IService>("same"));
    assert!(Arc::ptr_eq(&first?, &second?));
    assert_eq!(builds.load(Ordering::SeqCst), 1);
    context.close_all::<dyn IService>().await?;
    assert_eq!(closes.load(Ordering::SeqCst), 1);
    assert!(context.get::<dyn IService>("same").await.is_err());
    context.shutdown().await?;
    assert!(context
        .register_if_absent::<dyn IService, _>("same", || async {
            Ok(Arc::new(TestService(Arc::new(AtomicUsize::new(0)))) as Arc<dyn IService>)
        })
        .is_err());
    Ok(())
}
