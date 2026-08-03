use std::collections::{HashMap, HashSet};

use n00n_storage::id::n00nId;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LineageLimits {
    pub(crate) max_depth: usize,
    pub(crate) max_total_descendants: usize,
    pub(crate) max_active_descendants: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LiveSession {
    pub(crate) id: n00nId,
    pub(crate) parent_id: Option<n00nId>,
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
    parent_id: Option<n00nId>,
    active: bool,
}

#[derive(Debug, Clone, Copy)]
struct PendingReservation {
    caller: n00nId,
    parent: n00nId,
    root: n00nId,
    depth: usize,
}

pub(crate) struct SessionLineageGuard {
    limits: LineageLimits,
    sessions: HashMap<n00nId, SessionNode>,
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
            reservations: HashMap::new(),
            next_reservation_id: 1,
        };
        for session in sessions {
            if guard
                .sessions
                .insert(
                    session.id,
                    SessionNode {
                        parent_id: session.parent_id,
                        active: true,
                    },
                )
                .is_some()
            {
                return Err(LineageError::DuplicateSession(session.id));
            }
        }
        guard.validate_graph()?;
        Ok(guard)
    }

    pub(crate) fn activate_runtime(&mut self, session: LiveSession) -> Result<(), LineageError> {
        if let Some(existing) = self.sessions.get(&session.id) {
            if existing.active {
                return Err(LineageError::DuplicateSession(session.id));
            }
            if existing.parent_id != session.parent_id {
                return Err(LineageError::ParentChanged { id: session.id });
            }
            self.sessions
                .get_mut(&session.id)
                .ok_or(LineageError::UnknownSession(session.id))?
                .active = true;
            if let Err(error) = self.validate_graph() {
                if let Some(node) = self.sessions.get_mut(&session.id) {
                    node.active = false;
                }
                return Err(error);
            }
            return Ok(());
        }

        self.sessions.insert(
            session.id,
            SessionNode {
                parent_id: session.parent_id,
                active: true,
            },
        );
        if let Err(error) = self.validate_graph() {
            self.sessions.remove(&session.id);
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn remove_runtime(&mut self, id: n00nId) -> Result<(), LineageError> {
        let node = self
            .sessions
            .get_mut(&id)
            .ok_or(LineageError::UnknownSession(id))?;
        node.active = false;
        Ok(())
    }

    pub(crate) fn lineage(&self, caller: n00nId) -> Result<SessionLineage, LineageError> {
        let node = self
            .sessions
            .get(&caller)
            .ok_or(LineageError::CallerNotLive(caller))?;
        if !node.active {
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
    ) -> Result<NewReservation, LineageError> {
        self.validate_graph()?;
        let caller_lineage = self.lineage(caller)?;
        let parent = explicit_parent.unwrap_or(caller);
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
        if limit_reached(
            counts.active,
            counts.reserved,
            self.limits.max_active_descendants,
        ) {
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
                parent_id: Some(pending.parent),
                active: true,
            },
        );
        if let Err(error) = self.validate_graph() {
            self.sessions.remove(&child_id);
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

    pub(crate) fn authorize_prompt(
        &self,
        caller: n00nId,
        explicit_target: Option<n00nId>,
    ) -> Result<n00nId, LineageError> {
        self.validate_graph()?;
        let caller_lineage = self.lineage(caller)?;
        let target = explicit_target.unwrap_or(caller);
        let target_node = self
            .sessions
            .get(&target)
            .ok_or(LineageError::UnknownSession(target))?;
        if !target_node.active {
            return Err(LineageError::TargetNotLive(target));
        }
        let caller_path = self.path_from(caller)?;
        let target_path = self.path_from(target)?;
        if caller_lineage.caller == target
            || caller_path.contains(&target)
            || target_path.contains(&caller)
        {
            return Ok(target);
        }
        Err(LineageError::UnauthorizedTarget)
    }

    pub(crate) fn descendant_counts(&self, root: n00nId) -> Result<DescendantCounts, LineageError> {
        let mut total = 0;
        let mut active = 0;
        for (&id, node) in &self.sessions {
            let lineage = self.lineage_for(id)?;
            if lineage.root != root || id == root {
                continue;
            }
            total += 1;
            if node.active {
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

    fn validate_graph(&self) -> Result<(), LineageError> {
        for &id in self.sessions.keys() {
            self.path_from(id)?;
        }
        Ok(())
    }

    fn lineage_for(&self, id: n00nId) -> Result<SessionLineage, LineageError> {
        let path = self.path_from(id)?;
        let node = self
            .sessions
            .get(&id)
            .ok_or(LineageError::UnknownSession(id))?;
        let root = path
            .last()
            .copied()
            .ok_or(LineageError::UnknownSession(id))?;
        Ok(SessionLineage {
            caller: id,
            root,
            parent: node.parent_id,
            depth: path.len() - 1,
        })
    }

    fn path_from(&self, start: n00nId) -> Result<Vec<n00nId>, LineageError> {
        if !self.sessions.contains_key(&start) {
            return Err(LineageError::UnknownSession(start));
        }
        let mut path = Vec::new();
        let mut seen = HashSet::new();
        let mut current = start;
        loop {
            if !seen.insert(current) {
                return Err(LineageError::Cycle(current));
            }
            path.push(current);
            let parent = self
                .sessions
                .get(&current)
                .ok_or(LineageError::UnknownSession(current))?
                .parent_id;
            let Some(parent) = parent else {
                return Ok(path);
            };
            if !self.sessions.contains_key(&parent) {
                return Ok(path);
            }
            current = parent;
        }
    }
}

fn limit_reached(committed: usize, reserved: usize, limit: usize) -> bool {
    committed >= limit || reserved >= limit.saturating_sub(committed)
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
        LiveSession { id, parent_id }
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

        let reservation = guard.reserve_new(root_a, None).expect("root A capacity");
        guard.commit_new(reservation, child_a).expect("child A");
        assert!(matches!(
            guard.reserve_new(root_a, None),
            Err(LineageError::TotalDescendantsExceeded { .. })
        ));

        let reservation = guard.reserve_new(root_b, None).expect("root B capacity");
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
            guard.reserve_new(root, Some(foreign)),
            Err(LineageError::ParentMismatch)
        ));
        assert!(matches!(
            guard.reserve_new(id(99), None),
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
        let reservation = guard.reserve_new(child, None).expect("depth one");
        guard
            .commit_new(reservation, grandchild)
            .expect("grandchild");
        assert!(matches!(
            guard.reserve_new(grandchild, None),
            Err(LineageError::DepthExceeded { limit: 2 })
        ));

        let mut active_limited = SessionLineageGuard::from_live(
            [session(root, None), session(child, Some(root))],
            limits(4, 3, 1),
        )
        .expect("valid graph");
        assert!(matches!(
            active_limited.reserve_new(root, None),
            Err(LineageError::ActiveDescendantsExceeded { limit: 1 })
        ));
    }

    #[test]
    fn reservation_release_is_exact_and_removal_releases_only_active_capacity() {
        let root = id(1);
        let child = id(2);
        let mut guard = SessionLineageGuard::from_live([session(root, None)], limits(4, 1, 1))
            .expect("valid root");
        let reservation = guard.reserve_new(root, None).expect("reserve");
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

        let reservation = guard.reserve_new(root, None).expect("reserve again");
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
            guard.reserve_new(root, None),
            Err(LineageError::TotalDescendantsExceeded { limit: 1 })
        ));
    }

    #[test]
    fn failed_commit_consumes_its_reservation() {
        let root = id(1);
        let mut guard = SessionLineageGuard::from_live([session(root, None)], limits(4, 1, 1))
            .expect("valid root");
        let reservation = guard.reserve_new(root, None).expect("reserve");
        assert!(matches!(
            guard.commit_new(reservation, root),
            Err(LineageError::DuplicateSession(_))
        ));
        assert_eq!(guard.descendant_counts(root).expect("counts").reserved, 0);
    }
}
