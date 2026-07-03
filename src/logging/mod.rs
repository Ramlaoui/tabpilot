use tracing::Level;

pub fn init() {
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(Level::INFO)
        .with_writer(std::io::stderr)
        .init();
}
