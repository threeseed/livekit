//! Room and participant storage.
//!
//! Ports `pkg/service/localstore.go` and the `ObjectStore` interface it
//! implements. On a single node the store is a map behind a lock; the Redis
//! store that makes the same data visible to a cluster is phase 4 work, and
//! the trait is what keeps that swap from touching anything else.
//!
//! Room locking is part of the trait because room creation is
//! read-modify-write: two participants joining an unknown room at the same
//! moment must not each create it. The local store takes one process-wide lock,
//! as the Go one does.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use lk_proto::livekit::{ParticipantInfo, Room, RoomInternal};
use tokio::sync::{Mutex, OwnedMutexGuard, RwLock};

use crate::error::{Error, Result};

/// A boxed future, so the store can be used behind a trait object.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A held room lock. Dropping it releases the lock.
pub struct RoomLock {
    _guard: OwnedMutexGuard<()>,
}

/// Where rooms and participants live.
pub trait ObjectStore: Send + Sync + 'static {
    /// Takes the room lock for a create-or-join.
    fn lock_room<'a>(
        &'a self,
        room_name: &'a str,
        timeout: Duration,
    ) -> BoxFuture<'a, Result<RoomLock>>;

    /// Stores a room and its server-side settings.
    fn store_room<'a>(&'a self, room: Room, internal: RoomInternal) -> BoxFuture<'a, Result<()>>;

    /// Loads a room, and its server-side settings when asked for.
    fn load_room<'a>(
        &'a self,
        room_name: &'a str,
        include_internal: bool,
    ) -> BoxFuture<'a, Result<(Room, Option<RoomInternal>)>>;

    /// Lists rooms, or the named subset.
    fn list_rooms<'a>(&'a self, names: &'a [String]) -> BoxFuture<'a, Result<Vec<Room>>>;

    /// Deletes a room and everyone in it.
    fn delete_room<'a>(&'a self, room_name: &'a str) -> BoxFuture<'a, Result<()>>;

    /// Stores a participant.
    fn store_participant<'a>(
        &'a self,
        room_name: &'a str,
        participant: ParticipantInfo,
    ) -> BoxFuture<'a, Result<()>>;

    /// Loads a participant.
    fn load_participant<'a>(
        &'a self,
        room_name: &'a str,
        identity: &'a str,
    ) -> BoxFuture<'a, Result<ParticipantInfo>>;

    /// Lists a room's participants.
    fn list_participants<'a>(
        &'a self,
        room_name: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ParticipantInfo>>>;

    /// Deletes a participant.
    fn delete_participant<'a>(
        &'a self,
        room_name: &'a str,
        identity: &'a str,
    ) -> BoxFuture<'a, Result<()>>;
}

/// The single-node store.
#[derive(Debug, Default)]
pub struct LocalStore {
    state: RwLock<State>,
    /// Local rooms lock globally, as in Go: there is one process, and room
    /// creation is rare enough that a finer lock would only add a way to
    /// deadlock.
    global_lock: Arc<Mutex<()>>,
}

#[derive(Debug, Default)]
struct State {
    rooms: BTreeMap<String, Room>,
    internal: BTreeMap<String, RoomInternal>,
    participants: BTreeMap<String, BTreeMap<String, ParticipantInfo>>,
}

