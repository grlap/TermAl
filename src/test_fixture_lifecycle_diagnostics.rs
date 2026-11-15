// Diagnostic candidate only: compact attribution for fixture roots and SQLite.
// Keeps no fixture owner, worker or AppState alive. Opt-in, test-only output.
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const PROCESS_SLOTS: usize = 1024;
const INITIAL_BYTES: usize = 64 * 1024;
const GROWTH_SLOTS: usize = 240;
const GROWTH_BYTES: usize = 4 * 1024 * 1024;
// Small initial reservations plus growth reservations total exactly 1 GiB.
// Schema-only children do not reserve a large slot needed by another process.
// Atomic create_new tickets remain reserved until this one job is removed.
const TOTAL_BYTES: usize = PROCESS_SLOTS * INITIAL_BYTES + GROWTH_SLOTS * GROWTH_BYTES;
const PROCESS_BYTES: usize = 128 * 1024 * 1024;
const RESERVED_MARKER_BYTES: usize = 4096;
const ROOTS: usize = 32768;
const THREADS: usize = 32768;
const STACK_BYTES: usize = 32768;

#[derive(Default)]
struct RootLife {
    id: usize,
    creator_recorded: bool,
    drop_started: bool,
    removed: bool,
    opens: u64,
    late_opens: usize,
}

struct Trace {
    directory: PathBuf,
    file: File,
    bytes: usize,
    reserved: usize,
    limited: bool,
    roots: BTreeMap<PathBuf, RootLife>,
    threads: BTreeMap<String, usize>,
    stacks: BTreeMap<String, usize>,
}

