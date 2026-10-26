// Scheduler-only input. Capture each table once, copying identifiers and
// predicate facts rather than session messages, runtimes, event proofs or
// complete histories. Cross-table lookups and planning happen after unlock.
// This is a candidate plan, not authority: the worker still revalidates each
// exact root, journal and reader against current state before doing a read.
struct EngramRootReclamationSnapshot {
    roots: Vec<EngramWorkSourceRoot>,
    unpublished: Vec<EngramWorkSourceRoot>,
    journals: Vec<EngramReclamationJournal>,
    sessions: Vec<EngramReclamationSession>,
    projects: Vec<EngramReclamationProject>,
    parents: Vec<(String, String)>,
    evaluations: Vec<(EngramAuthorityStoreKey, String)>,
    pending_releases: Vec<EngramWorkSourceRoot>,
    quarantined: BTreeSet<String>,
    last_store: Option<EngramAuthorityStoreKey>,
    cooldowns: Vec<(EngramWorkSourceRoot, std::time::Instant)>,
    refused_claims: Vec<(EngramAuthorityStoreKey, String)>,
    disposition_cooldowns: Vec<(EngramWorkSourceRoot, std::time::Instant)>,
}

struct EngramReclamationJournal {
    store: EngramAuthorityStoreKey,
    claim_id: String,
    binding: Option<EngramControlWorkBinding>,
    pending: bool,
}

struct EngramReclamationSession {
    id: String,
    project_id: Option<String>,
    child_shape: bool,
    local: bool,
    enabled: bool,
    token: bool,
    idle: bool,
    binding: Option<EngramControlWorkBinding>,
}

struct EngramReclamationProject {
    id: String,
    eligible: bool,
    store: Option<EngramAuthorityStoreKey>,
}

impl EngramRootReclamationSnapshot {
    fn capture(inner: &StateInner) -> Self {
        let schedule = &inner.engram_root_reclamation;
        Self {
            roots: inner.engram_work_source_roots.clone(),
            unpublished: inner
                .engram_work_naming_history
                .iter()
                .flat_map(|history| &history.retirements)
                .filter(|notice| !notice.published)
                .map(|notice| notice.selection.clone())
                .collect(),
            journals: inner
                .engram_named_root_journal
                .iter()
                .map(|journal| EngramReclamationJournal {
                    store: journal.store.clone(),
                    claim_id: journal.claim_id.clone(),
                    binding: journal.read_binding.clone(),
                    pending: journal.pending.is_some(),
                })
                .collect(),
            sessions: inner
                .sessions
                .iter()
                .map(|record| EngramReclamationSession {
                    id: record.session.id.clone(),
                    project_id: record.session.project_id.clone(),
                    child_shape: record.session.parent_delegation_id.is_some(),
                    local: record.is_local_session(),
                    enabled: record.engram.disabled_reason.is_none(),
                    token: record.engram.routing_token.is_some(),
                    idle: record.engram.active_grant_id.is_none()
                        && record.engram.uncertain_grant_id.is_none()
                        && !record.engram.checkpoint_in_progress
                        && matches!(
                            record.session.status,
                            SessionStatus::Idle | SessionStatus::Error
                        )
                        && record.unmediated_claude_turn.is_none(),
                    binding: record.engram.work_binding.clone(),
                })
                .collect(),
            projects: inner
                .projects
                .iter()
                .map(|project| EngramReclamationProject {
                    id: project.id.clone(),
                    eligible: !inner.engram_project_resets.contains(&project.id)
                        && project.remote_id == LOCAL_REMOTE_ID
                        && project.engram.as_ref().is_some_and(|settings| {
                            settings.is_base_enabled()
                                && settings.binary_path.is_some()
                                && settings.home.is_some()
                        }),
                    store: project
                        .engram
                        .as_ref()
                        .and_then(|settings| settings.authority_store_key.clone()),
                })
                .collect(),
            parents: inner
                .delegations
                .iter()
                .map(|delegation| {
                    (
                        delegation.child_session_id.clone(),
                        delegation.parent_session_id.clone(),
                    )
                })
                .collect(),
            evaluations: inner
                .delegations
                .iter()
                .filter(|delegation| {
                    matches!(
                        delegation.status,
                        DelegationStatus::Queued
                            | DelegationStatus::Running
                            | DelegationStatus::Held
                    )
                })
                .filter_map(|delegation| delegation.acceptance_evaluation.as_ref())
                .filter(|target| matches!(target.submission, AcceptanceEvaluationSubmission::None))
                .flat_map(|target| {
                    target
                        .naming_history
                        .as_ref()
                        .map(|token| &token.work_id)
                        .into_iter()
                        .chain(target.source_root.as_ref().map(|root| &root.work_id))
                        .filter_map(move |work| target.store.clone().map(|store| (store, work.clone())))
                })
                .collect(),
            pending_releases: inner
                .engram_named_root_release_audit
                .iter()
                .filter(|record| {
                    !record.acknowledged
                        && record.route != EngramNamedRootReleaseRoute::Canonical.label()
                })
                .map(|record| record.selection.clone())
                .collect(),
            quarantined: inner.quarantined_persisted_session_ids.clone(),
            last_store: schedule.last_store.clone(),
            cooldowns: schedule.cooldowns.clone(),
            refused_claims: schedule.refused_claims.clone(),
            disposition_cooldowns: schedule.disposition_cooldowns.clone(),
        }
    }

    fn journal(&self, root: &EngramWorkSourceRoot) -> Option<&EngramReclamationJournal> {
        self.journals
            .iter()
            .find(|journal| journal.store == root.store && journal.claim_id == root.claim_id)
    }

