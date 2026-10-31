//! Downloads for `tron update`.
//!
//! A release archive comes down over several connections at once: the first
//! request asks for the whole file, and as soon as its headers tell the size,
//! helpers fetch the later parts with range requests while the first keeps
//! reading the front. Nothing waits for an extra round trip to learn the size,
//! and a server that ignores ranges is read over the one connection. Parts that
//! break off resume where they stopped. Everything stays in memory: archives
//! are a few megabytes, and the checksum and unpacking read them from there.

use std::io::{self, IsTerminal, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use ureq::ResponseExt;
use ureq::http::Uri;

/// Connections for one file, the first included.
const CONNECTIONS: usize = 4;
/// Smallest part worth its own connection.
const MIN_PART: u64 = 1 << 20;
/// Times a part is requested again after its connection fails.
const RETRIES: usize = 3;
/// Most bytes read for a text file such as `SHA256SUMS`.
const TEXT_LIMIT: u64 = 4 << 20;
/// Most bytes for a downloaded file.
const FILE_LIMIT: u64 = 1 << 30;

#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
}

impl Client {
    pub fn new() -> Self {
        let agent = ureq::Agent::config_builder()
            .user_agent(concat!("tron/", env!("CARGO_PKG_VERSION")))
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(15)))
            .timeout_per_read(Some(Duration::from_secs(20)))
            .build()
            .new_agent();
        Self { agent }
    }

    /// A small text file, or `None` when the server has no such file (404).
    pub fn text(&self, url: &str) -> anyhow::Result<Option<String>> {
        let response = match self.agent.get(url).call() {
            Ok(response) => response,
            Err(ureq::Error::StatusCode(404)) => return Ok(None),
            Err(error) => return Err(error).with_context(|| format!("cannot download {url}")),
        };
        let mut text = String::new();
        response
            .into_body()
            .into_reader()
            .take(TEXT_LIMIT)
            .read_to_string(&mut text)
            .with_context(|| format!("cannot read {url}"))?;
        Ok(Some(text))
    }

    /// The file at `url`, with a progress line on a terminal labelled `label`.
    pub fn file(&self, url: &str, label: &str) -> anyhow::Result<Vec<u8>> {
        let mut first =
            self.agent.get(url).header("Range", "bytes=0-").call().with_context(|| format!("cannot download {url}"))?;
        // Helpers skip the redirects the first request followed.
        let uri = first.get_uri().clone();
        let total = match first.status().as_u16() {
            206 => {
                first.headers().get("content-range").and_then(|value| value.to_str().ok()).and_then(content_range_total)
            }
            _ => None,
        };
        let progress = Progress::new(label, total);
        let reader = first.body_mut().as_reader();
        let Some(total) = total else {
            // No ranges: one connection, size unknown until the end.
            let mut data = Vec::new();
            let mut reader = Counted { inner: reader.take(FILE_LIMIT), progress: &progress };
            reader.read_to_end(&mut data).with_context(|| format!("cannot download {url}"))?;
            progress.finish();
            return Ok(data);
        };
        if total > FILE_LIMIT {
            bail!("{url} is {total} bytes, too large for a release");
        }
        let mut data = vec![0; total as usize];
        let parts = part_sizes(total);
        let progress = &progress;
        let done = &AtomicBool::new(false);
        std::thread::scope(|scope| -> anyhow::Result<()> {
            let ticker = progress.visible.then(|| {
                scope.spawn(move || {
                    while !done.load(Ordering::Acquire) {
                        progress.draw();
                        std::thread::park_timeout(Duration::from_millis(50));
                    }
                })
            });
            let mut slices = Vec::with_capacity(parts.len());
            let mut rest = data.as_mut_slice();
            for &size in &parts {
                let (part, tail) = rest.split_at_mut(size as usize);
                slices.push(part);
                rest = tail;
            }
            let mut slices = slices.into_iter();
            let head = slices.next().unwrap_or_default();
            let mut start = head.len() as u64;
            let helpers: Vec<_> = slices
                .map(|part| {
                    let offset = start;
                    start += part.len() as u64;
                    let (client, uri) = (self.clone(), &uri);
                    scope.spawn(move || client.fetch_range(uri, offset, part, progress, None))
                })
                .collect();
            let result = self.fetch_range(&uri, 0, head, progress, Some(reader));
            let results: Vec<_> = helpers
                .into_iter()
                .map(|helper| helper.join().unwrap_or_else(|_| Err(anyhow::anyhow!("a download thread panicked"))))
                .collect();
            done.store(true, Ordering::Release);
            if let Some(ticker) = ticker {
                ticker.thread().unpark();
                let _ = ticker.join();
            }
            results.into_iter().chain([result]).collect()
        })
        .with_context(|| format!("cannot download {url}"))?;
        progress.finish();
        Ok(data)
    }

    /// Fills `part` with the bytes from `offset` on, reading from `reader`
    /// first when given, and resuming with range requests when a read fails.
    fn fetch_range(
        &self,
        uri: &Uri,
        offset: u64,
        part: &mut [u8],
        progress: &Progress,
        mut reader: Option<ureq::BodyReader<'_>>,
    ) -> anyhow::Result<()> {
        let mut filled = 0;
        let mut attempts = 0;
        while filled < part.len() {
            let result = match reader.take() {
                Some(reader) => read_into(reader, &mut part[filled..], &mut filled, progress),
                None => {
                    let from = offset + filled as u64;
                    let to = offset + part.len() as u64 - 1;
                    self.agent
                        .get(uri)
                        .header("Range", format!("bytes={from}-{to}"))
                        .call()
                        .map_err(io::Error::other)
                        .and_then(|mut response| {
                            if response.status().as_u16() != 206 {
                                return Err(io::Error::other(format!(
                                    "asked for bytes {from}-{to}, got status {}",
                                    response.status()
                                )));
                            }
                            read_into(response.body_mut().as_reader(), &mut part[filled..], &mut filled, progress)
                        })
                }
            };
            match result {
                Ok(()) => {}
                Err(error) if attempts < RETRIES => {
                    attempts += 1;
                    log::debug!("download of bytes from {offset} broke off, resuming: {error}");
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

/// Reads until `part` is full, adding what was read to `filled`.
fn read_into(mut reader: impl Read, part: &mut [u8], filled: &mut usize, progress: &Progress) -> io::Result<()> {
    let mut at = 0;
    while at < part.len() {
        match reader.read(&mut part[at..]) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the connection closed early")),
            Ok(read) => {
                at += read;
                *filled += read;
                progress.add(read);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Sizes of the parts `total` bytes are split into, front first.
fn part_sizes(total: u64) -> Vec<u64> {
    let count = (total / MIN_PART).clamp(1, CONNECTIONS as u64);
    let size = total.div_ceil(count);
    (0..count).map(|index| size.min(total - (index * size).min(total))).filter(|&size| size > 0).collect()
}

/// The full size from a `Content-Range: bytes 0-99/1234` header.
fn content_range_total(value: &str) -> Option<u64> {
    value.strip_prefix("bytes ")?.rsplit_once('/')?.1.trim().parse().ok()
}

/// A one-line progress display on standard error, when that is a terminal.
pub struct Progress {
    label: String,
    total: Option<u64>,
    received: AtomicU64,
    started: Instant,
    visible: bool,
}

impl Progress {
    fn new(label: &str, total: Option<u64>) -> Self {
        let visible = !label.is_empty() && io::stderr().is_terminal();
        Self { label: label.to_owned(), total, received: AtomicU64::new(0), started: Instant::now(), visible }
    }

    fn add(&self, bytes: usize) {
        self.received.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    fn draw(&self) {
        let received = self.received.load(Ordering::Relaxed);
        let seconds = self.started.elapsed().as_secs_f64().max(0.001);
        let size = match self.total {
            Some(total) => format!("{} / {}", megabytes(received), megabytes(total)),
            None => megabytes(received),
        };
        let rate = megabytes((received as f64 / seconds) as u64);
        let _ = write!(io::stderr(), "\r\x1b[2K\x1b[36m==>\x1b[0m Downloading {}  {size}  {rate}/s", self.label);
    }

    fn finish(&self) {
        if self.visible {
            let _ = write!(io::stderr(), "\r\x1b[2K");
        }
    }
}

fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1e6)
}

/// A reader that counts what passes through for the progress line.
struct Counted<'a, R> {
    inner: R,
    progress: &'a Progress,
}

impl<R: Read> Read for Counted<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.progress.add(read);
        if self.progress.visible {
            self.progress.draw();
        }
        Ok(read)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::BufRead;
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    /// A local HTTP server for tests. `/redirect/NAME` redirects to `/NAME`,
    /// and files are served with range support unless `ranges` is false.
    pub struct Server {
        pub url: String,
        pub requests: Arc<AtomicUsize>,
    }

    impl Server {
        pub fn start(files: Vec<(&'static str, Vec<u8>)>, ranges: bool) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let files = Arc::new(files);
            let requests = Arc::new(AtomicUsize::new(0));
            let counter = requests.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let (files, counter) = (files.clone(), counter.clone());
                    std::thread::spawn(move || serve(stream, &files, ranges, &counter));
                }
            });
            Self { url, requests }
        }
    }

    fn serve(stream: std::net::TcpStream, files: &[(&str, Vec<u8>)], ranges: bool, counter: &AtomicUsize) {
        let mut reader = io::BufReader::new(stream.try_clone().unwrap());
        let mut stream = stream;
        loop {
            let mut request = String::new();
            if reader.read_line(&mut request).unwrap_or(0) == 0 {
                return;
            }
            counter.fetch_add(1, Ordering::Relaxed);
            let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
            let mut range = None;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
                if let Some((key, value)) = line.split_once(':')
                    && key.eq_ignore_ascii_case("range")
                {
                    let spec = value.trim().strip_prefix("bytes=").unwrap().to_owned();
                    let (from, to) = spec.split_once('-').unwrap();
                    range = Some((from.parse::<usize>().unwrap(), to.parse::<usize>().ok()));
                }
            }
            if let Some(name) = path.strip_prefix("/redirect/") {
                let _ = write!(stream, "HTTP/1.1 302 Found\r\nLocation: /{name}\r\nContent-Length: 0\r\n\r\n");
                continue;
            }
            let Some((_, body)) = files.iter().find(|(name, _)| path == format!("/{name}")) else {
                let _ = write!(stream, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
                continue;
            };
            let _ = match range.filter(|_| ranges) {
                Some((from, to)) => {
                    let to = to.unwrap_or(body.len() - 1).min(body.len() - 1);
                    write!(
                        stream,
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {from}-{to}/{}\r\nContent-Length: {}\r\n\r\n",
                        body.len(),
                        to + 1 - from
                    )
                    .and_then(|()| stream.write_all(&body[from..=to]))
                }
                None => write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len())
                    .and_then(|()| stream.write_all(body)),
            };
        }
    }

    fn sample(len: usize) -> Vec<u8> {
        (0..len).map(|index| (index * 7 + index / 251) as u8).collect()
    }

    #[test]
    fn splits_into_parts_that_cover_the_file() {
        for total in [1, 1000, MIN_PART, MIN_PART + 1, 3 * MIN_PART + 17, 50 * MIN_PART + 3] {
            let parts = part_sizes(total);
            assert_eq!(parts.iter().sum::<u64>(), total, "{total}");
            assert!(parts.len() <= CONNECTIONS);
        }
        assert_eq!(part_sizes(MIN_PART - 1).len(), 1, "small files use one connection");
        assert_eq!(part_sizes(9 * MIN_PART).len(), CONNECTIONS);
    }

    #[test]
    fn reads_the_size_from_content_range() {
        assert_eq!(content_range_total("bytes 0-99/1234"), Some(1234));
        assert_eq!(content_range_total("bytes 0-99/*"), None);
        assert_eq!(content_range_total("items 0-1/2"), None);
    }

    #[test]
    fn downloads_in_parallel_parts() {
        let data = sample(5 * MIN_PART as usize + 12345);
        let server = Server::start(vec![("big", data.clone())], true);
        let client = Client::new();
        let got = client.file(&format!("{}/redirect/big", server.url), "").unwrap();
        assert!(got == data, "the parts join up to the file");
        // The redirect, the first request and one request per helper.
        assert_eq!(server.requests.load(Ordering::Relaxed), 1 + CONNECTIONS);
    }

    #[test]
    fn downloads_without_ranges() {
        let data = sample(3 * MIN_PART as usize);
        let server = Server::start(vec![("big", data.clone())], false);
        let got = Client::new().file(&format!("{}/big", server.url), "").unwrap();
        assert!(got == data);
        assert_eq!(server.requests.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn missing_text_is_none() {
        let server = Server::start(vec![("SUMS", b"abc".to_vec())], true);
        let client = Client::new();
        assert_eq!(client.text(&format!("{}/SUMS", server.url)).unwrap().as_deref(), Some("abc"));
        assert_eq!(client.text(&format!("{}/nothing", server.url)).unwrap(), None);
    }
}
