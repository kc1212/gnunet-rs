use std::io::net::pipe::UnixStream;
use std::io::util::LimitReader;
use std::collections::HashMap;
use std::kinds::marker::InvariantLifetime;
use sync::comm::{Empty, Disconnected};

use identity;
use ll;
use service;
use service::{Service, ProcessMessageResult};
use EcdsaPublicKey;
use EcdsaPrivateKey;
use Configuration;
use self::error::*;
pub use self::record::*;

mod error;
mod record;

/// A handle to a locally-running instance of the GNS daemon.
pub struct GNS {
  service: Service,
  lookup_id: u32,
  lookup_tx: Sender<(u32, Sender<Record>)>,
}

/// Options for GNS lookups.
pub enum LocalOptions {
  /// Default behaviour. Look in the local cache, then in the DHT.
  Default     = 0,
  /// Do not look in the DHT, keep the request to the local cache.
  NoDHT       = 1,
  /// For domains controlled by our master zone only look in the cache. Otherwise look in the
  /// cache, then in the DHT.
  LocalMaster = 2,
}

impl GNS {
  /// Connect to the GNS service.
  ///
  /// Returns either a handle to the GNS service or a `service::ConnectError`. `cfg` contains the
  /// configuration to use to connect to the service. Can be `None` to use the system default
  /// configuration - this should work on most properly-configured systems.
  pub fn connect(cfg: Option<&Configuration>) -> Result<GNS, service::ConnectError> {
    let (lookup_tx, lookup_rx) = channel::<(u32, Sender<Record>)>();
    let mut handles: HashMap<u32, Sender<Record>> = HashMap::new();

    let mut service = ttry!(Service::connect(cfg, "gns"));
    service.init_callback_loop(move |&mut: tpe: u16, mut reader: LimitReader<UnixStream>| -> ProcessMessageResult {
      loop {
        match lookup_rx.try_recv() {
          Ok((id, sender)) => {
            handles.insert(id, sender);
          },
          Err(e)  => match e {
            Empty         => break,
            Disconnected  => return ProcessMessageResult::Shutdown,
          },
        }
      }
      match tpe {
        ll::GNUNET_MESSAGE_TYPE_GNS_LOOKUP_RESULT => {
          let id = match reader.read_be_u32() {
            Ok(id)  => id,
            Err(_)  => return ProcessMessageResult::Reconnect,
          };
          match handles.get(&id) {
            Some(sender) => {
              let rd_count = match reader.read_be_u32() {
                Ok(x)   => x,
                Err(_)  => return ProcessMessageResult::Reconnect,
              };
              for _ in range(0, rd_count) {
                let rec = match Record::deserialize(&mut reader) {
                  Ok(r)   => r,
                  Err(_)  => return ProcessMessageResult::Reconnect,
                };
                sender.send(rec);
              };
            },
            _ => (),
          };
        },
        _ => return ProcessMessageResult::Reconnect,
      };
      match reader.limit() {
        0 => ProcessMessageResult::Continue,
        _ => ProcessMessageResult::Reconnect,
      }
    });
    Ok(GNS {
      service: service,
      lookup_id: 0,
      lookup_tx: lookup_tx,
    })
  }

  /// Lookup a GNS record in the given zone.
  ///
  /// If `shorten` is not `None` then the result is added to the given shorten zone. Returns
  /// immediately with a handle that can be queried for results.
  ///
  /// # Example
  ///
  /// ```rust
  /// use gnunet::{IdentityService, GNS, gns};
  ///
  /// let mut ids = IdentityService::connect(None).unwrap();
  /// let gns_ego = ids.get_default_ego("gns-master").unwrap();
  /// let mut gns = GNS::connect(None).unwrap();
  /// let mut lh = gns.lookup("www.gnu",
  ///                         &gns_ego.get_public_key(),
  ///                         gns::A,
  ///                         gns::LOLocalMaster,
  ///                         None).unwrap();
  /// let record = lh.recv();
  /// println!("Got the IPv4 record for www.gnu: {}", record);
  /// ```
  pub fn lookup<'a>(
      &'a mut self,
      name: &str,
      zone: &EcdsaPublicKey,
      record_type: RecordType,
      options: LocalOptions,
      shorten: Option<&EcdsaPrivateKey>
    ) -> Result<LookupHandle<'a>, LookupError> {

    let name_len = name.len();
    if name_len > ll::GNUNET_DNSPARSER_MAX_NAME_LENGTH as uint {
      return Err(LookupError::NameTooLong);
    };

    let id = self.lookup_id;
    self.lookup_id += 1;

    let msg_length = (80 + name_len + 1).to_u16().unwrap();
    let mut mw = self.service.write_message(msg_length, ll::GNUNET_MESSAGE_TYPE_GNS_LOOKUP);
    ttry!(mw.write_be_u32(id));
    ttry!(zone.serialize(&mut mw));
    ttry!(mw.write_be_i16(options as i16));
    ttry!(mw.write_be_i16(shorten.is_some() as i16));
    ttry!(mw.write_be_i32(record_type as i32));
    match shorten {
      Some(z) => ttry!(z.serialize(&mut mw)),
      None    => ttry!(mw.write(&[0u8, ..32])),
    };
    ttry!(mw.write(name.as_bytes()));
    ttry!(mw.write_u8(0u8));

    let (tx, rx) = channel::<Record>();
    self.lookup_tx.send((id, tx));
    ttry!(mw.send());
    Ok(LookupHandle {
      marker: InvariantLifetime,
      receiver: rx,
    })
  }
}