    // Mirrors the reader-view predicates in engram_session_authority.rs.
    // Resolve ancestry on the owned metadata, never by re-entering StateInner.
    // First-match order, orphan child refusal and cycle refusal are preserved.
    fn reader_project<'a>(
        &'a self,
        session: &EngramReclamationSession,
        sessions: &HashMap<&str, &EngramReclamationSession>,
        parents: &HashMap<&str, &str>,
        projects: &HashMap<&str, &'a EngramReclamationProject>,
    ) -> Option<&'a EngramReclamationProject> {
        if !session.local || !session.enabled {
            return None;
        }
        let child = parents.contains_key(session.id.as_str());
        if session.child_shape && !child {
            return None;
        }
        let project_id = if child {
            let mut current = session;
            let mut visited = HashSet::new();
            loop {
                if !visited.insert(current.id.as_str()) {
                    return None;
                }
                if let Some(project) = current.project_id.as_deref() {
                    break project;
                }
                current = sessions.get(parents.get(current.id.as_str())?)?;
            }
        } else {
            session.project_id.as_deref()?
        };
        projects
            .get(project_id)
            .copied()
            .filter(|project| project.eligible)
    }

    fn readers(&self) -> Vec<(&EngramReclamationSession, &EngramAuthorityStoreKey)> {
        let mut sessions = HashMap::new();
        for session in &self.sessions {
            sessions.entry(session.id.as_str()).or_insert(session);
        }
        let mut parents = HashMap::new();
        for (child, parent) in &self.parents {
            parents.entry(child.as_str()).or_insert(parent.as_str());
        }
        let mut projects = HashMap::new();
        for project in &self.projects {
            projects.entry(project.id.as_str()).or_insert(project);
        }
        self.sessions
            .iter()
            .filter(|session| session.token)
            .filter_map(|session| {
                self.reader_project(session, &sessions, &parents, &projects)
                    .and_then(|project| project.store.as_ref())
                    .map(|store| (session, store))
            })
            .collect()
    }

    fn plan(&self, now: std::time::Instant) -> Vec<EngramRootReclamationTask> {
        #[cfg(test)]
        engram_root_reclamation_planning_boundary("canonical", &[]);
        let readers = self.readers();
        let mut roots = self.roots.clone();
        roots.extend(
            self.unpublished
                .iter()
                .filter(|root| !self.roots.contains(root))
                .cloned(),
        );
        roots.sort_by(|a, b| {
            a.named_at
                .cmp(&b.named_at)
                .then(a.generation.cmp(&b.generation))
        });
        let mut stores: BTreeMap<
            EngramAuthorityStoreKey,
            std::collections::VecDeque<EngramRootReclamationTask>,
        > = BTreeMap::new();
        for root in roots {
            if !readers.iter().any(|(_, store)| **store == root.store)
                || self
                    .cooldowns
                    .iter()
                    .any(|(selection, until)| selection == &root && now < *until)
            {
                continue;
            }
            let Some(journal) = self.journal(&root) else {
                continue;
            };
            let Some(binding) = journal.binding.as_ref().filter(|binding| {
                !journal.pending
                    && binding.work_id == root.work_id
                    && binding.claim_id == root.claim_id
            }) else {
                continue;
            };
            let owner = readers.iter().find(|(session, store)| {
                **store == root.store
                    && session
                        .binding
                        .as_ref()
                        .is_some_and(|current| engram_same_recovery_run(current, binding))
            });
            let route = match owner {
                Some(_)
                    if self
                        .evaluations
                        .iter()
                        .any(|(store, work)| *store == root.store && *work == root.work_id) =>
                {
                    continue;
                }
                Some((session, _)) if session.idle => {
                    EngramRootReclamationRoute::Current(session.id.clone())
                }
                Some(_) => continue,
                None => EngramRootReclamationRoute::Unfocused,
            };
            stores
                .entry(root.store.clone())
                .or_default()
                .push_back(EngramRootReclamationTask { root, route });
        }
        let mut order = stores.keys().cloned().collect::<Vec<_>>();
        if let Some(last) = &self.last_store
            && !order.is_empty()
        {
            let start = order.partition_point(|store| store <= last) % order.len();
            order.rotate_left(start);
        }
        let mut plan = Vec::new();
        while plan.len() < ENGRAM_ROOT_RECLAMATION_READ_LIMIT {
            let mut advanced = false;
            for store in &order {
                if let Some(task) = stores.get_mut(store).and_then(|roots| roots.pop_front()) {
                    plan.push(task);
                    advanced = true;
                }
                if plan.len() == ENGRAM_ROOT_RECLAMATION_READ_LIMIT {
                    break;
                }
            }
            if !advanced {
                break;
            }
        }
        plan
    }

    fn disposition_due(&self, now: std::time::Instant) -> bool {
        #[cfg(test)]
        engram_root_reclamation_planning_boundary("disposition", &[]);
        self.pending_releases
            .iter()
            .any(|selection| !self.roots.contains(selection))
            || self.roots.iter().any(|root| {
                let journal = self.journal(root);
                let association = journal
                    .and_then(|journal| journal.binding.as_ref())
                    .is_some_and(|binding| {
                        binding.work_id == root.work_id && binding.claim_id == root.claim_id
                    });
                let refused = self
                    .refused_claims
                    .iter()
                    .any(|(store, claim)| *store == root.store && *claim == root.claim_id);
                let held = self.sessions.iter().any(|session| {
                    session.binding.as_ref().is_some_and(|binding| {
                        binding.work_id == root.work_id && binding.claim_id == root.claim_id
                    })
                });
                (!association || refused)
                    && !held
                    && !self.quarantined.contains(&root.named_by_session)
                    && !journal.is_some_and(|journal| journal.pending)
                    && !self
                        .disposition_cooldowns
                        .iter()
                        .any(|(cooling, until)| cooling == root && *until > now)
            })
    }
}
