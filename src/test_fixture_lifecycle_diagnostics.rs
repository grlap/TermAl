// Diagnostic candidate only: opt-in test resource attribution. No worker,
// fixture ownership, production behavior, stdout or panic-journal changes.
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const SLOTS: usize = 8;
const BYTES_PER_SLOT: usize = 128 * 1024 * 1024;
const ROOTS: usize = 32768;
const STACK_BYTES: usize = 32768;

#[derive(Default)]
struct RootLife {
    creator: String,
    drop_started: bool,
    removed: bool,
    opens: u64,
    late_opens: usize,
}

struct Trace {
    file: File,
    bytes: usize,
    limited: bool,
    roots: BTreeMap<PathBuf, RootLife>,
}

fn bounded(value: &str, bytes: usize) -> String {
    let mut end = value.len().min(bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn trace() -> Option<&'static Mutex<Trace>> {
    static TRACE: OnceLock<Option<Mutex<Trace>>> = OnceLock::new();
    TRACE.get_or_init(|| {
        let requested = PathBuf::from(std::env::var_os("TERMAL_TEST_LIFECYCLE_TRACE_DIR")?);
        let temp = fs::canonicalize(std::env::var_os("TERMAL_TEST_USER_TEMP")?).ok()?;
        if !requested.is_absolute() || !temp.is_absolute() { return None; }
        // The runner creates this private directory before launch. Do not
        // create anything through a link or outside its existing job temp.
        let directory = fs::canonicalize(&requested).ok()?;
        if directory != requested || !directory.starts_with(&temp) { return None; }
        if let Some(run) = std::env::var_os("TERMAL_TEST_RUN_ROOT") {
            if directory.starts_with(PathBuf::from(run)) { return None; }
        }
        for slot in 0..SLOTS {
            match OpenOptions::new().write(true).create_new(true)
                .open(directory.join(format!("lifecycle-{slot}.jsonl"))) {
                Ok(file) => {
                    let mut trace = Trace { file, bytes: 0, limited: false, roots: BTreeMap::new() };
                    trace.write(serde_json::json!({"event":"trace_started","pid":std::process::id(),
                        "slots":SLOTS,"bytesPerSlot":BYTES_PER_SLOT,"rootsPerProcess":ROOTS,"stackBytes":STACK_BYTES}));
                    return Some(Mutex::new(trace));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(_) => return None,
            }
        }
        None
    }).as_ref()
}

impl Trace {
    fn write(&mut self, value: serde_json::Value) {
        if self.limited {
            return;
        }
        let Ok(mut line) = serde_json::to_vec(&value) else {
            return;
        };
        line.push(b'\n');
        if self.bytes + line.len() > BYTES_PER_SLOT - 1024 {
            let line = b"{\"event\":\"trace_byte_limit\"}\n";
            let _ = self.file.write_all(line);
            self.bytes += line.len();
            self.limited = true;
            return;
        }
        if self.file.write_all(&line).is_ok() {
            self.bytes += line.len();
        } else {
            self.limited = true;
        }
    }

    fn event(&mut self, event: &str, root: &Path, detail: serde_json::Value) {
        let full_stack = std::backtrace::Backtrace::force_capture().to_string();
        let thread = std::thread::current();
        self.write(serde_json::json!({
            "event":event,"pid":std::process::id(),
            "thread":bounded(thread.name().unwrap_or("unnamed"),256),
            "threadId":format!("{:?}",thread.id()),
            "at":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true),
            "root":bounded(&root.to_string_lossy(),4096),
            "rootExists":root.exists(),"detail":detail,
            "stack":bounded(&full_stack,STACK_BYTES),"stackTruncated":full_stack.len()>STACK_BYTES,
        }));
    }
}

pub(crate) fn root_event(path: &Path, event: &'static str, removed: Option<bool>) {
    let Some(trace) = trace() else {
        return;
    };
    let mut trace = trace
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if trace.limited {
        return;
    }
    if path.as_os_str().len() > 4096 {
        trace.write(serde_json::json!({"event":"trace_path_limit","pid":std::process::id()}));
        trace.limited = true;
        return;
    }
    if !trace.roots.contains_key(path) && trace.roots.len() >= ROOTS {
        trace.write(serde_json::json!({"event":"trace_root_limit","pid":std::process::id()}));
        trace.limited = true;
        return;
    }
    let life = trace
        .roots
        .entry(path.to_owned())
        .or_insert_with(|| RootLife {
            creator: bounded(std::thread::current().name().unwrap_or("unnamed"), 256),
            ..RootLife::default()
        });
    if event == "root_drop_begin" {
        life.drop_started = true;
    }
    if let Some(removed) = removed {
        life.removed = removed;
    }
    let detail = serde_json::json!({"creator":life.creator,"dropStarted":life.drop_started,
        "removed":life.removed,"sqliteOpens":life.opens});
    trace.event(event, path, detail);
}

pub(crate) fn sqlite_open(path: &Path, mode: &'static str) {
    let Some(trace) = trace() else {
        return;
    };
    let mut trace = trace
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if trace.limited {
        return;
    }
    if path.as_os_str().len() > 4096 {
        trace.write(serde_json::json!({"event":"trace_path_limit","pid":std::process::id()}));
        trace.limited = true;
        return;
    }
    let root = trace
        .roots
        .keys()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count())
        .cloned();
    let root = root.or_else(|| path.parent().map(Path::to_path_buf));
    let Some(root) = root else {
        return;
    };
    if !trace.roots.contains_key(&root) && trace.roots.len() >= ROOTS {
        trace.write(serde_json::json!({"event":"trace_root_limit","pid":std::process::id()}));
        trace.limited = true;
        return;
    }
    let life = trace.roots.entry(root.clone()).or_default();
    life.opens = life.opens.saturating_add(1);
    let late = life.drop_started || life.removed || !root.exists();
    // One healthy opening proves the caller; late openings retain their
    // original call stacks, with a per-root cap as well as the byte bound.
    if life.opens != 1 && !late {
        return;
    }
    if late {
        if life.late_opens >= 8 {
            return;
        }
        life.late_opens += 1;
    }
    let detail = serde_json::json!({"creator":life.creator,"mode":mode,
        "database":bounded(&path.to_string_lossy(),4096),"databaseExists":path.exists(),
        "dropStarted":life.drop_started,"removed":life.removed,"late":late,"openOrdinal":life.opens});
    trace.event("sqlite_open_begin", &root, detail);
}
