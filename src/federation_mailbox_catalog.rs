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
    by_delegation: BTreeMap<String, DelegationCatalogPosition>,
    children: BTreeMap<String, usize>,
    child_positions: BTreeMap<String, BTreeSet<usize>>,
    parent_positions: BTreeMap<String, BTreeSet<usize>>,
    scan: Option<usize>,
    progress: usize,
}

struct DelegationCatalogPosition {
    parent: String,
    child: String,
    index: usize,
}

impl FederationCatalogChildren {
    // Constructed from owned restored records before StateInner is published.
    fn from_records(rows: &[DelegationRecord]) -> Self {
        let mut index = Self::default();
        for (position, row) in rows.iter().enumerate() {
            #[cfg(test)]
            note_persist_delegation_visit();
            index.observe(row, position);
        }
        index
    }

    fn ready(&self, records: usize) -> bool {
        self.scan.is_none() && self.by_delegation.len() == records
    }

    fn removed_at(&mut self, position: usize) {
        if let Some(id) = self.by_delegation.iter()
            .find_map(|(id, at)| (at.index == position).then(|| id.clone())) {
            self.remove(&id);
        }
        self.child_positions.clear();
        self.parent_positions.clear();
        for at in self.by_delegation.values_mut() {
            if at.index > position { at.index -= 1; }
            self.child_positions.entry(at.child.clone()).or_default().insert(at.index);
            self.parent_positions.entry(at.parent.clone()).or_default().insert(at.index);
        }
        if let Some(scan) = self.scan.as_mut() {
            if position < *scan { *scan -= 1; }
        }
    }

    /// Returns whether this row changed the indexed relationship.
    fn observe(&mut self, row: &DelegationRecord, position: usize) -> bool {
        if self.by_delegation.get(&row.id).is_some_and(|at|
            at.child == row.child_session_id && at.parent == row.parent_session_id && at.index == position)
        {
            return false;
        }
        self.remove(&row.id);
        self.by_delegation.insert(row.id.clone(), DelegationCatalogPosition {
            parent: row.parent_session_id.clone(),
            child: row.child_session_id.clone(),
            index: position,
        });
        *self.children.entry(row.child_session_id.clone()).or_default() += 1;
        self.child_positions.entry(row.child_session_id.clone()).or_default().insert(position);
        self.parent_positions.entry(row.parent_session_id.clone()).or_default().insert(position);
        true
    }

    fn remove(&mut self, id: &str) {
        if let Some(at) = self.by_delegation.remove(id) {
            Self::remove_position(&mut self.child_positions, &at.child, at.index);
            Self::remove_position(&mut self.parent_positions, &at.parent, at.index);
            if let Some(count) = self.children.get_mut(&at.child) {
                *count -= 1;
                if *count == 0 { self.children.remove(&at.child); }
            }
        }
    }

    fn remove_position(
        by_session: &mut BTreeMap<String, BTreeSet<usize>>,
        session: &str,
        position: usize,
    ) {
        if let Some(positions) = by_session.get_mut(session) {
            positions.remove(&position);
            if positions.is_empty() {
                by_session.remove(session);
            }
        }
    }

    fn ensure_ready(&mut self, rows: &[DelegationRecord]) {
        if !self.ready(rows.len()) {
            *self = Self::from_records(rows);
        }
    }

    fn delegation_for_child(&mut self, rows: &[DelegationRecord], child: &str) -> Option<usize> {
        // Boot publishes a complete index; production insertion/removal keeps
        // it current. Direct bulk fixture construction may start cold.
        self.ensure_ready(rows);
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
        self.ensure_ready(rows);
        let position = self.by_delegation.get(id)?.index;
        if rows.get(position).is_some_and(|row| row.id == id) { return Some(position); }
        *self = Self::from_records(rows);
        self.by_delegation.get(id).map(|at| at.index)
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
            self.observe(row, start + offset);
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
