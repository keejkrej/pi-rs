//! Port of packages/server/test/fixtures/stale-socket-server.mjs (fixture process entry)

use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("Socket path argument is required")?;

    #[cfg(unix)]
    {
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(b"listening\n")?;
        stdout.flush()?;
        drop(stdout);
        loop {
            let _ = listener.accept();
        }
    }

    #[cfg(not(unix))]
    {
        let _ = path;
        Err("stale-socket-server fixture requires Unix".into())
    }
}
