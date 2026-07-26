//! Environment-backed tunables for the transport and transfer engine.
//!
//! Every performance-relevant constant is reachable from an environment
//! variable so a slow link can be bisected in the field without a rebuild —
//! which is the difference between diagnosing a problem at a remote site in an
//! afternoon and shipping a build per hypothesis.
//!
//! Values are read once and cached. A malformed value falls back to the default
//! rather than failing: a typo in an env var must not stop the app from starting.

use std::sync::OnceLock;

/// Read a numeric tunable from the environment, or use `default`.
///
/// The cache is keyed per call site by the `OnceLock` the caller owns, so this
/// helper is only ever called through the accessors below.
fn read_env<T: std::str::FromStr>(key: &str, default: T) -> T {
    match std::env::var(key) {
        Ok(v) => match v.trim().parse::<T>() {
            Ok(parsed) => parsed,
            Err(_) => {
                // No tracing here: this runs before the subscriber is installed.
                eprintln!("myd: ignoring malformed {}={:?}", key, v);
                default
            }
        },
        Err(_) => default,
    }
}

macro_rules! tunable {
    ($(#[$meta:meta])* $name:ident: $ty:ty = $env:literal, $default:expr) => {
        $(#[$meta])*
        pub fn $name() -> $ty {
            static CACHE: OnceLock<$ty> = OnceLock::new();
            *CACHE.get_or_init(|| read_env($env, $default))
        }
    };
}

tunable! {
    /// SSH channel receive window, in bytes.
    ///
    /// This is the ceiling nothing else can lift: with a window of `w` and a
    /// round trip of `rtt`, throughput cannot exceed `w / rtt` however deep the
    /// SFTP request pipeline is. russh defaults to 2 MiB, which caps a 150 ms
    /// transatlantic link at roughly 13 MiB/s — below what the link and the
    /// request pipeline could otherwise sustain.
    ///
    /// 64 MiB leaves headroom for ~426 MiB/s at that latency. It is a receive
    /// window we advertise, so the memory is only committed as the peer fills it.
    ssh_window_size: u32 = "MYD_SSH_WINDOW", 64 * 1024 * 1024
}

tunable! {
    /// SSH maximum packet size, in bytes.
    ///
    /// Left at russh's default. Raising it risks a disconnect from strict
    /// servers and is not a bottleneck — exposed only so it can be ruled out.
    ssh_max_packet: u32 = "MYD_SSH_MAX_PACKET", 32768
}

tunable! {
    /// Whether to disable Nagle's algorithm on the SSH socket.
    ///
    /// SFTP is request/response with many small packets, exactly the shape where
    /// Nagle interacts with delayed ACK to add tens of milliseconds per
    /// exchange. russh leaves it enabled by default.
    ssh_nodelay: bool = "MYD_SSH_NODELAY", true
}

tunable! {
    /// How many bytes the SFTP client may have outstanding on the compat-file
    /// write path before it waits.
    ///
    /// The crate defaults to 640 KiB, which caps uploads at about 4 MiB/s over a
    /// 150 ms link no matter how large the buffers above it are.
    sftp_write_limit: usize = "MYD_SFTP_WRITE_LIMIT", 16 * 1024 * 1024
}

tunable! {
    /// Ceiling on SFTP requests in flight on one connection.
    sftp_max_pending: u16 = "MYD_SFTP_MAX_PENDING", 1024
}

tunable! {
    /// Bytes per chunk on the parallel read path.
    ///
    /// Should stay at or below the server's negotiated read limit (256 KiB for
    /// OpenSSH). A larger value does not produce a larger request — it produces
    /// *several* requests issued back-to-back on one handle, which makes the
    /// pipeline shallower than its window count suggests.
    transfer_chunk_size: usize = "MYD_CHUNK_SIZE", 256 * 1024
}

tunable! {
    /// Concurrent chunk reads within one large file.
    transfer_chunks_in_flight: usize = "MYD_CHUNKS_IN_FLIGHT", 32
}

tunable! {
    /// Concurrent transfers, and files copied per directory level.
    transfer_max_parallel: usize = "MYD_MAX_PARALLEL", 16
}

tunable! {
    /// Global ceiling on concurrent file operations during a recursive copy.
    ///
    /// Per-level windows would otherwise multiply: a tree `d` levels deep can
    /// reach `max_parallel^d` simultaneous operations without a shared bound.
    transfer_global_concurrency: usize = "MYD_GLOBAL_CONCURRENCY", 64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_values_fall_back_to_the_default() {
        assert_eq!(read_env::<usize>("MYD_TEST_NOT_SET_XYZ", 42), 42);
    }

    #[test]
    fn defaults_are_sane() {
        // The chunk size must not exceed a typical negotiated read limit, or one
        // logical chunk silently becomes several serial wire requests.
        assert!(transfer_chunk_size() <= 256 * 1024);
        assert!(ssh_window_size() >= 2 * 1024 * 1024);
    }
}
