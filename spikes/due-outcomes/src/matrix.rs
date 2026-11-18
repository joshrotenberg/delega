//! The case matrix: which outcome case runs on which path, provider and
//! cleanup variant.

use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Path {
    OneShot,
    Resident,
}

impl Path {
    pub fn name(self) -> &'static str {
        match self {
            Path::OneShot => "oneshot",
            Path::Resident => "resident",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prov {
    Claude,
    Codex,
}

impl Prov {
    pub fn name(self) -> &'static str {
        match self {
            Prov::Claude => "claude",
            Prov::Codex => "codex",
        }
    }
}

/// One outcome case. `letter` is what the fake provider sees.
#[derive(Clone, Copy, Debug)]
pub struct Case {
    pub id: &'static str,
    pub letter: char,
    /// Assignment timeout handed to due.
    pub timeout: Option<Duration>,
    pub desc: &'static str,
}

const fn case(id: &'static str, letter: char, timeout_s: u64, desc: &'static str) -> Case {
    Case {
        id,
        letter,
        timeout: if timeout_s == 0 {
            None
        } else {
            Some(Duration::from_secs(timeout_s))
        },
        desc,
    }
}

pub const CASES: &[Case] = &[
    case("a", 'a', 0, "normal completion"),
    case("b", 'b', 0, "provider exits 3 with an error"),
    case(
        "c",
        'c',
        2,
        "assignment timeout 2 s, provider keeps producing output",
    ),
    case("d", 'd', 0, "caller interrupts at 1 s"),
    case("e", 'e', 0, "provider SIGKILLed mid-run"),
    case("ed", 'e', 3, "provider SIGKILLed mid-run, timeout 3 s"),
    case("f", 'f', 0, "provider hangs with no output, no deadline"),
    case("fd", 'f', 3, "provider hangs with no output, timeout 3 s"),
    case("g", 'g', 0, "permission asked, nobody answers, no deadline"),
    case(
        "gd",
        'g',
        3,
        "permission asked, nobody answers, timeout 3 s",
    ),
    case("gr", 'g', 0, "permission asked, reject interaction mode"),
    case(
        "h",
        'h',
        0,
        "provider closes stdout, stays alive, no deadline",
    ),
    case(
        "hd",
        'h',
        3,
        "provider closes stdout, stays alive, timeout 3 s",
    ),
    case(
        "i",
        'i',
        0,
        "output ends without a final result, provider stays alive (resident) or exits 0 (one-shot), no deadline",
    ),
    case("id", 'i', 3, "as i, timeout 3 s"),
    case(
        "j",
        'j',
        0,
        "calling task cancelled or wait future dropped at 1 s",
    ),
    case("j2", 'j', 0, "Engine dropped at 1 s (resident only)"),
];

pub const VARIANTS: &[&str] = &["plain", "child", "setsid", "ignore", "stubborn", "pipe"];

#[derive(Clone, Copy, Debug)]
pub struct Spec {
    pub path: Path,
    pub prov: Prov,
    pub case: Case,
    pub variant: &'static str,
    /// One-shot only: use the streaming (observer) code path.
    pub stream: bool,
}

impl Spec {
    pub fn id(&self) -> String {
        format!(
            "{}{}-{}-{}-{}",
            self.path.name(),
            if self.stream { "+stream" } else { "" },
            self.prov.name(),
            self.case.id,
            self.variant
        )
    }

    /// The child gives up on a call after this long and records a hang.
    pub fn hard_limit(&self) -> Duration {
        match self.case.letter {
            'f' | 'g' | 'h' | 'i' if self.case.timeout.is_none() => Duration::from_secs(12),
            _ => Duration::from_secs(30),
        }
    }

    /// A shutdown stage follows the result stage for resident cases.
    pub fn has_shutdown_stage(&self) -> bool {
        self.path == Path::Resident && self.case.id != "j2"
    }
}

pub fn lookup_case(id: &str) -> Option<Case> {
    CASES.iter().copied().find(|c| c.id == id)
}

pub fn lookup_variant(name: &str) -> Option<&'static str> {
    VARIANTS.iter().copied().find(|v| *v == name)
}

/// Every row measured.
pub fn all() -> Vec<Spec> {
    let mut specs = Vec::new();
    // Cases that get all cleanup variants.
    let varied = ["c", "d", "e", "f", "fd", "j", "j2"];
    for path in [Path::OneShot, Path::Resident] {
        for prov in [Prov::Claude, Prov::Codex] {
            for case in CASES {
                if path == Path::OneShot && matches!(case.id, "gd" | "gr" | "j2") {
                    continue;
                }
                let variants: Vec<&'static str> = if case.id == "ed" {
                    vec!["plain", "pipe"]
                } else if varied.contains(&case.id) {
                    VARIANTS
                        .iter()
                        .copied()
                        .filter(|v| !(case.id == "e" && *v == "stubborn"))
                        .filter(|v| !(case.id == "j2" && *v == "stubborn"))
                        .collect()
                } else {
                    vec!["plain"]
                };
                for variant in variants {
                    specs.push(Spec {
                        path,
                        prov,
                        case: *case,
                        variant,
                        stream: false,
                    });
                }
            }
        }
    }
    // One-shot streaming path, plain variant, where the code differs.
    for prov in [Prov::Claude, Prov::Codex] {
        for id in ["a", "b", "c", "d", "e", "fd", "h", "i", "j"] {
            let case = lookup_case(id).unwrap();
            specs.push(Spec {
                path: Path::OneShot,
                prov,
                case,
                variant: "plain",
                stream: true,
            });
        }
    }
    specs
}
