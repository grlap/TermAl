// Host-owned raw review identity protocol. Owns strict request admission,
// canonical framing, bounded double capture and inventory paging. Git/FS
// readers are separate fragments; no project helper or policy code is loaded.
const REVIEW_IDENTITY_ALGORITHM: &str = "termal-review-identity-v1";
const REVIEW_IDENTITY_RECORD_LIMIT: usize = 200_000;
const REVIEW_IDENTITY_INVENTORY_LIMIT: usize = 8 * 1024 * 1024;
const REVIEW_IDENTITY_PAYLOAD_LIMIT: usize = 128 * 1024 * 1024;
const REVIEW_IDENTITY_AGGREGATE_LIMIT: usize = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReviewIdentityRequest {
    operation: String,
    schema_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    platform_profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    page_limit: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor: Option<String>,
}

#[derive(Debug)]
struct ReviewIdentityRefusal(&'static str);
impl std::fmt::Display for ReviewIdentityRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for ReviewIdentityRefusal {}
fn identity_refusal(code: &'static str) -> anyhow::Error {
    anyhow!(ReviewIdentityRefusal(code))
}
fn identity_error_code(error: &anyhow::Error) -> &'static str {
    if let Some(reason) = error.downcast_ref::<ReviewIdentityRefusal>() {
        reason.0
    } else {
        let message = format!("{error:#}");
        if message.contains("deadline") {
            "deadline_exceeded"
        } else if message.contains("limit exceeded") {
            "input_limit_exceeded"
        } else {
            "input_unavailable"
        }
    }
}
fn identity_failure(code: &str) -> Value {
    json!({"schemaVersion":1,"algorithm":REVIEW_IDENTITY_ALGORITHM,
        "complete":false,"verified":false,"reason":{"code":code}})
}
fn identity_platform() -> &'static str {
    if cfg!(windows) { "windows" } else { "unix" }
}
impl ReviewIdentityRequest {
    fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            return Err(identity_refusal("unsupported_schema"));
        }
        if !matches!(self.operation.as_str(), "capture" | "check")
            || self
                .page_limit
                .is_some_and(|limit| limit == 0 || limit > 100)
            || self
                .cursor
                .as_ref()
                .is_some_and(|cursor| cursor.len() > 8192)
            || self.root.as_ref().is_some_and(|root| {
                root.is_empty() || root.len() > 4096 || root.chars().any(char::is_control)
            })
        {
            return Err(identity_refusal("invalid_request"));
        }
        if self.operation == "capture" {
            if self.expected_fingerprint.is_some()
                || self.platform_profile.is_some()
                || self.cursor.is_some()
            {
                return Err(identity_refusal("invalid_request"));
            }
        } else if self.root.is_some()
            || !self
                .expected_fingerprint
                .as_ref()
                .is_some_and(|hash| is_lowercase_sha256(hash))
            || !self
                .platform_profile
                .as_deref()
                .is_some_and(|profile| matches!(profile, "windows" | "unix"))
        {
            return Err(identity_refusal("invalid_request"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct ReviewIdentityEntry {
    path: String,
    layer: String,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    stage: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    oid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    flags: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    object_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fs_mode: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
}
impl ReviewIdentityEntry {
    fn new(path: String, layer: &str, kind: &str) -> Self {
        Self {
            path,
            layer: layer.to_owned(),
            kind: kind.to_owned(),
            ..Self::default()
        }
    }
    fn digest(&self) -> String {
        let mut hash = Sha256::new();
        for (tag, value) in [
            ("domain", "termal-review-entry-v1"),
            ("path", self.path.as_str()),
            ("layer", &self.layer),
            ("kind", &self.kind),
        ] {
            review_freeze_chunk(&mut hash, tag, value.as_bytes());
        }
        // Absent and present values have distinct tags; optional field order
        // is fixed by this list, never by a JSON map or serializer.
        let stage = self.stage.map(|v| v.to_string());
        let size = self.size.map(|v| v.to_string());
        let mode = self.fs_mode.as_ref().map(|v| {
            if cfg!(windows) {
                "windows:none".to_owned()
            } else {
                format!("unix:{}", v["unix"].as_u64().unwrap_or(0))
            }
        });
        for (tag, value) in [
            ("stage", stage.as_deref()),
            ("mode", self.mode.as_deref()),
            ("oid", self.oid.as_deref()),
            ("flags", self.flags.as_deref()),
            ("objectFormat", self.object_format.as_deref()),
            ("size", size.as_deref()),
            ("content", self.content_sha256.as_deref()),
            ("fsMode", mode.as_deref()),
            ("target", self.target.as_deref()),
        ] {
            review_freeze_chunk(
                &mut hash,
                &format!(
                    "{tag}:{}",
                    if value.is_some() { "present" } else { "absent" }
                ),
                value.unwrap_or("").as_bytes(),
            );
        }
        format!("{:x}", hash.finalize())
    }
}

struct ReviewIdentityCapture {
    entries: Vec<ReviewIdentityEntry>,
    repositories: usize,
    bytes: usize,
    encoded: usize,
    visited_entries: usize,
    seen_repositories: HashSet<String>,
}
impl ReviewIdentityCapture {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            repositories: 0,
            bytes: 0,
            encoded: 0,
            visited_entries: 0,
            seen_repositories: HashSet::new(),
        }
    }
    fn add(&mut self, entry: ReviewIdentityEntry) -> Result<()> {
        self.encoded += serde_json::to_vec(&entry)?.len() + 1;
        if self.entries.len() >= REVIEW_IDENTITY_RECORD_LIMIT
            || self.encoded > REVIEW_IDENTITY_INVENTORY_LIMIT
        {
            return Err(identity_refusal("input_limit_exceeded"));
        }
        self.entries.push(entry);
        Ok(())
    }
    fn consume(&mut self, bytes: usize) -> Result<()> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .ok_or_else(|| identity_refusal("input_limit_exceeded"))?;
        if bytes > REVIEW_IDENTITY_PAYLOAD_LIMIT || self.bytes > REVIEW_IDENTITY_AGGREGATE_LIMIT {
            return Err(identity_refusal("input_limit_exceeded"));
        }
        Ok(())
    }
    fn visit(&mut self) -> Result<()> {
        self.visited_entries += 1;
        if self.visited_entries > REVIEW_IDENTITY_RECORD_LIMIT {
            return Err(identity_refusal("input_limit_exceeded"));
        }
        Ok(())
    }
    fn finish(&mut self) -> String {
        self.entries
            .sort_by(|a, b| (&a.path, &a.layer, a.stage).cmp(&(&b.path, &b.layer, b.stage)));
        let mut hash = Sha256::new();
        review_freeze_chunk(&mut hash, "domain", b"termal-review-inventory-v1");
        for entry in &self.entries {
            review_freeze_chunk(&mut hash, "entry", entry.digest().as_bytes());
        }
        format!("{:x}", hash.finalize())
    }
}

