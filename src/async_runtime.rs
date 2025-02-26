use std::{num::NonZeroUsize, time::Duration};

use tokio::runtime::Handle;

pub(crate) fn async_runtime(thread_count: Option<NonZeroUsize>) -> Handle {
    let thread_count = thread_count.map(usize::from).unwrap_or(1);

    let async_runtime = if thread_count == 1 {
        tokio::runtime::Builder::new_current_thread()
            .thread_name("Tokio Runtime Worker Thread")
            .enable_time()
            .enable_io()
            .build()
            .unwrap()
    } else {
        tokio::runtime::Builder::new_multi_thread()
            .thread_name(format!(
                "Tokio Runtime Worker Thread (one among {thread_count})"
            ))
            .worker_threads(thread_count)
            .enable_time()
            .enable_io()
            .build()
            .unwrap()
    };
    let async_runtime_handle = async_runtime.handle().clone();

    std::thread::Builder::new()
        .name("\"Block-On\" Thread for Tokio Runtime".to_string())
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
