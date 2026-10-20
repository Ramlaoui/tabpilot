mod answer;
mod completion;
mod context;
mod documents;
mod logging;
mod lsp;
mod protocol;
mod providers;
mod workspace;

use crate::lsp::server;

#[tokio::main]
async fn main() {
    logging::init();

    let (mainloop, _) = async_lsp::MainLoop::new_server(server::service);

    let (stdin, stdout) = (
        async_lsp::stdio::PipeStdin::lock_tokio().expect("failed to lock stdin"),
        async_lsp::stdio::PipeStdout::lock_tokio().expect("failed to lock stdout"),
    );

    // An editor that vanishes ends the loop with an error; the agents still stop.
    if let Err(err) = mainloop.run_buffered(stdin, stdout).await {
        tracing::error!(%err, "tabpilot-lsp main loop failed");
    }
    providers::commonthread::shutdown().await;
}