fn identity_composite(root: &str, content: &str, platform: &str) -> String {
    let mut hash = Sha256::new();
    for (tag, value) in [
        ("domain", REVIEW_IDENTITY_ALGORITHM),
        ("schema", "1"),
        ("root", root),
        ("content", content),
        ("platform", platform),
    ] {
        review_freeze_chunk(&mut hash, tag, value.as_bytes());
    }
    format!("{:x}", hash.finalize())
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReviewIdentityCursor {
    fingerprint: String,
    root: String,
    next: usize,
}

#[cfg(test)]
fn identity_result(root: &FsPath, request: &ReviewIdentityRequest) -> Result<Value> {
    identity_result_until(
        root,
        request,
        std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT,
    )
}
fn identity_result_until(
    root: &FsPath,
    request: &ReviewIdentityRequest,
    deadline: std::time::Instant,
) -> Result<Value> {
    request.validate()?;
    if request
        .platform_profile
        .as_deref()
        .is_some_and(|profile| profile != identity_platform())
    {
        return Err(identity_refusal("platform_profile_mismatch"));
    }
    let git = ReviewIdentityGit::new_until(root, deadline)?;
    let canonical = identity_utf8_path(&git.root)?;
    if let Some(requested) = &request.root
        && fs::canonicalize(requested)? != git.root
    {
        return Err(identity_refusal("root_mismatch"));
    }
    let mut first = ReviewIdentityCapture::new();
    identity_repository(&git, "", 0, &mut first)?;
    let content = first.finish();
    identity_between_captures(&git.root);
    let mut second = ReviewIdentityCapture::new();
    identity_repository(&git, "", 0, &mut second)?;
    if second.finish() != content {
        return Err(identity_refusal("concurrent_change"));
    }
    git.check_deadline()?;
    let fingerprint = identity_composite(&canonical, &content, identity_platform());
    if request
        .expected_fingerprint
        .as_ref()
        .is_some_and(|expected| expected != &fingerprint)
    {
        return Err(identity_refusal("fingerprint_mismatch"));
    }
    let start = if let Some(cursor) = &request.cursor {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(cursor)
            .map_err(|_| identity_refusal("invalid_cursor"))?;
        let cursor: ReviewIdentityCursor =
            serde_json::from_slice(&bytes).map_err(|_| identity_refusal("invalid_cursor"))?;
        if cursor.fingerprint != fingerprint
            || cursor.root != canonical
            || cursor.next >= first.entries.len()
        {
            return Err(identity_refusal("stale_cursor"));
        }
        cursor.next
    } else {
        0
    };
    let limit = request.page_limit.unwrap_or(100);
    let mut end = start;
    // Reserve envelope/cursor space as well as entry bytes within the page.
    let mut page_bytes = 16 * 1024;
    while end < first.entries.len() && end - start < limit {
        let size = serde_json::to_vec(&first.entries[end])?.len() + 1;
        if page_bytes + size > 64 * 1024 {
            break;
        }
        page_bytes += size;
        end += 1;
    }
    if end == start && start < first.entries.len() {
        return Err(identity_refusal("input_limit_exceeded"));
    }
    let next = if end < first.entries.len() {
        Some(
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(
                &ReviewIdentityCursor {
                    fingerprint: fingerprint.clone(),
                    root: canonical.clone(),
                    next: end,
                },
            )?),
        )
    } else {
        None
    };
    Ok(
        json!({"schemaVersion":1,"algorithm":REVIEW_IDENTITY_ALGORITHM,
        "canonicalRoot":canonical,"platformProfile":identity_platform(),"objectFormat":git.object_format,
        "contentDigest":content,"fingerprint":fingerprint,"complete":true,
        "verified":if request.operation=="check" {Some(true)} else {None},
        "counts":{"repositories":first.repositories,"files":first.entries.iter().filter(|e|e.layer=="worktree").count(),"records":first.entries.len(),"rawBytes":first.bytes},
        "inventoryDigest":content,"inventory":{"totalRecords":first.entries.len(),"entries":&first.entries[start..end],"nextCursor":next}}),
    )
}

