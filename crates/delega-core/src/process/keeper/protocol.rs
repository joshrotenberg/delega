//! The keeper's hello and the lines both sides write.

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::time::Duration;

const MAGIC: &[u8; 4] = b"DKH1";
/// Most bytes a hello may hold, far above any command line.
pub(super) const MAX_HELLO: usize = 64 * 1024 * 1024;
/// Longest line either side writes.
pub(super) const MAX_LINE: usize = 4096;

const PROGRAM: u8 = 1;
const ARG: u8 = 2;
const ENV: u8 = 3;
const CWD: u8 = 4;
const STDIN: u8 = 5;
const GRACE: u8 = 6;
const DEADLINE: u8 = 7;
const CEILING: u8 = 8;
const MARKER: u8 = 9;

/// What the keeper is asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hello {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
    pub cwd: Option<PathBuf>,
    /// Whether the program has a standard input of its own (the first of the
    /// passed descriptors); otherwise it reads the null device.
    pub stdin: bool,
    pub grace: Duration,
    pub deadline: Duration,
    pub ceiling: Duration,
    pub marker: String,
}

fn record(out: &mut Vec<u8>, tag: u8, bytes: &[u8]) {
    out.push(tag);
    out.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(bytes);
}

fn millis(d: Duration) -> Vec<u8> {
    d.as_millis().to_string().into_bytes()
}

impl Hello {
    /// The hello as bytes.
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut body = Vec::new();
        record(&mut body, PROGRAM, self.program.as_bytes());
        for arg in &self.args {
            record(&mut body, ARG, arg.as_bytes());
        }
        for (name, value) in &self.env {
            let mut pair = name.as_bytes().to_vec();
            pair.push(b'=');
            pair.extend_from_slice(value.as_bytes());
            record(&mut body, ENV, &pair);
        }
        if let Some(cwd) = &self.cwd {
            record(&mut body, CWD, cwd.as_os_str().as_bytes());
        }
        if self.stdin {
            record(&mut body, STDIN, b"1");
        }
        record(&mut body, GRACE, &millis(self.grace));
        record(&mut body, DEADLINE, &millis(self.deadline));
        record(&mut body, CEILING, &millis(self.ceiling));
        record(&mut body, MARKER, self.marker.as_bytes());
        let mut out = MAGIC.to_vec();
        out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(u32::MAX).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// How many bytes the whole hello takes, once its header is in `bytes`.
    pub(crate) fn total_len(bytes: &[u8]) -> Result<Option<usize>, String> {
        if bytes.len() < 8 {
            return Ok(None);
        }
        if &bytes[..4] != MAGIC {
            return Err("not a keeper hello".into());
        }
        let len = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        if len > MAX_HELLO {
            return Err("the hello is too large".into());
        }
        Ok(Some(8 + len))
    }