impl LocalStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl ObjectStore for LocalStore {
    fn lock_room<'a>(
        &'a self,
        _room_name: &'a str,
        _timeout: Duration,
    ) -> BoxFuture<'a, Result<RoomLock>> {
        let lock = self.global_lock.clone();
        Box::pin(async move {
            Ok(RoomLock {
                _guard: lock.lock_owned().await,
            })
        })
    }

    fn store_room<'a>(
        &'a self,
        mut room: Room,
        internal: RoomInternal,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if room.creation_time == 0 {
                room.creation_time = unix_seconds();
                room.creation_time_ms = unix_millis();
            }
            let name = room.name.clone();
            let mut state = self.state.write().await;
            state.rooms.insert(name.clone(), room);
            state.internal.insert(name, internal);
            Ok(())
        })
    }

    fn load_room<'a>(
        &'a self,
        room_name: &'a str,
        include_internal: bool,
    ) -> BoxFuture<'a, Result<(Room, Option<RoomInternal>)>> {
        Box::pin(async move {
            let state = self.state.read().await;
            let room = state
                .rooms
                .get(room_name)
                .cloned()
                .ok_or(Error::RoomNotFound)?;
            let internal = include_internal
                .then(|| state.internal.get(room_name).cloned())
                .flatten();
            Ok((room, internal))
        })
    }

    fn list_rooms<'a>(&'a self, names: &'a [String]) -> BoxFuture<'a, Result<Vec<Room>>> {
        Box::pin(async move {
            let state = self.state.read().await;
            Ok(state
                .rooms
                .values()
                .filter(|room| names.is_empty() || names.contains(&room.name))
                .cloned()
                .collect())
        })
    }

    fn delete_room<'a>(&'a self, room_name: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let mut state = self.state.write().await;
            state.rooms.remove(room_name);
            state.internal.remove(room_name);
            state.participants.remove(room_name);
            Ok(())
        })
    }

    fn store_participant<'a>(
        &'a self,
        room_name: &'a str,
        participant: ParticipantInfo,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let mut state = self.state.write().await;
            state
                .participants
                .entry(room_name.to_owned())
                .or_default()
                .insert(participant.identity.clone(), participant);
            Ok(())
        })
    }

    fn load_participant<'a>(
        &'a self,
        room_name: &'a str,
        identity: &'a str,
    ) -> BoxFuture<'a, Result<ParticipantInfo>> {
        Box::pin(async move {
            let state = self.state.read().await;
            state
                .participants
                .get(room_name)
                .and_then(|room| room.get(identity))
                .cloned()
                .ok_or(Error::BadRequest("participant does not exist"))
        })
    }

    fn list_participants<'a>(
        &'a self,
        room_name: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ParticipantInfo>>> {
        Box::pin(async move {
            let state = self.state.read().await;
            Ok(state
                .participants
                .get(room_name)
                .map(|room| room.values().cloned().collect())
                .unwrap_or_default())
        })
    }

    fn delete_participant<'a>(
        &'a self,
        room_name: &'a str,
        identity: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let mut state = self.state.write().await;
            if let Some(room) = state.participants.get_mut(room_name) {
                room.remove(identity);
            }
            Ok(())
        })
    }
}

fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn unix_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn room(name: &str) -> Room {
        Room {
            sid: format!("RM_{name}"),
            name: name.to_owned(),
            ..Room::default()
        }
    }

    #[tokio::test]
    async fn rooms_round_trip() {
        let store = LocalStore::new();
        store
            .store_room(room("my-room"), RoomInternal::default())
            .await
            .unwrap();

        let (loaded, internal) = store.load_room("my-room", true).await.unwrap();
        assert_eq!(loaded.name, "my-room");
        assert!(internal.is_some());
        // a creation time is filled in on the way past, as in Go
        assert!(loaded.creation_time > 0);

        let (_, internal) = store.load_room("my-room", false).await.unwrap();
        assert!(internal.is_none());

        assert_eq!(store.list_rooms(&[]).await.unwrap().len(), 1);
        assert!(
            store
                .list_rooms(&["other".to_owned()])
                .await
                .unwrap()
                .is_empty()
        );

        assert!(matches!(
            store.load_room("nope", false).await,
            Err(Error::RoomNotFound)
        ));
    }

    #[tokio::test]
    async fn deleting_a_room_deletes_its_participants() {
        let store = LocalStore::new();
        store
            .store_room(room("my-room"), RoomInternal::default())
            .await
            .unwrap();
        store
            .store_participant(
                "my-room",
                ParticipantInfo {
                    identity: "alice".to_owned(),
                    ..ParticipantInfo::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(store.list_participants("my-room").await.unwrap().len(), 1);

        store.delete_room("my-room").await.unwrap();
        assert!(store.list_participants("my-room").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_room_lock_is_exclusive() {
        let store = Arc::new(LocalStore::new());
        let held = store
            .lock_room("my-room", Duration::from_secs(5))
            .await
            .unwrap();

        let other = store.clone();
        let waiter = tokio::spawn(async move {
            let _lock = other
                .lock_room("my-room", Duration::from_secs(5))
                .await
                .unwrap();
            true
        });

        // the second caller is still waiting while the first holds the lock
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished());

        drop(held);
        assert!(waiter.await.unwrap());
    }
}
