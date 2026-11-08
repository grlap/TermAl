// Test-run inputs of the existing state projection cache. The cut, not a
// revision shortcut or a second run index, owns each derived value. Consumers
// materialize it after dropping StateInner; serializing an action response
// uses the same cut as snapshots and the ordered broadcaster.

struct TestRunHistoryProjection {
    version: u64,
    cut: TestRunProjectionCut,
    value: std::sync::OnceLock<TestRunHistoryValue>,
    // Derived metadata DTOs only, never SessionRecord/runtime ownership.
    // Retaining the raw Arc prevents pointer reuse from masquerading as a hit.
    activity_rows: Mutex<HashMap<String, (Arc<StateSessionSummary>, Arc<StateSessionSummary>)>>,
}

struct TestRunHistoryValue {
    runs: Vec<TestRunSummary>,
    overview: TestRunOverview,
    activity: BTreeMap<String, SessionTestRunActivity>,
}

impl TestRunHistoryProjection {
    fn value(&self) -> &TestRunHistoryValue {
        self.value.get_or_init(|| {
            let mut runs = test_run_project_history(&self.cut, true);
            let mut seen = HashSet::new();
            runs.retain(|run| seen.insert(run.run_id.clone()));
            let (overview, activity) = test_run_overview(
                &runs,
                self.cut.projection_index().scanned_once,
                self.version,
                &self.cut.attention_ids,
            );
            TestRunHistoryValue {
                runs,
                overview,
                activity,
            }
        })
    }

    fn session_with_activity(
        &self,
        raw: &Arc<StateSessionSummary>,
        activity: &SessionTestRunActivity,
    ) -> Arc<StateSessionSummary> {
        let mut rows = self
            .activity_rows
            .lock()
            .expect("test-run activity projection mutex poisoned");
        if let Some((saved_raw, enriched)) = rows.get(&raw.id) {
            if Arc::ptr_eq(saved_raw, raw) {
                return enriched.clone();
            }
        }
        let mut summary = (**raw).clone();
        summary.test_run_activity = SessionTestRunActivitySnapshot {
            source: None,
            wire: activity.clone(),
        };
        let enriched = Arc::new(summary);
        rows.insert(raw.id.clone(), (raw.clone(), enriched.clone()));
        enriched
    }
}

impl AppState {
    fn test_run_history_projection_locked(
        &self,
        inner: &StateInner,
    ) -> Arc<TestRunHistoryProjection> {
        let mut cache = inner
            .state_projection_cache
            .lock()
            .expect("projection cache mutex poisoned");
        if let Some(saved) = &cache.test_runs {
            if saved.cut.matches(inner) {
                return saved.clone();
            }
        }
        // Exact row and association validation includes unstamped mutations.
        // Initial capture is O(history) handle/value copying; no summary
        // construction, canonical merge, or sorting runs under this guard.
        cache.test_run_version = cache
            .test_run_version
            .checked_add(1)
            .expect("test-run projection version exhausted");
        let projection = Arc::new(TestRunHistoryProjection {
            version: cache.test_run_version,
            cut: TestRunProjectionCut::capture_history(inner),
            value: std::sync::OnceLock::new(),
            activity_rows: Mutex::new(HashMap::new()),
        });
        cache.test_runs = Some(projection.clone());
        projection
    }
}

// A lazy wire value lets existing action-response builders capture while
// locked without making them project history there. Deserialized remote
// values never acquire local authority or join this cache.
#[derive(Clone, Default)]
struct TestRunOverviewSnapshot {
    source: Option<Arc<TestRunHistoryProjection>>,
    wire: TestRunOverview,
}

impl TestRunOverviewSnapshot {
    fn captured(source: Arc<TestRunHistoryProjection>) -> Self {
        Self {
            source: Some(source),
            wire: TestRunOverview::default(),
        }
    }

    fn get(&self) -> &TestRunOverview {
        self.source
            .as_ref()
            .map_or(&self.wire, |source| &source.value().overview)
    }
}

impl Serialize for TestRunOverviewSnapshot {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.get().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TestRunOverviewSnapshot {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self {
            source: None,
            wire: TestRunOverview::deserialize(deserializer)?,
        })
    }
}

#[derive(Clone, Default)]
struct SessionTestRunActivitySnapshot {
    source: Option<(Arc<TestRunHistoryProjection>, String)>,
    wire: SessionTestRunActivity,
}

impl SessionTestRunActivitySnapshot {
    fn captured(source: Arc<TestRunHistoryProjection>, session: &str) -> Self {
        Self {
            source: Some((source, session.to_owned())),
            wire: SessionTestRunActivity::default(),
        }
    }

    fn get(&self) -> &SessionTestRunActivity {
        self.source
            .as_ref()
            .and_then(|(source, id)| source.value().activity.get(id))
            .unwrap_or(&self.wire)
    }

    fn is_empty(&self) -> bool {
        self.get() == &SessionTestRunActivity::default()
    }
}

impl std::fmt::Debug for SessionTestRunActivitySnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionTestRunActivitySnapshot")
            .field("captured", &self.source.is_some())
            .finish_non_exhaustive()
    }
}

impl Serialize for SessionTestRunActivitySnapshot {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.get().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SessionTestRunActivitySnapshot {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self {
            source: None,
            wire: SessionTestRunActivity::deserialize(deserializer)?,
        })
    }
}

#[derive(Clone, Default)]
struct TestRunActivitiesSnapshot {
    source: Option<Arc<TestRunHistoryProjection>>,
    wire: BTreeMap<String, SessionTestRunActivity>,
}

impl Serialize for TestRunActivitiesSnapshot {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.source
            .as_ref()
            .map_or(&self.wire, |source| &source.value().activity)
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TestRunActivitiesSnapshot {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self {
            source: None,
            wire: BTreeMap::deserialize(deserializer)?,
        })
    }
}

impl ImmutableStateSnapshot {
    // Diff rows use the ordinary session cache Arcs when activity is absent.
    // Only sessions with running owned/notify work need an enriched row.
    // Last activity disappearing compares an old enriched Arc with the plain
    // successor, so the diff clears it rather than leaving a stale badge.
    fn prepare_test_run_activity(&mut self) {
        let Some(source) = self.header.test_run_overview.source.as_ref() else {
            return;
        };
        let activity = &source.value().activity;
        for row in &mut self.sessions {
            if let Some(value) = activity.get(&row.id) {
                if row.test_run_activity.source.is_none() && row.test_run_activity.wire == *value {
                    continue;
                }
                *row = source.session_with_activity(row, value);
            }
        }
    }
}