    /// Read a complete hello.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, String> {
        let total = Self::total_len(bytes)?.ok_or("the hello is incomplete")?;
        if bytes.len() != total {
            return Err("the hello has the wrong length".into());
        }
        let mut rest = &bytes[8..];
        let mut hello = Hello {
            program: OsString::new(),
            args: Vec::new(),
            env: Vec::new(),
            cwd: None,
            stdin: false,
            grace: Duration::ZERO,
            deadline: Duration::ZERO,
            ceiling: Duration::ZERO,
            marker: String::new(),
        };
        let number = |b: &[u8]| -> Result<Duration, String> {
            std::str::from_utf8(b)
                .ok()
                .and_then(|t| t.parse::<u64>().ok())
                .map(Duration::from_millis)
                .ok_or_else(|| "a duration in the hello is not a number".to_string())
        };
        while !rest.is_empty() {
            if rest.len() < 5 {
                return Err("a record of the hello is cut short".into());
            }
            let tag = rest[0];
            let len = u32::from_le_bytes([rest[1], rest[2], rest[3], rest[4]]) as usize;
            if rest.len() < 5 + len {
                return Err("a record of the hello is cut short".into());
            }
            let value = &rest[5..5 + len];
            rest = &rest[5 + len..];
            match tag {
                PROGRAM => hello.program = OsString::from_vec(value.to_vec()),
                ARG => hello.args.push(OsString::from_vec(value.to_vec())),
                ENV => {
                    let at = value
                        .iter()
                        .position(|b| *b == b'=')
                        .ok_or("an environment record has no '='")?;
                    hello.env.push((
                        OsString::from_vec(value[..at].to_vec()),
                        OsString::from_vec(value[at + 1..].to_vec()),
                    ));
                }
                CWD => hello.cwd = Some(PathBuf::from(OsString::from_vec(value.to_vec()))),
                STDIN => hello.stdin = true,
                GRACE => hello.grace = number(value)?,
                DEADLINE => hello.deadline = number(value)?,
                CEILING => hello.ceiling = number(value)?,
                MARKER => {
                    hello.marker = String::from_utf8(value.to_vec())
                        .map_err(|_| "the marker is not text".to_string())?;
                }
                _ => return Err(format!("unknown hello record {tag}")),
            }
        }
        if hello.program.is_empty() {
            return Err("the hello names no program".into());
        }
        Ok(hello)
    }

    /// How many descriptors travel with the hello.
    pub(crate) fn descriptors(&self) -> usize {
        if self.stdin { 3 } else { 2 }
    }
}

/// A line from the keeper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FromKeeper {
    Subreaper(Result<(), String>),
    Started(i32),
    SpawnFailed {
        errno: i32,
        message: String,
    },
    ExitedCode(i32),
    ExitedSignal(i32),
    /// The answer to a census or a freeze, with the walk's details for the
    /// diagnostics (empty where there is no walk).
    Tree {
        id: u64,
        live: usize,
        detail: String,
    },
    /// The program is reaped; how many processes of the tree were still alive
    /// after the last freeze (Linux; 0 elsewhere), and whether the final
    /// census before the reap proved the tree empty (Linux with a subreaper
    /// only; see `Walked::proves_empty`), whether any answer of the run
    /// was unproven in a way no later answer repairs (`unproven`), and
    /// whether any was a freeze that only did not complete (`incomplete`),
    /// each recorded by the keeper before it sent that answer.
    Reaped {
        live: usize,
        proven: bool,
        unproven: bool,
        incomplete: bool,
    },
    /// The keeper is about to reap the program on its own (a signal, its
    /// ceiling, or a new parent): the owner must send no more signals that
    /// rely on the program's pid and answer `released`.
    Releasing,
    Warn(String),
}

impl FromKeeper {
    pub(crate) fn line(&self) -> String {
        match self {
            FromKeeper::Subreaper(Ok(())) => "subreaper on".into(),
            FromKeeper::Subreaper(Err(why)) => format!("subreaper off {}", one_line(why)),
            FromKeeper::Started(pid) => format!("started {pid}"),
            FromKeeper::SpawnFailed { errno, message } => {
                format!("spawn-failed {errno} {}", one_line(message))
            }
            FromKeeper::ExitedCode(code) => format!("exited code {code}"),
            FromKeeper::ExitedSignal(signal) => format!("exited signal {signal}"),
            FromKeeper::Tree { id, live, detail } if detail.is_empty() => {
                format!("tree {id} {live}")
            }
            FromKeeper::Tree { id, live, detail } => {
                format!("tree {id} {live} {}", one_line(detail))
            }
            FromKeeper::Reaped {
                live,
                proven,
                unproven,
                incomplete,
            } => format!(
                "reaped {live}{}{}{}",
                if *proven { " proven" } else { "" },
                if *unproven { " unproven" } else { "" },
                if *incomplete { " incomplete" } else { "" }
            ),
            FromKeeper::Releasing => "releasing".into(),
            FromKeeper::Warn(text) => format!("warn {}", one_line(text)),
        }
    }

