// Bounded native catalog pages with an incrementally maintained child index.
// Cursors describe iteration, never principal or execution authority.
// Lock order is StateInner, then delegation_catalog_children; never reverse.
// The bounded in-memory index/page work holds no lock across I/O or await.

const FEDERATION_CATALOG_SCAN: usize = 64;
const FEDERATION_CATALOG_PAGE: usize = 32;

#[derive(Default)]
struct FederationCatalogChildren {
    by_delegation: BTreeMap<String, String>,
    children: BTreeMap<String, usize>,
    scan: Option<usize>,
    progress: usize,
}

impl FederationCatalogChildren {
    // Constructed from owned restored records before StateInner is published.
    fn from_records(rows: &[DelegationRecord]) -> Self {
        let mut index = Self::default();
        for row in rows { index.observe(&row.id, &row.child_session_id); }
        index
    }

    fn ready(&self, records: usize) -> bool {
        self.scan.is_none() && self.by_delegation.len() == records
    }

    fn removed_at(&mut self, position: usize) {
        if let Some(scan) = self.scan.as_mut() {
            if position < *scan { *scan -= 1; }
        }
    }

    fn observe(&mut self, id: &str, child: &str) {
        if self.by_delegation.get(id).is_some_and(|old| old == child) { return; }
        self.remove(id);
        self.by_delegation.insert(id.to_owned(), child.to_owned());
        *self.children.entry(child.to_owned()).or_default() += 1;
    }

    fn remove(&mut self, id: &str) {
        if let Some(child) = self.by_delegation.remove(id) {
            if let Some(count) = self.children.get_mut(&child) {
                *count -= 1;
                if *count == 0 { self.children.remove(&child); }
            }
        }
    }

    // Direct bulk construction can start cold. Production insertions update
    // the index immediately; removals also shift this bounded scan cursor.
    // Never clear accumulated maps or restart on length/status churn.
    fn prepare(&mut self, rows: &[DelegationRecord]) -> Option<usize> {
        if self.ready(rows.len()) { return None; }
        let start = self.scan.unwrap_or(0).min(rows.len());
        let end = start.saturating_add(FEDERATION_CATALOG_SCAN).min(rows.len());
        for row in &rows[start..end] { self.observe(&row.id, &row.child_session_id); }
        self.progress = self.progress.saturating_add(end - start);
        if end == rows.len() { self.scan = None; None }
        else { self.scan = Some(end); Some(self.progress) }
    }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationCatalogPosition {
    session: usize,
    previous: Option<String>,
    children_progress: usize,
}

fn federation_catalog_root(root: &SessionRecord) -> FederationParticipationRoot {
    FederationParticipationRoot { session_id: root.session.id.clone(),
        native_creation_id: root.federation_native_creation_id.clone(),
        binding: root.mailbox_binding().cloned(), name: federation_mailbox_display_name(&root.session.name),
        automatic: root.federation_mailbox_binding.is_none(), catalog_after: None }
}

fn federation_native_catalog_locked(
    inner: &StateInner, after: Option<&str>, host: FederationHostIdentity,
    central: FederationHostIdentity, incarnation: String,
) -> Result<FederationParticipationCatalog, ApiError> {
    let mut position: FederationCatalogPosition = after.map(serde_json::from_str).transpose()
        .map_err(|_| ApiError::bad_request("invalid native catalog cursor"))?.unwrap_or_default();
    if position.session > inner.sessions.len() || position.session > 0 && inner.sessions.get(position.session - 1)
        .is_none_or(|row| Some(&row.session.id) != position.previous.as_ref()) {
        // A deletion shifts vector positions. Restart the bounded iteration;
        // already-admitted roots are idempotent and routine activity is no error.
        position.session = 0;
        position.previous = None;
    }
    let mut index = inner.delegation_catalog_children.lock().expect("catalog child index poisoned");
    if let Some(progress) = index.prepare(&inner.delegations) {
        position.children_progress = progress;
        return Ok(FederationParticipationCatalog { host, central, link_incarnation: incarnation,
            roots: Vec::new(), next: Some(serde_json::to_string(&position)
                .map_err(|_| ApiError::internal("catalog cursor encoding failed"))?) });
    }
    position.children_progress = 0;
    let mut roots = Vec::with_capacity(FEDERATION_CATALOG_PAGE);
    for _ in 0..FEDERATION_CATALOG_SCAN {
        let Some(root) = inner.sessions.get(position.session) else { break; };
        position.previous = Some(root.session.id.clone());
        position.session += 1;
        if !root.hidden && root.is_local_session() && root.session.parent_delegation_id.is_none()
            && !index.children.contains_key(&root.session.id) {
            let mut candidate = federation_catalog_root(root);
            candidate.catalog_after = Some(serde_json::to_string(&position)
                .map_err(|_| ApiError::internal("catalog cursor encoding failed"))?);
            roots.push(candidate);
            if roots.len() == FEDERATION_CATALOG_PAGE { break; }
        }
    }
    let next = if position.session < inner.sessions.len() {
        Some(serde_json::to_string(&position).map_err(|_| ApiError::internal("catalog cursor encoding failed"))?)
    } else { None };
    Ok(FederationParticipationCatalog { host, central, link_incarnation: incarnation, roots, next })
}
