use std::{
    collections::{HashMap, HashSet},
    pin::Pin,
    sync::{Arc, RwLock, Weak},
    time::Duration,
};

use engineio_server::async_trait;
use futures::{stream, Stream, StreamExt};
use itertools::Itertools;
use serde::de::DeserializeOwned;

use crate::{
    errors::{AckError, Error},
    ns::Namespace,
    packet::Packet,
    socket::{AckResponse, Socket},
};

pub type Room = String;

#[derive(Hash, PartialEq, Eq)]
pub enum BroadcastFlags {
    Local,
    Broadcast,
    Timeout(Duration),
}
pub struct BroadcastOptions {
    pub flags: HashSet<BroadcastFlags>,
    pub rooms: Vec<Room>,
    pub except: Vec<Room>,
    pub sid: i64,
}
impl Default for BroadcastOptions {
    fn default() -> Self {
        Self {
            flags: HashSet::new(),
            rooms: Vec::new(),
            except: Vec::new(),
            sid: -1,
        }
    }
}

#[async_trait]
pub trait Adapter: Send + Sync + 'static {
    fn new(ns: Weak<Namespace<Self>>) -> Self
    where
        Self: Sized;
    async fn init(&self);
    async fn close(&self);

    async fn server_count(&self) -> u16;

    async fn add_all(&self, sid: i64, rooms: Vec<String>);
    async fn del(&self, sid: i64, rooms: Vec<String>);
    async fn del_all(&self, sid: i64);

    async fn broadcast(
        &self,
        packet: Packet,
        binary: Option<Vec<Vec<u8>>>,
        opts: BroadcastOptions,
    ) -> Result<(), Error>;

    async fn broadcast_with_ack<V: DeserializeOwned>(
        &self,
        packet: Packet,
        binary: Option<Vec<Vec<u8>>>,
        opts: BroadcastOptions,
    ) -> Pin<Box<dyn Stream<Item = Result<AckResponse<V>, AckError>>>>;

    async fn sockets(&self, rooms: Vec<Room>) -> Vec<i64>;
    async fn socket_rooms(&self, sid: i64) -> Vec<String>;

    async fn fetch_sockets(&self, opts: BroadcastOptions) -> Vec<Arc<Socket<Self>>>
    where
        Self: Sized;
    async fn add_sockets(&self, opts: BroadcastOptions, rooms: Vec<String>);
    async fn del_sockets(&self, opts: BroadcastOptions, rooms: Vec<String>);
    async fn disconnect_socket(&self, opts: BroadcastOptions) -> Result<(), Error>;

    //TODO: implement
    // async fn server_side_emit(&self, packet: Packet, opts: BroadcastOptions) -> Result<u64, Error>;
    // async fn persist_session(&self, sid: i64);
    // async fn restore_session(&self, sid: i64) -> Session;
}

pub struct LocalAdapter {
    rooms: RwLock<HashMap<String, HashSet<i64>>>,
    ns: Weak<Namespace<Self>>,
}

#[async_trait]
impl Adapter for LocalAdapter {
    fn new(ns: Weak<Namespace<Self>>) -> Self {
        Self {
            rooms: HashMap::new().into(),
            ns,
        }
    }

    async fn init(&self) {}

    async fn close(&self) {}

    async fn server_count(&self) -> u16 {
        1
    }

    async fn add_all(&self, sid: i64, rooms: Vec<Room>) {
        let mut rooms_map = self.rooms.write().unwrap();
        for room in rooms {
            rooms_map
                .entry(room)
                .or_insert_with(HashSet::new)
                .insert(sid);
        }
    }

    async fn del(&self, sid: i64, rooms: Vec<Room>) {
        let mut rooms_map = self.rooms.write().unwrap();
        for room in rooms {
            if let Some(room) = rooms_map.get_mut(&room) {
                room.remove(&sid);
            }
        }
    }

    async fn del_all(&self, sid: i64) {
        let mut rooms_map = self.rooms.write().unwrap();
        for room in rooms_map.values_mut() {
            room.remove(&sid);
        }
    }

    async fn broadcast(
        &self,
        packet: Packet,
        binary: Option<Vec<Vec<u8>>>,
        opts: BroadcastOptions,
    ) -> Result<(), Error> {
        let sockets = self.apply_opts(opts);

        tracing::debug!("broadcasting packet to {} sockets", sockets.len());
        sockets
            .into_iter()
            .map(|socket| socket.send(packet.clone(), binary.clone()))
            .collect::<Result<(), Error>>()
    }