    pub(crate) fn parse(line: &str) -> Option<Self> {
        let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
        let mut words = rest.split(' ');
        Some(match verb {
            "subreaper" => match rest.split_once(' ').unwrap_or((rest, "")) {
                ("on", _) => FromKeeper::Subreaper(Ok(())),
                ("off", why) => FromKeeper::Subreaper(Err(why.to_string())),
                _ => return None,
            },
            "started" => FromKeeper::Started(rest.parse().ok()?),
            "spawn-failed" => {
                let (errno, message) = rest.split_once(' ').unwrap_or((rest, ""));
                FromKeeper::SpawnFailed {
                    errno: errno.parse().ok()?,
                    message: message.to_string(),
                }
            }
            "exited" => match (words.next()?, words.next()?.parse().ok()?) {
                ("code", n) => FromKeeper::ExitedCode(n),
                ("signal", n) => FromKeeper::ExitedSignal(n),
                _ => return None,
            },
            "tree" => FromKeeper::Tree {
                id: words.next()?.parse().ok()?,
                live: words.next()?.parse().ok()?,
                detail: words.collect::<Vec<_>>().join(" "),
            },
            "reaped" => {
                // The count is required: a line without it claims nothing.
                let live = words.next()?.parse().ok()?;
                // Each flag at most once, in this order; anything else is not
                // a `reaped` line.
                let mut flags = [
                    ("proven", false),
                    ("unproven", false),
                    ("incomplete", false),
                ];
                let mut next = 0;
                for word in words {
                    let at = flags[next..].iter().position(|(name, _)| *name == word)?;
                    flags[next + at].1 = true;
                    next += at + 1;
                }
                FromKeeper::Reaped {
                    live,
                    proven: flags[0].1,
                    unproven: flags[1].1,
                    incomplete: flags[2].1,
                }
            }
            "releasing" => FromKeeper::Releasing,
            "warn" => FromKeeper::Warn(rest.to_string()),
            _ => return None,
        })
    }
}

/// A line from the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToKeeper {
    Census(u64),
    Freeze(u64),
    Reap,
    /// The answer to `releasing`: the owner sends no more signals that rely on
    /// the program's pid.
    Released,
}

impl ToKeeper {
    pub(crate) fn line(self) -> String {
        match self {
            ToKeeper::Census(id) => format!("census {id}"),
            ToKeeper::Freeze(id) => format!("freeze {id}"),
            ToKeeper::Reap => "reap".into(),
            ToKeeper::Released => "released".into(),
        }
    }

    pub(crate) fn parse(line: &str) -> Option<Self> {
        let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
        Some(match verb {
            "census" => ToKeeper::Census(rest.parse().ok()?),
            "freeze" => ToKeeper::Freeze(rest.parse().ok()?),
            "reap" => ToKeeper::Reap,
            "released" => ToKeeper::Released,
            _ => return None,
        })
    }
}

fn one_line(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    // Cut by bytes, at a character boundary, so every line either side
    // writes stays within [`MAX_LINE`] with its verb and numbers.
    let mut end = flat.len().min(MAX_LINE / 2);
    while !flat.is_char_boundary(end) {
        end -= 1;
    }
    flat[..end].to_string()
}

/// One frame of a line stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Frame {
    /// A line, without its newline.
    Line(String),
    /// A line longer than [`MAX_LINE`]: nothing of it is a line, from its
    /// first byte through the next newline. Neither side writes one, so it
    /// is a protocol failure.
    Oversized,
}

/// Splits a byte stream into lines, keeping what is left of an unfinished
/// line for the next read. A line longer than [`MAX_LINE`] is never handed
/// out, in part or whole: it is reported once as [`Frame::Oversized`] and
/// discarded through the next newline, so its tail cannot be read as a line
/// either.
#[derive(Debug, Default)]
pub(crate) struct Lines {
    pending: Vec<u8>,
    /// Inside an oversized line already reported: discard up to and
    /// including the next newline.
    discarding: bool,
}