fn bounded(value: &str, bytes: usize) -> String {
    let mut end = value.len().min(bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

// Fixed-name empty flags remain observable when a process cannot obtain a
// trace slot or has spent its own reservation. No unbounded failure log.
fn flag(directory: &Path, reason: &'static str) {
    let _ = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join(reason));
}

fn trace() -> Option<&'static Mutex<Trace>> {
    static TRACE: OnceLock<Option<Mutex<Trace>>> = OnceLock::new();
    TRACE
        .get_or_init(|| {
            let requested = PathBuf::from(std::env::var_os("TERMAL_TEST_LIFECYCLE_TRACE_DIR")?);
            let temp = fs::canonicalize(std::env::var_os("TERMAL_TEST_USER_TEMP")?).ok()?;
            if !requested.is_absolute() || !temp.is_absolute() {
                return None;
            }
            let directory = fs::canonicalize(&requested).ok()?;
            if directory != requested || !directory.starts_with(&temp) {
                return None;
            }
            if let Some(run) = std::env::var_os("TERMAL_TEST_RUN_ROOT") {
                if directory.starts_with(PathBuf::from(run)) {
                    return None;
                }
            }
            for slot in 0..PROCESS_SLOTS {
                let path = directory.join(format!("lifecycle-{slot}.jsonl"));
                match OpenOptions::new().write(true).create_new(true).open(path) {
                    Ok(file) => {
                        let mut trace = Trace {
                            directory,
                            file,
                            bytes: 0,
                            reserved: INITIAL_BYTES,
                            limited: false,
                            roots: BTreeMap::new(),
                            threads: BTreeMap::new(),
                            stacks: BTreeMap::new(),
                        };
                        trace.write(serde_json::json!({
                            "e":"start","version":3,"pid":std::process::id(),
                            "processSlot":slot,"processSlots":PROCESS_SLOTS,
                            "initialBytes":INITIAL_BYTES,"growthSlots":GROWTH_SLOTS,
                            "growthBytes":GROWTH_BYTES,"totalBytes":TOTAL_BYTES,
                            "processBytes":PROCESS_BYTES,"roots":ROOTS,
                            "threads":THREADS,"stackBytes":STACK_BYTES,
                        }));
                        return Some(Mutex::new(trace));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(_) => {
                        flag(&directory, "trace-io-error");
                        return None;
                    }
                }
            }
            flag(&directory, "trace-process-slot-limit");
            None
        })
        .as_ref()
}

impl Trace {
    fn stop(&mut self, reason: &'static str) {
        if self.limited {
            return;
        }
        flag(&self.directory, reason);
        // Every ordinary event leaves fixed room for a terminal marker.
        let mut line = serde_json::to_vec(&serde_json::json!({
            "e":"limit","reason":reason,"pid":std::process::id(),
            "t":chrono::Utc::now().timestamp_millis(),
        }))
        .unwrap_or_default();
        line.push(b'\n');
        if self.bytes + line.len() <= self.reserved {
            if self.file.write_all(&line).is_ok() {
                self.bytes += line.len();
            } else {
                flag(&self.directory, "trace-io-error");
            }
        }
        self.limited = true;
    }

    fn grow_for(&mut self, needed: usize) -> bool {
        if needed > PROCESS_BYTES {
            self.stop("trace-process-byte-limit");
            return false;
        }
        while needed > self.reserved {
            let mut acquired = false;
            for slot in 0..GROWTH_SLOTS {
                match OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(self.directory.join(format!("budget-{slot}")))
                {
                    Ok(_) => {
                        self.reserved = (self.reserved + GROWTH_BYTES).min(PROCESS_BYTES);
                        acquired = true;
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(_) => {
                        self.stop("trace-io-error");
                        return false;
                    }
                }
            }
            if !acquired {
                self.stop("trace-total-byte-limit");
                return false;
            }
        }
        true
    }

    fn write(&mut self, value: serde_json::Value) {
        if self.limited {
            return;
        }
        let Ok(mut line) = serde_json::to_vec(&value) else {
            self.stop("trace-serialization-error");
            return;
        };
        line.push(b'\n');
        if !self.grow_for(self.bytes + line.len() + RESERVED_MARKER_BYTES) {
            return;
        }
        if self.file.write_all(&line).is_ok() {
            self.bytes += line.len();
        } else {
            flag(&self.directory, "trace-io-error");
            self.limited = true;
        }
    }

    fn thread(&mut self) -> Option<usize> {
        let thread = std::thread::current();
        let key = format!("{:?}", thread.id());
        if let Some(id) = self.threads.get(&key) {
            return Some(*id);
        }
        if self.threads.len() >= THREADS {
            self.stop("trace-thread-limit");
            return None;
        }
        let name = thread.name().unwrap_or("unnamed");
        if name.len() > 256 {
            flag(&self.directory, "trace-thread-name-truncated");
        }
        let id = self.threads.len();
        self.threads.insert(key.clone(), id);
        self.write(serde_json::json!({
            "e":"thread","h":id,"id":key,"name":bounded(name,256),
        }));
        Some(id)
    }

    fn event(&mut self, mut value: serde_json::Value) {
        let Some(thread) = self.thread() else { return };
        value["h"] = serde_json::json!(thread);
        value["t"] = serde_json::json!(chrono::Utc::now().timestamp_millis());
        self.write(value);
    }

    fn root(&mut self, path: &Path) -> Option<usize> {
        if let Some(life) = self.roots.get(path) {
            return Some(life.id);
        }
        if path.to_string_lossy().len() > 4096 {
            self.stop("trace-path-limit");
            return None;
        }
        if path.to_str().is_none() {
            flag(&self.directory, "trace-path-not-utf8");
        }
        if self.roots.len() >= ROOTS {
            self.stop("trace-root-limit");
            return None;
        }
        let id = self.roots.len();
        self.roots.insert(
            path.to_owned(),
            RootLife {
                id,
                ..RootLife::default()
            },
        );
        self.event(serde_json::json!({"e":"root","r":id,"path":path.to_string_lossy()}));
        Some(id)
    }

    fn creator_stack(&mut self) -> usize {
        let full = std::backtrace::Backtrace::force_capture().to_string();
        let stack = bounded(&full, STACK_BYTES);
        let truncated = full.len() > STACK_BYTES;
        if truncated {
            flag(&self.directory, "trace-stack-truncated");
        }
        let hash = crate::sha256_hex(stack.as_bytes());
        if let Some(id) = self.stacks.get(&hash) {
            return *id;
        }
        // At most one stack registration per owned root, itself bounded.
        let id = self.stacks.len();
        self.stacks.insert(hash, id);
        self.write(serde_json::json!({
            "e":"creator_stack","s":id,"stack":stack,"truncated":truncated,
        }));
        id
    }
}

pub(crate) fn root_event(path: &Path, event: &'static str, removed: Option<bool>) {
    let Some(trace) = trace() else { return };
    let mut trace = trace
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if trace.limited {
        return;
    }
    let Some(id) = trace.root(path) else { return };
    if !trace.roots[path].creator_recorded {
        let stack = trace.creator_stack();
        trace.roots.get_mut(path).unwrap().creator_recorded = true;
        trace.event(serde_json::json!({"e":"creator","r":id,"s":stack}));
    }
    let life = trace.roots.get_mut(path).unwrap();
    if event == "root_drop_begin" {
        life.drop_started = true;
    }
    if let Some(removed) = removed {
        life.removed = removed;
    }
    let detail = serde_json::json!({
        "e":event,"r":id,"exists":path.exists(),"removed":removed,"opens":life.opens,
    });
    trace.event(detail);
}

pub(crate) fn sqlite_open(path: &Path, mode: &'static str) {
    let Some(trace) = trace() else { return };
    let mut trace = trace
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if trace.limited {
        return;
    }
    if path.to_string_lossy().len() > 4096 {
        trace.stop("trace-path-limit");
        return;
    }
    if path.to_str().is_none() {
        flag(&trace.directory, "trace-path-not-utf8");
    }
    let root = trace
        .roots
        .keys()
        .filter(|root| path.starts_with(root))
        .max_by_key(|root| root.components().count())
        .cloned()
        .or_else(|| path.parent().map(Path::to_path_buf));
    let Some(root) = root else { return };
    let Some(id) = trace.root(&root) else { return };
    let life = trace.roots.get_mut(&root).unwrap();
    life.opens = life.opens.saturating_add(1);
    let late = life.drop_started || life.removed || !root.exists();
    if life.opens != 1 && !late {
        return;
    }
    if late && life.late_opens >= 8 {
        flag(&trace.directory, "trace-late-open-limit");
        return;
    }
    if late {
        life.late_opens += 1;
    }
    let detail = serde_json::json!({
        "e":"sqlite_open_begin","r":id,"mode":mode,
        "database":path.strip_prefix(&root).unwrap_or(path).to_string_lossy(),
        "exists":path.exists(),"late":late,"opens":life.opens,
        "dropStarted":life.drop_started,"removed":life.removed,
    });
    trace.event(detail);
}
