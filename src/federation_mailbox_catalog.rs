// Bounded native catalog pages with an incrementally maintained child index.
// Cursors describe iteration, never principal or execution authority.
// Lock order is StateInner, then delegation_catalog_children; never reverse.
// The bounded in-memory index/page work holds no lock across I/O or await.

const FEDERATION_CATALOG_SCAN: usize = 64;
const FEDERATION_CATALOG_PAGE: usize = 32;

#[derive(Default)]
// Production boot publishes a complete index and every relationship mutation
// calls mark_delegation_mutated. Fixtures that edit a row or replace the vector
// must use that same contract, including replacements at unchanged length.
// Cold bulk fixture lookup and test-only vector removal may rebuild O(n) here;
// production lookup is warm and production does not remove delegation rows.
struct FederationCatalogChildren {
    by_delegation: BTreeMap<String, (String, usize)>,
    children: BTreeMap<String, usize>,
    child_positions: BTreeMap<String, BTreeSet<usize>>,
    scan: Option<usize>,
    progress: usize,
}

impl FederationCatalogChildren {
    // Constructed from owned restored records before StateInner is published.
    fn from_records(rows: &[DelegationRecord]) -> Self {
        let mut index = Self::default();
        for (position, row) in rows.iter().enumerate() {
            index.observe(&row.id, &row.child_session_id, position);
        }
        index
    }

    fn ready(&self, records: usize) -> bool {
        self.scan.is_none() && self.by_delegation.len() == records
    }

    fn removed_at(&mut self, position: usize) {
        if let Some(id) = self.by_delegation.iter()
            .find_map(|(id, (_, at))| (*at == position).then(|| id.clone())) {
            self.remove(&id);
        }
        self.child_positions.clear();
        for (child, at) in self.by_delegation.values_mut() {
            if *at > position { *at -= 1; }
            self.child_positions.entry(child.clone()).or_default().insert(*at);
        }
        if let Some(scan) = self.scan.as_mut() {
            if position < *scan { *scan -= 1; }
        }
    }

    fn observe(&mut self, id: &str, child: &str, position: usize) {
        if self.by_delegation.get(id).is_some_and(|(old, at)| old == child && *at == position) { return; }
        self.remove(id);
        self.by_delegation.insert(id.to_owned(), (child.to_owned(), position));
        *self.children.entry(child.to_owned()).or_default() += 1;
        self.child_positions.entry(child.to_owned()).or_default().insert(position);
    }

    fn remove(&mut self, id: &str) {
        if let Some((child, position)) = self.by_delegation.remove(id) {
            if let Some(positions) = self.child_positions.get_mut(&child) {
                positions.remove(&position);
                if positions.is_empty() { self.child_positions.remove(&child); }
            }
            if let Some(count) = self.children.get_mut(&child) {
                *count -= 1;
                if *count == 0 { self.children.remove(&child); }
            }
        }
    }

    fn delegation_for_child(&mut self, rows: &[DelegationRecord], child: &str) -> Option<usize> {
        // Boot publishes a complete index; production insertion/removal keeps
        // it current. Direct bulk fixture construction may start cold.
        if !self.ready(rows.len()) { *self = Self::from_records(rows); }
        let position = *self.child_positions.get(child)?.first()?;
        if rows.get(position).is_some_and(|row| row.child_session_id == child) {
            return Some(position);
        }
        // A direct vector replacement/removal bypassed the mutation helpers.
        // Repair in this existing index owner rather than returning a stale slot.
        *self = Self::from_records(rows);
        self.child_positions.get(child)?.first().copied()
    }

    fn delegation_by_id(&mut self, rows: &[DelegationRecord], id: &str) -> Option<usize> {
        if !self.ready(rows.len()) { *self = Self::from_records(rows); }
        let position = self.by_delegation.get(id)?.1;
        if rows.get(position).is_some_and(|row| row.id == id) { return Some(position); }
        *self = Self::from_records(rows);
        self.by_delegation.get(id).map(|(_, position)| *position)
    }

    // Direct bulk construction can start cold. Production insertions update
    // the index immediately; removals also shift this bounded scan cursor.
    // This bounded catalog scan never clears accumulated maps or restarts on
    // length/status churn; a cold point lookup above can complete it eagerly.
    fn prepare(&mut self, rows: &[DelegationRecord]) -> Option<usize> {
        if self.ready(rows.len()) { return None; }
        let start = self.scan.unwrap_or(0).min(rows.len());
        let end = start.saturating_add(FEDERATION_CATALOG_SCAN).min(rows.len());
        for (offset, row) in rows[start..end].iter().enumerate() {
            self.observe(&row.id, &row.child_session_id, start + offset);
        }
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
    inner.require_delegation_inventory()?;
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