impl Lines {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Vec<Frame> {
        self.pending.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(at) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=at).collect();
            let line = &line[..line.len() - 1];
            if std::mem::take(&mut self.discarding) {
                continue;
            }
            out.push(if line.len() > MAX_LINE {
                Frame::Oversized
            } else {
                Frame::Line(String::from_utf8_lossy(line).into_owned())
            });
        }
        if self.pending.len() > MAX_LINE {
            if !self.discarding {
                out.push(Frame::Oversized);
                self.discarding = true;
            }
            self.pending.clear();
        } else if self.discarding {
            self.pending.clear();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> Hello {
        Hello {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "echo 'a b'\n".into()],
            env: vec![("PATH".into(), "/bin".into()), ("X".into(), "a=b".into())],
            cwd: Some("/tmp".into()),
            stdin: true,
            grace: Duration::from_millis(1500),
            deadline: Duration::from_millis(10_000),
            ceiling: Duration::from_millis(70_000),
            marker: "00ff".into(),
        }
    }

    #[test]
    fn a_hello_reads_back_as_written() {
        let h = hello();
        let bytes = h.encode();
        assert_eq!(Hello::total_len(&bytes[..8]), Ok(Some(bytes.len())));
        assert_eq!(Hello::total_len(&bytes[..3]), Ok(None));
        assert_eq!(Hello::decode(&bytes), Ok(h.clone()));
        assert_eq!(h.descriptors(), 3);
    }

    #[test]
    fn a_damaged_hello_is_refused() {
        let bytes = hello().encode();
        assert!(Hello::decode(&bytes[..bytes.len() - 1]).is_err());
        let mut wrong = bytes.clone();
        wrong[0] = b'X';
        assert!(Hello::decode(&wrong).is_err());
    }

    #[test]
    fn keeper_lines_read_back_as_written() {
        for msg in [
            FromKeeper::Subreaper(Ok(())),
            FromKeeper::Subreaper(Err("EPERM no".into())),
            FromKeeper::Started(42),
            FromKeeper::SpawnFailed {
                errno: 2,
                message: "No such file".into(),
            },
            FromKeeper::ExitedCode(3),
            FromKeeper::ExitedSignal(9),
            FromKeeper::Tree {
                id: 7,
                live: 2,
                detail: String::new(),
            },
            FromKeeper::Tree {
                id: 8,
                live: 0,
                detail: "passes=3 complete=true refused=1".into(),
            },
            FromKeeper::Reaped {
                live: 0,
                proven: false,
                unproven: false,
                incomplete: false,
            },
            FromKeeper::Reaped {
                live: 0,
                proven: true,
                unproven: false,
                incomplete: false,
            },
            FromKeeper::Reaped {
                live: 0,
                proven: true,
                unproven: true,
                incomplete: false,
            },
            FromKeeper::Reaped {
                live: 3,
                proven: false,
                unproven: true,
                incomplete: false,
            },
            FromKeeper::Reaped {
                live: 0,
                proven: true,
                unproven: false,
                incomplete: true,
            },
            FromKeeper::Reaped {
                live: 1,
                proven: false,
                unproven: true,
                incomplete: true,
            },
            FromKeeper::Releasing,
            FromKeeper::Warn("x y".into()),
        ] {
            assert_eq!(FromKeeper::parse(&msg.line()), Some(msg.clone()), "{msg:?}");
        }
        for msg in [
            ToKeeper::Census(1),
            ToKeeper::Freeze(2),
            ToKeeper::Reap,
            ToKeeper::Released,
        ] {
            assert_eq!(ToKeeper::parse(&msg.line()), Some(msg));
        }
        assert_eq!(FromKeeper::parse("nonsense"), None);
        // Only the exact word claims a proof.
        assert_eq!(FromKeeper::parse("reaped 0 provenx"), None);
        assert_eq!(FromKeeper::parse("reaped 0 proven extra"), None);
        assert_eq!(FromKeeper::parse("reaped 0 unproven proven"), None);
        assert_eq!(FromKeeper::parse("reaped 0 unprovenx"), None);
        assert_eq!(FromKeeper::parse("reaped 0 incomplete unproven"), None);
        assert_eq!(FromKeeper::parse("reaped 0 incomplete proven"), None);
        assert_eq!(FromKeeper::parse("reaped 0 proven proven"), None);
        assert_eq!(FromKeeper::parse("reaped 0 incomplete incomplete"), None);
        assert_eq!(FromKeeper::parse("reaped 0 incompletex"), None);
        // A missing or non-numeric count is not zero.
        assert_eq!(FromKeeper::parse("reaped  proven"), None);
        assert_eq!(FromKeeper::parse("reaped proven"), None);
        assert_eq!(FromKeeper::parse("reaped"), None);
        assert_eq!(FromKeeper::parse("reaped "), None);
    }

    fn line(text: &str) -> Frame {
        Frame::Line(text.to_string())
    }

    #[test]
    fn lines_are_split_across_reads() {
        let mut lines = Lines::default();
        assert!(lines.push(b"star").is_empty());
        assert_eq!(
            lines.push(b"ted 1\nreaped\nx"),
            vec![line("started 1"), line("reaped")]
        );
        assert_eq!(lines.push(b"\n"), vec![line("x")]);
    }

    /// A line of exactly [`MAX_LINE`] bytes is a line; one byte more is not.
    #[test]
    fn a_line_up_to_the_limit_is_a_line() {
        let mut lines = Lines::default();
        let full = "a".repeat(MAX_LINE);
        assert_eq!(
            lines.push(format!("{full}\n").as_bytes()),
            vec![line(&full)]
        );
        assert_eq!(
            lines.push(format!("{full}b\nok\n").as_bytes()),
            vec![Frame::Oversized, line("ok")]
        );
    }

    /// An oversized line that arrives unterminated is reported once and
    /// nothing of it is handed out as a line: not the part before the limit,
    /// not the tail that follows in later reads, up to its newline. The line
    /// after it is read as usual.
    #[test]
    fn an_oversized_unterminated_line_is_never_handed_out() {
        let mut lines = Lines::default();
        let padded = format!(
            "tree 3 1 passes=1 complete=false refused=0 read_failures=0{}",
            " ".repeat(MAX_LINE)
        );
        assert_eq!(lines.push(padded.as_bytes()), vec![Frame::Oversized]);
        assert!(lines.push(&[b' '; 100]).is_empty());
        assert!(lines.push(&vec![b' '; MAX_LINE * 2]).is_empty());
        assert_eq!(
            lines.push(b" task=failed\nexited code 0\n"),
            vec![line("exited code 0")]
        );
        assert_eq!(lines.push(b"reaped 0\n"), vec![line("reaped 0")]);
    }

    /// No line a side writes is oversized, whatever text it carries.
    #[test]
    fn a_long_text_is_cut_to_fit_one_line() {
        for text in ["x".repeat(MAX_LINE * 2), "\u{1F600}".repeat(MAX_LINE)] {
            for msg in [
                FromKeeper::Warn(text.clone()),
                FromKeeper::SpawnFailed {
                    errno: i32::MAX,
                    message: text.clone(),
                },
                FromKeeper::Subreaper(Err(text.clone())),
                FromKeeper::Tree {
                    id: u64::MAX,
                    live: usize::MAX,
                    detail: text.clone(),
                },
            ] {
                let written = msg.line();
                assert!(written.len() <= MAX_LINE, "{}", written.len());
                let mut lines = Lines::default();
                assert_eq!(
                    lines.push(format!("{written}\n").as_bytes()),
                    vec![line(&written)]
                );
            }
        }
    }
}