#[cfg(not(test))]
fn identity_between_captures(_: &FsPath) {}
#[cfg(test)]
thread_local! { static IDENTITY_CAPTURE_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce(&FsPath)>>> = const { std::cell::RefCell::new(None) }; }
#[cfg(test)]
fn identity_between_captures(root: &FsPath) {
    IDENTITY_CAPTURE_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook(root);
        }
    });
}

fn review_identity_mode(args: &[String]) -> Result<()> {
    let deadline = std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT;
    let result = (|| {
        if !matches!(args.len(), 3 | 4) || args[2].len() > 16 * 1024 {
            return Err(identity_refusal("invalid_request"));
        }
        let request: ReviewIdentityRequest =
            serde_json::from_str(&args[2]).map_err(|_| identity_refusal("invalid_request"))?;
        let registration = if args.len() == 4 {
            if args[3].len() > 16 * 1024 || request.operation != "capture" {
                return Err(identity_refusal("invalid_request"));
            }
            let scope: ReviewIdentityScope =
                serde_json::from_str(&args[3]).map_err(|_| identity_refusal("invalid_request"))?;
            Some(scope.admit(FsPath::new(&args[1]))?)
        } else {
            None
        };
        let mut value = identity_result_until(FsPath::new(&args[1]), &request, deadline)?;
        if let Some(registration) = registration
            && identity_registered_common(&fs::canonicalize(&args[1])?)? != registration
        {
            return Err(identity_refusal("concurrent_change"));
        }
        value["requestedRoot"] = json!(args[1]);
        if std::time::Instant::now() >= deadline {
            return Err(identity_refusal("deadline_exceeded"));
        }
        Ok(value)
    })();
    match result {
        Ok(value) => {
            println!("{}", serde_json::to_string(&value)?);
            Ok(())
        }
        Err(error) => {
            println!("{}", identity_failure(identity_error_code(&error)));
            Err(error)
        }
    }
}
