use std::collections::{HashMap, HashSet};

use n00n_storage::id::n00nId;
use thiserror::Error;
use tracing::warn;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_field_names)]
pub(crate) struct LineageLimits {
    pub(crate) max_depth: usize,
    pub(crate) max_total_descendants: usize,
    pub(crate) max_active_descendants: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LiveSession {
    pub(crate) id: n00nId,
    pub(crate) root_session_id: n00nId,
    pub(crate) parent_id: Option<n00nId>,
    pub(crate) runtime_present: bool,
    pub(crate) execution_active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionLineage {
    pub(crate) caller: n00nId,
    pub(crate) root: n00nId,
    pub(crate) parent: Option<n00nId>,
    pub(crate) depth: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DescendantCounts {
    pub(crate) total: usize,
    pub(crate) active: usize,
    pub(crate) reserved: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NewReservation {
    id: u64,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum LineageError {
    #[error("caller session is not live: {0}")]
    CallerNotLive(n00nId),
    #[error("session is not live: {0}")]
    TargetNotLive(n00nId),
    #[error("session is not known: {0}")]
    UnknownSession(n00nId),
    #[error("session lineage parent must match caller")]
    ParentMismatch,
    #[error("session lineage root is missing for descendant {0}")]
    MissingRoot(n00nId),
    #[error("session lineage parent {parent} is missing for {id}")]
    MissingParent { id: n00nId, parent: n00nId },
    #[error("session lineage root mismatch for {id}: expected {expected}, found {found}")]
    RootMismatch {
        id: n00nId,
        expected: n00nId,
        found: n00nId,
    },
    #[error("session lineage contains a cycle at {0}")]
    Cycle(n00nId),
    #[error("session lineage depth limit exceeded: {limit}")]
    DepthExceeded { limit: usize },
    #[error("session lineage total descendant limit exceeded: {limit}")]
    TotalDescendantsExceeded { limit: usize },
    #[error("session lineage active descendant limit exceeded: {limit}")]
    ActiveDescendantsExceeded { limit: usize },
    #[error("prompt target is outside the caller lineage")]
    UnauthorizedTarget,
    #[error("session already exists: {0}")]
    DuplicateSession(n00nId),
    #[error("session lineage parent changed for {id}")]
    ParentChanged { id: n00nId },
    #[error("session lineage reservation is unknown")]
    UnknownReservation,
    #[error("session lineage reservation id space exhausted")]
    ReservationIdExhausted,
}

#[derive(Debug, Clone, Copy)]
struct SessionNode {
    root_session_id: n00nId,
    parent_id: Option<n00nId>,
    runtime_present: bool,
    execution_active: bool,
    deleted: bool,
}

#[derive(Debug, Clone, Copy)]
struct PendingReservation {
    caller: n00nId,
    parent: n00nId,
    root: n00nId,
    depth: usize,
    execution_active: bool,
}

#[derive(Debug, Clone, Copy)]
struct CachedLineage {
    root: n00nId,
    depth: usize,
}

pub(crate) struct SessionLineageGuard {
    limits: LineageLimits,
    sessions: HashMap<n00nId, SessionNode>,
    children: HashMap<n00nId, HashSet<n00nId>>,
    lineage_cache: HashMap<n00nId, CachedLineage>,
    reservations: HashMap<u64, PendingReservation>,
    next_reservation_id: u64,
}

impl SessionLineageGuard {
    pub(crate) fn from_live(
        sessions: impl IntoIterator<Item = LiveSession>,
        limits: LineageLimits,
    ) -> Result<Self, LineageError> {
        let mut guard = Self {
            limits,
            sessions: HashMap::new(),
            children: HashMap::new(),
            lineage_cache: HashMap::new(),
            reservations: HashMap::new(),
            next_reservation_id: 1,
        };
        for session in sessions {
            if guard
                .sessions
                .insert(
                    session.id,
                    SessionNode {
                        root_session_id: session.root_session_id,
                        parent_id: session.parent_id,
                        runtime_present: session.runtime_present,
                        execution_active: session.execution_active,
                        deleted: false,
                    },
                )
                .is_some()
            {
                return Err(LineageError::DuplicateSession(session.id));
            }
        }
        guard.rebuild_topology()?;
        let roots = guard
            .lineage_cache
            .values()
            .map(|lineage| lineage.root)
            .collect::<HashSet<_>>();
        for root in roots {
            let counts = guard.descendant_counts(root)?;
            if counts.active > guard.limits.max_active_descendants {
                return Err(LineageError::ActiveDescendantsExceeded {
                    limit: guard.limits.max_active_descendants,
                });
            }
        }
        Ok(guard)
    }

    pub(crate) fn activate_runtime(&mut self, session: LiveSession) -> Result<(), LineageError> {
        if let Some(existing) = self.sessions.get(&session.id) {
            if existing.deleted {
                return Err(LineageError::UnknownSession(session.id));
            }
            if existing.runtime_present {
                return Err(LineageError::DuplicateSession(session.id));
            }
            if existing.parent_id != session.parent_id
                || existing.root_session_id != session.root_session_id
            {
                return Err(LineageError::ParentChanged { id: session.id });
            }
            self.sessions
                .get_mut(&session.id)
                .ok_or(LineageError::UnknownSession(session.id))?
                .runtime_present = true;
            self.sessions
                .get_mut(&session.id)
                .ok_or(LineageError::UnknownSession(session.id))?
                .execution_active = session.execution_active;
            return Ok(());
        }

        self.sessions.insert(
            session.id,
            SessionNode {
                root_session_id: session.root_session_id,
                parent_id: session.parent_id,
                runtime_present: true,
                execution_active: session.execution_active,
                deleted: false,
            },
        );
        if let Err(error) = self.rebuild_topology() {
            self.sessions.remove(&session.id);
            if let Err(rollback_error) = self.rebuild_topology() {
                warn!(
                    session_id = %session.id,
                    error = %rollback_error,
                    "failed to rebuild session lineage topology after activation rollback"
                );
            }
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn remove_runtime(&mut self, id: n00nId) -> Result<(), LineageError> {
        let node = self
            .sessions
            .get_mut(&id)
            .ok_or(LineageError::UnknownSession(id))?;
        node.runtime_present = false;
        node.execution_active = false;
        Ok(())
    }

    pub(crate) fn set_execution_active(
        &mut self,
        id: n00nId,
        active: bool,
    ) -> Result<bool, LineageError> {
        let node = self
            .sessions
            .get_mut(&id)
            .ok_or(LineageError::UnknownSession(id))?;
        if active && (!node.runtime_present || node.deleted) {
            return Err(LineageError::TargetNotLive(id));
        }
        let changed = node.execution_active != active;
        node.execution_active = active;
        Ok(changed)
    }

    pub(crate) fn begin_execution(&mut self, id: n00nId) -> Result<bool, LineageError> {
        let lineage = self.lineage_for(id)?;
        let node = self
            .sessions
            .get(&id)
            .ok_or(LineageError::UnknownSession(id))?;
        if !node.runtime_present {
            return Err(LineageError::TargetNotLive(id));
        }
        if node.execution_active {
            return Ok(false);
        }
        if id != lineage.root {
            let counts = self.descendant_counts(lineage.root)?;
            if counts.active >= self.limits.max_active_descendants {
                return Err(LineageError::ActiveDescendantsExceeded {
                    limit: self.limits.max_active_descendants,
                });
            }
        }
        self.sessions
            .get_mut(&id)
            .ok_or(LineageError::UnknownSession(id))?
            .execution_active = true;
        Ok(true)
    }

    pub(crate) fn lineage(&self, caller: n00nId) -> Result<SessionLineage, LineageError> {
        let node = self
            .sessions
            .get(&caller)
            .ok_or(LineageError::CallerNotLive(caller))?;
        if !node.runtime_present || node.deleted {
            return Err(LineageError::CallerNotLive(caller));
        }
        self.lineage_for(caller).map_err(|error| match error {
            LineageError::UnknownSession(_) => LineageError::CallerNotLive(caller),
            error => error,
        })
    }

    pub(crate) fn reserve_new(
        &mut self,
        caller: n00nId,
        explicit_parent: Option<n00nId>,
        execution_active: bool,
    ) -> Result<NewReservation, LineageError> {
        let caller_lineage = self.lineage(caller)?;
        let parent = match explicit_parent {
            Some(p) => p,
            None => caller,
        };
        if parent != caller {
            return Err(LineageError::ParentMismatch);
        }
        let depth = caller_lineage
            .depth
            .checked_add(1)
            .ok_or(LineageError::DepthExceeded {
                limit: self.limits.max_depth,
            })?;
        if depth > self.limits.max_depth {
            return Err(LineageError::DepthExceeded {
                limit: self.limits.max_depth,
            });
        }

        let counts = self.descendant_counts(caller_lineage.root)?;
        if limit_reached(
            counts.total,
            counts.reserved,
            self.limits.max_total_descendants,
        ) {
            return Err(LineageError::TotalDescendantsExceeded {
                limit: self.limits.max_total_descendants,
            });
        }
        let active_reservations = self
            .reservations
            .values()
            .filter(|reservation| {
                reservation.root == caller_lineage.root && reservation.execution_active
            })
            .count();
        if execution_active
            && limit_reached(
                counts.active,
                active_reservations,
                self.limits.max_active_descendants,
            )
        {
            return Err(LineageError::ActiveDescendantsExceeded {
                limit: self.limits.max_active_descendants,
            });
        }

        let id = self.next_reservation_id;
        self.next_reservation_id = self
            .next_reservation_id
            .checked_add(1)
            .ok_or(LineageError::ReservationIdExhausted)?;
        self.reservations.insert(
            id,
            PendingReservation {
                caller,
                parent,
                root: caller_lineage.root,
                depth,
                execution_active,
            },
        );
        Ok(NewReservation { id })
    }

    pub(crate) fn commit_new(
        &mut self,
        reservation: NewReservation,
        child_id: n00nId,
    ) -> Result<(), LineageError> {
        let pending = self
            .reservations
            .remove(&reservation.id)
            .ok_or(LineageError::UnknownReservation)?;
        if self.sessions.contains_key(&child_id) {
            return Err(LineageError::DuplicateSession(child_id));
        }
        let caller_lineage = self.lineage(pending.caller)?;
        if caller_lineage.root != pending.root || caller_lineage.depth + 1 != pending.depth {
            return Err(LineageError::UnknownReservation);
        }
        self.sessions.insert(
            child_id,
            SessionNode {
                root_session_id: pending.root,
                parent_id: Some(pending.parent),
                runtime_present: true,
                execution_active: pending.execution_active,
                deleted: false,
            },
        );
        if let Err(error) = self.rebuild_topology() {
            self.sessions.remove(&child_id);
            if let Err(rollback_error) = self.rebuild_topology() {
                warn!(
                    session_id = %child_id,
                    error = %rollback_error,
                    "failed to rebuild session lineage topology after reservation rollback"
                );
            }
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn release(&mut self, reservation: NewReservation) -> Result<(), LineageError> {
        self.reservations
            .remove(&reservation.id)
            .map(|_| ())
            .ok_or(LineageError::UnknownReservation)
    }
    pub(crate) fn rollback_new(&mut self, id: n00nId) -> Result<(), LineageError> {
        if !self.sessions.contains_key(&id) {
            return Err(LineageError::UnknownSession(id));
        }
        if self
            .sessions
            .values()
            .any(|node| node.parent_id == Some(id))
        {
            return Err(LineageError::ParentChanged { id });
        }
        self.sessions.remove(&id);
        self.rebuild_topology()?;
        Ok(())
    }

    pub(crate) fn descendants_of(&self, parent: n00nId) -> Result<Vec<n00nId>, LineageError> {
        self.descendants(parent)
    }

    pub(crate) fn descendants_for_delete(
        &self,
        parent: n00nId,
    ) -> Result<Vec<n00nId>, LineageError> {
        if !self.sessions.contains_key(&parent) {
            return Err(LineageError::UnknownSession(parent));
        }
        let mut pending = self
            .children
            .get(&parent)
            .into_iter()
            .flat_map(|children| children.iter().copied())
            .map(|id| (id, false))
            .collect::<Vec<_>>();
        let mut descendants = Vec::new();
        while let Some((id, visited)) = pending.pop() {
            if visited {
                descendants.push(id);
                continue;
            }
            pending.push((id, true));
            if let Some(children) = self.children.get(&id) {
                pending.extend(children.iter().copied().map(|child| (child, false)));
            }
        }
        Ok(descendants)
    }

    fn descendants(&self, parent: n00nId) -> Result<Vec<n00nId>, LineageError> {
        if !self.sessions.contains_key(&parent) {
            return Err(LineageError::UnknownSession(parent));
        }
        let mut pending = self
            .children
            .get(&parent)
            .into_iter()
            .flat_map(|children| children.iter().copied())
            .collect::<Vec<_>>();
        let mut descendants = Vec::new();
        while let Some(id) = pending.pop() {
            if self.sessions.get(&id).is_some_and(|node| !node.deleted) {
                descendants.push(id);
            }
            if let Some(children) = self.children.get(&id) {
                pending.extend(children.iter().copied());
            }
        }
        Ok(descendants)
    }

    pub(crate) fn remove_sessions(&mut self, ids: &[n00nId]) {
        let removed: HashSet<_> = ids.iter().copied().collect();
        for id in &removed {
            if let Some(node) = self.sessions.get_mut(id) {
                node.runtime_present = false;
                node.execution_active = false;
                node.deleted = true;
            }
        }
        self.reservations.retain(|_, reservation| {
            !removed.contains(&reservation.caller) && !removed.contains(&reservation.parent)
        });
    }

    pub(crate) fn authorize_prompt(
        &self,
        caller: n00nId,
        explicit_target: Option<n00nId>,
    ) -> Result<n00nId, LineageError> {
        let caller_lineage = self.lineage(caller)?;
        let target = match explicit_target {
            Some(t) => t,
            None => caller,
        };
        let target_node = self
            .sessions
            .get(&target)
            .ok_or(LineageError::UnknownSession(target))?;
        if !target_node.runtime_present || target_node.deleted {
            return Err(LineageError::TargetNotLive(target));
        }
        let target_lineage = self.lineage_for(target)?;
        if caller_lineage.caller == target
            || (caller_lineage.root == target_lineage.root
                && self.path_from(target)?.contains(&caller))
        {
            return Ok(target);
        }
        Err(LineageError::UnauthorizedTarget)
    }

    pub(crate) fn descendant_counts(&self, root: n00nId) -> Result<DescendantCounts, LineageError> {
        let mut total = 0;
        let mut active = 0;
        for id in self.descendants_of(root)? {
            let node = self
                .sessions
                .get(&id)
                .ok_or(LineageError::UnknownSession(id))?;
            if node.deleted {
                continue;
            }
            total += 1;
            if node.execution_active {
                active += 1;
            }
        }
        let reserved = self
            .reservations
            .values()
            .filter(|reservation| reservation.root == root)
            .count();
        Ok(DescendantCounts {
            total,
            active,
            reserved,
        })
    }

    fn rebuild_topology(&mut self) -> Result<(), LineageError> {
        let mut children = HashMap::<n00nId, HashSet<n00nId>>::new();
        for (&id, node) in &self.sessions {
            if let Some(parent) = node.parent_id {
                if !self.sessions.contains_key(&parent) {
                    return Err(LineageError::MissingParent { id, parent });
                }
                children.entry(parent).or_default().insert(id);
            }
        }

        let mut lineage_cache = HashMap::new();
        for &id in self.sessions.keys() {
            resolve_cached_lineage(&self.sessions, &mut lineage_cache, id)?;
        }
        for (&id, node) in &self.sessions {
            let lineage = lineage_cache
                .get(&id)
                .ok_or(LineageError::UnknownSession(id))?;
            if node.root_session_id != lineage.root {
                return Err(LineageError::RootMismatch {
                    id,
                    expected: lineage.root,
                    found: node.root_session_id,
                });
            }
        }
        self.children = children;
        self.lineage_cache = lineage_cache;
        Ok(())
    }

    fn lineage_for(&self, id: n00nId) -> Result<SessionLineage, LineageError> {
        let node = self
            .sessions
            .get(&id)
            .ok_or(LineageError::UnknownSession(id))?;
        let cached = self
            .lineage_cache
            .get(&id)
            .ok_or(LineageError::UnknownSession(id))?;
        Ok(SessionLineage {
            caller: id,
            root: cached.root,
            parent: node.parent_id,
            depth: cached.depth,
        })
    }

    fn path_from(&self, start: n00nId) -> Result<Vec<n00nId>, LineageError> {
        if !self.sessions.contains_key(&start) {
            return Err(LineageError::UnknownSession(start));
        }
        let mut path = Vec::new();
        let mut current = start;
        loop {
            path.push(current);
            let parent = self
                .sessions
                .get(&current)
                .ok_or(LineageError::UnknownSession(current))?
                .parent_id;
            let Some(parent) = parent else {
                return Ok(path);
            };
            current = parent;
        }
    }
}

fn resolve_cached_lineage(
    sessions: &HashMap<n00nId, SessionNode>,
    cache: &mut HashMap<n00nId, CachedLineage>,
    start: n00nId,
) -> Result<(), LineageError> {
    if cache.contains_key(&start) {
        return Ok(());
    }
    let mut trail = Vec::new();
    let mut seen = HashSet::new();
    let mut current = start;
    loop {
        if let Some(cached) = cache.get(&current).copied() {
            let mut depth = cached.depth;
            for id in trail.into_iter().rev() {
                depth = depth
                    .checked_add(1)
                    .ok_or(LineageError::DepthExceeded { limit: usize::MAX })?;
                cache.insert(
                    id,
                    CachedLineage {
                        root: cached.root,
                        depth,
                    },
                );
            }
            return Ok(());
        }
        if !seen.insert(current) {
            return Err(LineageError::Cycle(current));
        }
        let node = sessions
            .get(&current)
            .ok_or(LineageError::UnknownSession(current))?;
        let Some(parent) = node.parent_id else {
            cache.insert(
                current,
                CachedLineage {
                    root: current,
                    depth: 0,
                },
            );
            let mut depth = 0usize;
            for id in trail.into_iter().rev() {
                depth = depth
                    .checked_add(1)
                    .ok_or(LineageError::DepthExceeded { limit: usize::MAX })?;
                cache.insert(
                    id,
                    CachedLineage {
                        root: current,
                        depth,
                    },
                );
            }
            return Ok(());
        };
        if !sessions.contains_key(&parent) {
            return Err(LineageError::MissingParent {
                id: current,
                parent,
            });
        }
        trail.push(current);
        current = parent;
    }
}

fn limit_reached(committed: usize, reserved: usize, limit: usize) -> bool {
    committed.saturating_add(reserved) >= limit
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: u16) -> n00nId {
        format!("00000000-0000-7000-8000-{value:012x}")
            .parse()
            .expect("valid test id")
    }

    fn session(id: n00nId, parent_id: Option<n00nId>) -> LiveSession {
        LiveSession {
            id,
            root_session_id: match parent_id {
                Some(p) => p,
                None => id,
            },
            parent_id,
            runtime_present: true,
            execution_active: parent_id.is_some(),
        }
    }

    fn limits(
        max_depth: usize,
        max_total_descendants: usize,
        max_active_descendants: usize,
    ) -> LineageLimits {
        LineageLimits {
            max_depth,
            max_total_descendants,
            max_active_descendants,
        }
    }

    #[test]
    fn independent_roots_have_independent_limits() {
        let root_a = id(1);
        let root_b = id(2);
        let child_a = id(3);
        let child_b = id(4);
        let mut guard = SessionLineageGuard::from_live(
            [session(root_a, None), session(root_b, None)],
            limits(4, 1, 1),
        )
        .expect("valid roots");

        let reservation = guard
            .reserve_new(root_a, None, true)
            .expect("root A capacity");
        guard.commit_new(reservation, child_a).expect("child A");
        assert!(matches!(
            guard.reserve_new(root_a, None, true),
            Err(LineageError::TotalDescendantsExceeded { .. })
        ));

        let reservation = guard
            .reserve_new(root_b, None, true)
            .expect("root B capacity");
        guard.commit_new(reservation, child_b).expect("child B");
        assert_eq!(guard.descendant_counts(root_a).expect("counts").total, 1);
        assert_eq!(guard.descendant_counts(root_b).expect("counts").total, 1);
    }

    #[test]
    fn new_and_prompt_reject_spoofed_relationships() {
        let root = id(1);
        let sibling = id(2);
        let foreign = id(3);
        let mut guard = SessionLineageGuard::from_live(
            [
                session(root, None),
                session(sibling, Some(root)),
                session(foreign, None),
            ],
            limits(4, 4, 4),
        )
        .expect("valid graph");

        assert!(matches!(
            guard.reserve_new(root, Some(foreign), true),
            Err(LineageError::ParentMismatch)
        ));
        assert!(matches!(
            guard.reserve_new(id(99), None, true),
            Err(LineageError::CallerNotLive(_))
        ));
        assert_eq!(
            guard
                .authorize_prompt(root, Some(sibling))
                .expect("descendant"),
            sibling
        );
        assert!(matches!(
            guard.authorize_prompt(sibling, Some(foreign)),
            Err(LineageError::UnauthorizedTarget)
        ));
        assert!(matches!(
            guard.authorize_prompt(sibling, Some(root)),
            Err(LineageError::UnauthorizedTarget)
        ));
    }

    #[test]
    fn cycles_are_rejected() {
        let first = id(1);
        let second = id(2);
        assert!(matches!(
            SessionLineageGuard::from_live(
                [session(first, Some(second)), session(second, Some(first))],
                limits(4, 4, 4),
            ),
            Err(LineageError::Cycle(_))
        ));
    }

    #[test]
    fn depth_total_and_active_limits_are_distinct() {
        let root = id(1);
        let child = id(2);
        let grandchild = id(3);
        let mut guard = SessionLineageGuard::from_live(
            [session(root, None), session(child, Some(root))],
            limits(2, 2, 2),
        )
        .expect("valid graph");
        let reservation = guard.reserve_new(child, None, true).expect("depth one");
        guard
            .commit_new(reservation, grandchild)
            .expect("grandchild");
        assert!(matches!(
            guard.reserve_new(grandchild, None, true),
            Err(LineageError::DepthExceeded { limit: 2 })
        ));

        let mut active_limited = SessionLineageGuard::from_live(
            [session(root, None), session(child, Some(root))],
            limits(4, 3, 1),
        )
        .expect("valid graph");
        assert!(matches!(
            active_limited.reserve_new(root, None, true),
            Err(LineageError::ActiveDescendantsExceeded { limit: 1 })
        ));
    }

    #[test]
    fn idle_reservation_does_not_consume_active_capacity() {
        let root = id(1);
        let active_child = id(2);
        let idle_child = id(3);
        let mut guard = SessionLineageGuard::from_live(
            [session(root, None), session(active_child, Some(root))],
            limits(4, 3, 1),
        )
        .expect("valid graph");

        assert!(matches!(
            guard.reserve_new(root, None, true),
            Err(LineageError::ActiveDescendantsExceeded { limit: 1 })
        ));
        let reservation = guard.reserve_new(root, None, false).expect("idle capacity");
        guard
            .commit_new(reservation, idle_child)
            .expect("idle child");
        assert_eq!(
            guard.descendant_counts(root).expect("counts"),
            DescendantCounts {
                total: 2,
                active: 1,
                reserved: 0,
            }
        );
        assert!(matches!(
            guard.begin_execution(idle_child),
            Err(LineageError::ActiveDescendantsExceeded { limit: 1 })
        ));
        guard
            .set_execution_active(active_child, false)
            .expect("release active child");
        assert!(guard.begin_execution(idle_child).expect("start idle child"));
    }

    #[test]
    fn restored_active_descendants_must_fit_limit() {
        let root = id(1);
        let first = id(2);
        let second = id(3);

        assert!(matches!(
            SessionLineageGuard::from_live(
                [
                    session(root, None),
                    session(first, Some(root)),
                    session(second, Some(root)),
                ],
                limits(4, 4, 1),
            ),
            Err(LineageError::ActiveDescendantsExceeded { limit: 1 })
        ));
    }

    #[test]
    fn reservation_release_is_exact_and_removal_releases_only_active_capacity() {
        let root = id(1);
        let child = id(2);
        let mut guard = SessionLineageGuard::from_live([session(root, None)], limits(4, 1, 1))
            .expect("valid root");
        let reservation = guard.reserve_new(root, None, true).expect("reserve");
        assert_eq!(
            guard.descendant_counts(root).expect("counts"),
            DescendantCounts {
                total: 0,
                active: 0,
                reserved: 1,
            }
        );
        guard.release(reservation).expect("release");
        assert_eq!(
            guard.descendant_counts(root).expect("counts"),
            DescendantCounts {
                total: 0,
                active: 0,
                reserved: 0,
            }
        );

        let reservation = guard.reserve_new(root, None, true).expect("reserve again");
        guard.commit_new(reservation, child).expect("commit");
        guard.remove_runtime(child).expect("remove");
        assert_eq!(
            guard.descendant_counts(root).expect("counts"),
            DescendantCounts {
                total: 1,
                active: 0,
                reserved: 0,
            }
        );
        assert!(matches!(
            guard.reserve_new(root, None, true),
            Err(LineageError::TotalDescendantsExceeded { limit: 1 })
        ));
    }

    #[test]
    fn failed_commit_consumes_its_reservation() {
        let root = id(1);
        let mut guard = SessionLineageGuard::from_live([session(root, None)], limits(4, 1, 1))
            .expect("valid root");
        let reservation = guard.reserve_new(root, None, true).expect("reserve");
        assert!(matches!(
            guard.commit_new(reservation, root),
            Err(LineageError::DuplicateSession(_))
        ));
        assert_eq!(guard.descendant_counts(root).expect("counts").reserved, 0);
    }

    #[test]
    fn descendants_of_omits_tombstoned_sessions() {
        let root = id(1);
        let child = id(2);
        let grandchild = id(3);
        let sibling = id(4);
        let grandchild_session = LiveSession {
            id: grandchild,
            root_session_id: root,
            parent_id: Some(child),
            runtime_present: true,
            execution_active: true,
        };
        let mut guard = SessionLineageGuard::from_live(
            [
                session(root, None),
                session(child, Some(root)),
                grandchild_session,
                session(sibling, Some(root)),
            ],
            limits(4, 4, 4),
        )
        .expect("valid graph");

        guard.remove_sessions(&[child, grandchild]);
        assert_eq!(
            guard.descendants_of(root).expect("descendants"),
            vec![sibling]
        );
        let delete_descendants = guard
            .descendants_for_delete(root)
            .expect("delete descendants");
        assert_eq!(
            delete_descendants.iter().copied().collect::<HashSet<_>>(),
            HashSet::from([child, grandchild, sibling])
        );
        let grandchild_index = delete_descendants
            .iter()
            .position(|id| *id == grandchild)
            .expect("grandchild position");
        let child_index = delete_descendants
            .iter()
            .position(|id| *id == child)
            .expect("child position");
        assert!(grandchild_index < child_index);
    }
}