/// Lookup a GNS record in the given zone.
///
/// If `shorten` is not `None` then the result is added to the given shorten zone. This function
/// will block until it returns the first matching record that it can find.
///
/// # Example
///
/// ```rust
/// use gnunet::{identity, gns};
///
/// let gns_ego = identity::get_default_ego(None, "gns-master").unwrap();
/// let record = gns::lookup(None,
///                          "www.gnu",
///                          &gns_ego.get_public_key(),
///                          gns::A,
///                          gns::LOLocalMaster,
///                          None).unwrap();
/// println!("Got the IPv4 record for www.gnu: {}", record);
/// ```
///
/// # Note
///
/// This is a convenience function that connects to the GNS service, performs the lookup, retrieves
/// one result, then disconects. If you are performing multiple lookups this function should be
/// avoided and `GNS::lookup_in_zone` used instead.
pub fn lookup(
    cfg: Option<&Configuration>,
    name: &str,
    zone: &EcdsaPublicKey,
    record_type: RecordType,
    options: LocalOptions,
    shorten: Option<&EcdsaPrivateKey>) -> Result<Record, ConnectLookupError> {
  let mut gns = ttry!(GNS::connect(cfg));
  let mut h = ttry!(gns.lookup(name, zone, record_type, options, shorten));
  Ok(h.recv())
}

/// Lookup a GNS record in the master zone.
///
/// If `shorten` is not `None` then the result is added to the given shorten zone. This function
/// will block until it returns the first matching record that it can find.
///
/// # Example
///
/// ```rust
/// use gnunet::gns;
///
/// let record = gns::lookup_in_master(None, "www.gnu", gns::A, None).unwrap();
/// println!("Got the IPv4 record for www.gnu: {}", record);
/// ```
///
/// # Note
///
/// This is a convenience function that connects to the identity service, fetches the default ego
/// for gns-master, then connects to the GNS service, performs the lookup, retrieves one result,
/// then disconnects from everything. If you are performing lots of lookups this function should be
/// avoided and `GNS::lookup_in_zone` used instead.
pub fn lookup_in_master(
    cfg: Option<&Configuration>,
    name: &str,
    record_type: RecordType,
    shorten: Option<&EcdsaPrivateKey>) -> Result<Record, ConnectLookupInMasterError> {
  let ego = ttry!(identity::get_default_ego(cfg, "gns-master"));
  let pk = ego.get_public_key();
  let mut it = name.split('.');
  let opt = match (it.next(), it.next(), it.next()) {
    (Some(_), Some("gnu"), None)  => LocalOptions::NoDHT,
    _                             => LocalOptions::LocalMaster,
  };
  let ret = ttry!(lookup(cfg, name, &pk, record_type, opt, shorten));
  Ok(ret)
}

/// A handle returned by `GNS::lookup`.
///
/// Used to retrieve the results of a lookup.
pub struct LookupHandle<'a> {
  marker: InvariantLifetime<'a>,
  receiver: Receiver<Record>,
}

impl<'a> LookupHandle<'a> {
  /// Receive a single result from a lookup.
  ///
  /// Blocks until a result is available. This function can be called multiple times on a handle to
  /// receive multiple results.
  pub fn recv(&mut self) -> Record {
    self.receiver.recv()
  }
}
