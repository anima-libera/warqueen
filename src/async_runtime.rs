use std::time::Duration;

use tokio::runtime::Handle;

pub(crate) fn async_runtime() -> Handle {
    let async_runtime = tokio::runtime::Builder::new_current_thread()
        .thread_name("Tokio Runtime Thread")
        .enable_time()
        .enable_io()
        .build()
        .unwrap();
    let async_runtime_handle = async_runtime.handle().clone();
    std::thread::Builder::new()
        .name("Tokio Runtime Thread".to_string())
        .spawn(move || {
            async_runtime.block_on(async {
                loop {
                    tokio::time::sleep(Duration::from_millis(1)).await
                }
            })
        })
        .unwrap();

    async_runtime_handle
}
