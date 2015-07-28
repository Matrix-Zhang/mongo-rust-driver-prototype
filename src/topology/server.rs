use Error;
use Result;

use bson::oid;
use connstring::Host;
use pool::{ConnectionPool, PooledStream};

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::thread;

use super::monitor::{IsMasterResult, Monitor};
use super::TopologyDescription;

/// Describes the server role within a server set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerType {
    Standalone,
    Mongos,
    RSPrimary,
    RSSecondary,
    RSArbiter,
    RSOther,
    RSGhost,
    Unknown,
}

/// Server information gathered from server monitoring.
#[derive(Clone, Debug)]
pub struct ServerDescription {
    pub stype: ServerType,
    pub err: Arc<Option<Error>>,
    pub round_trip_time: Option<i64>,
    pub min_wire_version: i64,
    pub max_wire_version: i64,
    pub me: Option<Host>,
    pub hosts: Vec<Host>,
    pub passives: Vec<Host>,
    pub arbiters: Vec<Host>,
    pub tags: BTreeMap<String, String>,
    pub set_name: String,
    pub election_id: Option<oid::ObjectId>,
    pub primary: Option<Host>,
}

/// Holds status and connection information about a single server.
#[derive(Clone)]
pub struct Server {
    pub host: Host,
    pool: Arc<ConnectionPool>,
    description: Arc<RwLock<ServerDescription>>,
    monitor_running: Arc<AtomicBool>,
}

impl ServerDescription {
    /// Returns a default, unknown server description.
    fn new() -> ServerDescription {
        ServerDescription {
            stype: ServerType::Unknown,
            err: Arc::new(None),
            round_trip_time: None,
            min_wire_version: 0,
            max_wire_version: 0,
            me: None,
            hosts: Vec::new(),
            passives: Vec::new(),
            arbiters: Vec::new(),
            tags: BTreeMap::new(),
            set_name: String::new(),
            election_id: None,
            primary: None,
        }
    }

    // Updates the server description using an isMaster server response.
    pub fn update(&mut self, ismaster: IsMasterResult) {
        self.min_wire_version = ismaster.min_wire_version;
        self.max_wire_version = ismaster.max_wire_version;
        self.me = ismaster.me;
        self.hosts = ismaster.hosts;
        self.passives = ismaster.passives;
        self.arbiters = ismaster.arbiters;
        self.tags = ismaster.tags;
        self.set_name = ismaster.set_name;
        self.election_id = ismaster.election_id;
        self.primary = ismaster.primary;

        let hosts_empty = self.hosts.is_empty();
        let set_name_empty = self.set_name.is_empty();
        let msg_empty = ismaster.msg.is_empty();

        self.stype = if msg_empty && set_name_empty && hosts_empty {
            ServerType::Standalone
        } else if !msg_empty {
            ServerType::Mongos
        } else if ismaster.is_master && !set_name_empty {
            ServerType::RSPrimary
        } else if ismaster.is_secondary && !set_name_empty {
            ServerType::RSSecondary
        } else if ismaster.arbiter_only && !set_name_empty {
            ServerType::RSArbiter
        } else if !set_name_empty {
            ServerType::RSOther
        } else if ismaster.is_replica_set {
            ServerType::RSGhost
        } else {
            ServerType::Unknown
        }
    }

    // Sets an encountered error and reverts the server type to Unknown.
    pub fn set_err(&mut self, err: Error) {
        self.err = Arc::new(Some(err));
        self.stype = ServerType::Unknown;
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.monitor_running.store(false, Ordering::SeqCst);
    }
}

impl Server {
    /// Returns a new server with the given host, initializing a new connection pool and monitor.
    pub fn new(req_id: Arc<AtomicIsize>, host: Host,
               top_description: Arc<RwLock<TopologyDescription>>) -> Server {

        let description = Arc::new(RwLock::new(ServerDescription::new()));

        // Create new monitor thread
        let host_clone = host.clone();
        let desc_clone = description.clone();

        let pool = Arc::new(ConnectionPool::new(host.clone()));

        // Fails silently
        let monitor = Monitor::new(host_clone, pool.clone(), top_description, desc_clone, req_id);

        let monitor_running = if monitor.is_ok() {
            monitor.as_ref().unwrap().running.clone()
        } else {
            Arc::new(AtomicBool::new(false))
        };

        if monitor.is_ok() {
            thread::spawn(move || {
                monitor.unwrap().run();
            });
        }

        Server {
            host: host,
            pool: pool,
            description: description.clone(),
            monitor_running: monitor_running,
        }
    }

    /// Returns a server stream from the connection pool.
    pub fn acquire_stream(&self) -> Result<PooledStream> {
        self.pool.acquire_stream()
    }
}