    async fn broadcast_with_ack<V: DeserializeOwned>(
        &self,
        packet: Packet,
        binary: Option<Vec<Vec<u8>>>,
        opts: BroadcastOptions,
    ) -> Pin<Box<dyn Stream<Item = Result<AckResponse<V>, AckError>>>> {
        let duration = opts.flags.iter().find_map(|flag| match flag {
            BroadcastFlags::Timeout(duration) => Some(*duration),
            _ => None,
        });
        let sockets = self.apply_opts(opts);
        tracing::debug!(
            "broadcasting packet to {} sockets: {:?}",
            sockets.len(),
            sockets.iter().map(|s| s.sid).collect::<Vec<_>>()
        );
        let count = sockets.len();
        let ack_futs = sockets.into_iter().map(move |socket| {
            let packet = packet.clone();
            let binary = binary.clone();
            async move { socket.clone().send_with_ack(packet, binary, duration).await }
        });
        stream::iter(ack_futs).buffer_unordered(count).boxed()
    }

    async fn sockets(&self, rooms: Vec<Room>) -> Vec<i64> {
        let opts = BroadcastOptions {
            rooms,
            ..Default::default()
        };
        self.apply_opts(opts)
            .into_iter()
            .map(|socket| socket.sid)
            .collect()
    }

    //TODO: make this operation O(1)
    async fn socket_rooms(&self, sid: i64) -> Vec<Room> {
        let rooms_map = self.rooms.read().unwrap();
        rooms_map
            .iter()
            .filter(|(_, sockets)| sockets.contains(&sid))
            .map(|(room, _)| room.clone())
            .collect()
    }

    async fn fetch_sockets(&self, opts: BroadcastOptions) -> Vec<Arc<Socket<Self>>> {
        self.apply_opts(opts)
    }

    async fn add_sockets(&self, opts: BroadcastOptions, rooms: Vec<Room>) {
        let futs = self
            .apply_opts(opts)
            .into_iter()
            .map(|socket| self.add_all(socket.sid, rooms.clone()));
        futures::future::join_all(futs).await;
    }

    async fn del_sockets(&self, opts: BroadcastOptions, rooms: Vec<Room>) {
        let futs = self
            .apply_opts(opts)
            .into_iter()
            .map(|socket| self.del(socket.sid, rooms.clone()));
        futures::future::join_all(futs).await;
    }

    async fn disconnect_socket(&self, opts: BroadcastOptions) -> Result<(), Error> {
        self.apply_opts(opts)
            .into_iter()
            .map(|socket| socket.disconnect())
            .collect::<Result<(), Error>>()
    }
}

impl LocalAdapter {
    /// Apply the given `opts` and return the sockets that match.
    fn apply_opts(&self, opts: BroadcastOptions) -> Vec<Arc<Socket<Self>>> {
        let rooms = opts.rooms;

        let except = self.get_except_sids(&opts.except);
        let ns = self.ns.upgrade().unwrap();
        if rooms.len() > 0 {
            let rooms_map = self.rooms.read().unwrap();
            rooms_map
                .iter()
                .filter(|(room, _)| rooms.contains(room))
                .flat_map(|(_, sockets)| sockets)
                .filter(|sid| {
                    !except.contains(*sid)
                        && (opts.flags.contains(&BroadcastFlags::Broadcast) && **sid != opts.sid)
                })
                .unique()
                .map(|sid| ns.get_socket(*sid))
                .filter(Option::is_some)
                .map(Option::unwrap)
                .collect()
        } else if opts.flags.contains(&BroadcastFlags::Broadcast) {
            let sockets = ns.get_sockets();
            sockets
                .into_iter()
                .filter(|socket| !except.contains(&socket.sid))
                .collect()
        } else if let Some(sock) = ns.get_socket(opts.sid) {
            vec![sock]
        } else {
            vec![]
        }
    }

    fn get_except_sids(&self, except: &Vec<Room>) -> HashSet<i64> {
        let mut except_sids = HashSet::new();
        let rooms_map = self.rooms.read().unwrap();
        for room in except {
            if let Some(sockets) = rooms_map.get(room) {
                except_sids.extend(sockets);
            }
        }
        except_sids
    }
}