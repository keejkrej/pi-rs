//! Port of packages/client/test/fixtures/stale-socket-server.mjs

fn main() {
    let path = std::env::args().nth(1).expect("Socket path argument is required");

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::net::UnixListener;

        let _listener = UnixListener::bind(&path).expect("failed to bind stale unix socket");
        println!("listening");
        let _ = std::io::stdout().flush();
        loop {
            std::thread::park();
        }
    }

    #[cfg(not(unix))]
    {
        let _ = path;
        eprintln!("Unix transport is not supported on Windows");
        std::process::exit(1);
    }
}
