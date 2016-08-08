use std::collections::HashMap;
use std::io::{self, Cursor};
use std::rc::Rc;
use byteorder::{BigEndian, ReadBytesExt};
use num::ToPrimitive;
use gj::{Promise};
use gjio::{Network};

use identity;
use ll;
use service::{self, ServiceWriter, ServiceReader, ReadMessageError, MessageTrait, MessageHeader};
use EcdsaPublicKey;
use EcdsaPrivateKey;
use Cfg;
pub use self::record::*;

mod record;

/// A handle to a locally-running instance of the GNS daemon.
pub struct GNS {
    service_reader: ServiceReader,
    service_writer: ServiceWriter,
    lookup_id: u32,
}

/// Options for GNS lookups.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LocalOptions {
    /// Default behaviour. Look in the local cache, then in the DHT.
    Default     = 0,
    /// Do not look in the DHT, keep the request to the local cache.
    NoDHT       = 1,
    /// For domains controlled by our master zone only look in the cache. Otherwise look in the
    /// cache, then in the DHT.
    LocalMaster = 2,
}

/// Possible errors returned by the GNS lookup functions.
error_def! LookupError {
    InvalidType { tpe: u16 }
        => "The received message type is invalid" ("The received message type \"{}\" is invalid.", tpe),
    NameTooLong { name: String }
        => "The domain name was too long" ("The domain name \"{}\" is too long to lookup.", name),
    Io { #[from] cause: io::Error }
        => "There was an I/O error communicating with the service" ("Specifically {}", cause),
    ReadMessage { #[from] cause: ReadMessageError }
        => "Failed to receive the response from the GNS service" ("Reason: {}", cause),
}

impl GNS {
    /// Connect to the GNS service.
    ///
    /// Returns either a promise to the GNS service or a `service::ConnectError`. `cfg` contains the
    /// configuration to use to connect to the service.
    pub fn connect(cfg: &Cfg, network: &Network) -> Promise<GNS, service::ConnectError> {
        service::connect(cfg, "gns", network)
            .map(|(sr, sw)| {
                Ok(GNS {
                    service_reader: sr,
                    service_writer: sw,
                    lookup_id : 0,
                })
            })
    }

    /// Lookup a vector of GNS records.
    ///
    /// If `shorten` is not `None` then the result is added to the given shorten zone.
    ///
    /// # Example
    ///
    /// ```rust
    /// use gnunet::{Cfg, identity, gns, GNS};
    /// use gnunet::util::async;
    ///
    /// let config = Cfg::default().unwrap();
    /// let mut event_port = async::EventPort::new().unwrap();
    /// let network = event_port.get_network();
    /// let gns_master = ::std::rc::Rc::new("gns-master".to_string());
    ///
    /// async::EventLoop::top_level(|wait_scope| -> Result<(), ::std::io::Error> {
    ///     let ego = identity::get_default_ego(&config, gns_master, &network).wait(wait_scope, &mut event_port).unwrap();
    ///     let pk = ego.get_public_key();
    ///     let mut gns = GNS::connect(&config, &network).wait(wait_scope, &mut event_port).unwrap();
    ///     let query = gns::LookupQuery { name: "gnu.org",
    ///                                    zone: pk,
    ///                                    record_type: gns::RecordType::A,
    ///                                    options: gns::LocalOptions::LocalMaster,
    ///                                    shorten: None };
    ///     let result = gns.lookup(vec![query]).wait(wait_scope, &mut event_port).unwrap();
    ///     println!("{:?}", result);
    ///     Ok(())
    /// }).expect("top_level");
    /// ```
    pub fn lookup(&mut self, query: Vec<LookupQuery>) -> Promise<Vec<Vec<Record>>, LookupError> {
        let mut sr = self.service_reader.clone();
        let start_id = self.lookup_id;

        let write_promises = query.into_iter().map(|x| {
            let name_len = x.name.len();
            if name_len > ll::GNUNET_DNSPARSER_MAX_NAME_LENGTH as usize {
                return Promise::err(LookupError::NameTooLong { name: x.name.to_string() });
            };

            let id = self.lookup_id;
            self.lookup_id += 1;

            let msg = LookupMessage::new(id, x.zone, x.options, x.shorten, x.record_type, x.name);

            self.service_writer.send_with_str(msg, x.name).lift()
        });

        Promise::all(write_promises).then(move |_| {
            let hm = HashMap::new();
            GNS::lookup_loop(&mut sr, hm).map(move |hm| {
                let mut counter = start_id;
                Ok(hm.into_iter().map(|(id, v)| {
                    assert_eq!(id, counter);
                    counter += 1;
                    v
                }).collect())
            })
        })
    }

    fn lookup_loop(sr: &mut ServiceReader, hashmap: HashMap<u32, Vec<Record>>) -> Promise<HashMap<u32, Vec<Record>>, LookupError> {
        let mut sr2 = sr.clone();
        sr.read_message()
            .lift()
            .then(move |(tpe, mr)| {
                match GNS::parse_lookup_result(tpe, mr, hashmap) {
                    Ok(v) => {
                        // read again if the result is empty
                        if v.is_empty() {
                            return GNS::lookup_loop(&mut sr2, v)
                        }
                        return Promise::ok(v)
                    },
                    Err(e) => return Promise::err(e),
                }
            })
    }

    fn parse_lookup_result(tpe: u16, mut reader: Cursor<Vec<u8>>, mut hashmap: HashMap<u32, Vec<Record>>)
                           -> Result<HashMap<u32, Vec<Record>>, LookupError> {
        match tpe {
            ll::GNUNET_MESSAGE_TYPE_GNS_LOOKUP_RESULT => {
                let mut records = Vec::new();

                let id = try!(reader.read_u32::<BigEndian>());
                let rd_count = try!(reader.read_u32::<BigEndian>());
                for _ in 0..rd_count {
                    let rec = try!(Record::deserialize(&mut reader));
                    records.push(rec);
                };

                if !records.is_empty() {
                    hashmap.insert(id, records);
                }
            },
            x => return Err(LookupError::InvalidType { tpe: x }),
        };
        Ok(hashmap)
    }

}

pub struct LookupQuery<'a> {
    pub name: &'a str,
    pub zone: EcdsaPublicKey,
    pub record_type: RecordType,
    pub options: LocalOptions,
    pub shorten: Option<EcdsaPrivateKey>,
}

#[repr(C, packed)]
struct LookupMessage {
    header: MessageHeader,
    id: u32,
    zone: EcdsaPublicKey,
    options: i16, // LocalOptions
    have_key: i16, // 0 or 1
    record_type: i32, // RecordType
    shorten_key: EcdsaPrivateKey,
    // followed by 0-terminated name to look up
}

impl LookupMessage {
    fn new(id: u32,
           zone: EcdsaPublicKey,
           options: LocalOptions,
           shorten: Option<EcdsaPrivateKey>,
           record_type: RecordType,
           name: &str) -> LookupMessage {
        use std::mem;
        let name_len = name.len();
        let msg_len = (mem::size_of::<LookupMessage>() + name_len + 1).to_u16().unwrap(); // TODO better error handling
        LookupMessage {
            header: MessageHeader {
                len: msg_len.to_be(),
                tpe: ll::GNUNET_MESSAGE_TYPE_GNS_LOOKUP.to_be(),
            },
            id: id.to_be(),
            zone: zone,
            options: (options as i16).to_be(),
            have_key: (shorten.is_some() as i16).to_be(),
            record_type: (record_type as i32).to_be(),
            shorten_key: match shorten {
                Some(x) => x,
                None    => EcdsaPrivateKey::zeros(),
            }
        }
    }
}

impl MessageTrait for LookupMessage {
    // Note that this does not include the 0-terminated string.
    fn into_slice(&self) -> &[u8] {
        message_to_slice!(LookupMessage, self)
    }
}

/// Errors returned by `gns::lookup`.
error_def! ConnectLookupError {
    Connect { #[from] cause: service::ConnectError }
        => "Failed to connect to the GNS service" ("Reason: {}", cause),
    Lookup { #[from] cause: LookupError }
        => "Failed to perform the lookup." ("Reason: {}", cause),
    Io { #[from] cause: io::Error }
        => "There was an I/O error communicating with the service" ("Specifically {}", cause),
}

/// Lookup a GNS record in the given zone.
///
/// If `shorten` is not `None` then the result is added to the given shorten zone.
/// The returned promise can only be fulfilled when a record is found.
/// The user should consider setting a timeout in case no record can be found.
///
/// # Example
///
/// ```rust
/// use gnunet::{Cfg, identity, gns};
/// use gnunet::util::async;
///
/// let config = Cfg::default().unwrap();
/// let mut event_port = async::EventPort::new().unwrap();
/// let network = event_port.get_network();
/// let gns_master = ::std::rc::Rc::new("gns-master".to_string());
/// let gnu_org = ::std::rc::Rc::new("gnu.org".to_string());
///
/// async::EventLoop::top_level(|wait_scope| -> Result<(), ::std::io::Error> {
///     let ego = identity::get_default_ego(&config, gns_master, &network).wait(wait_scope, &mut event_port).unwrap();
///     let pk = ego.get_public_key();
///     let record = gns::lookup(&config,
///                              &network,
///                              gnu_org,
///                              pk,
///                              gns::RecordType::A,
///                              gns::LocalOptions::LocalMaster,
///                              None).wait(wait_scope, &mut event_port).unwrap();
///     println!("Got the IPv4 record for gnu.org: {}", record);
///     Ok(())
/// }).expect("top_level");
/// ```
///
/// # Note
///
/// This is a convenience function that connects to the GNS service, performs the lookup, retrieves
/// one result, then disconects. If you are performing multiple lookups this function should be
/// avoided and `GNS::lookup_in_zone` used instead.
pub fn lookup(cfg: &Cfg,
              network: &Network,
              name: Rc<String>,
              zone: EcdsaPublicKey,
              record_type: RecordType,
              options: LocalOptions,
              shorten: Option<EcdsaPrivateKey>) -> Promise<Record, ConnectLookupError> {
    GNS::connect(cfg, network)
        .lift()
        .then(move |mut gns| {
            println!("connected to GNS");
            let query = LookupQuery { name: &name,
                                      zone: zone,
                                      record_type: record_type,
                                      options: options,
                                      shorten: shorten };
            gns.lookup(vec![query]).lift()
                .map(move |mut result| {
                    // it's ok to unwrap here because gns.lookup does not stop if it hasn't found a result
                    Ok(result.pop().unwrap().pop().unwrap())
                })
        })
}

/// Errors returned by `gns::lookup_in_master`.
error_def! ConnectLookupInMasterError {
    GnsLookup { #[from] cause: ConnectLookupError }
        => "Failed to connect to the GNS service and perform the lookup" ("Reason: {}", cause),
    IdentityGetDefaultEgo { #[from] cause: identity::ConnectGetDefaultEgoError }
        => "Failed to retrieve the default identity for gns-master from the identity service" ("Reason: {}", cause),
    Io { #[from] cause: io::Error }
        => "There was an I/O error communicating with the service" ("Specifically {}", cause),
}

/// Lookup a GNS record in the master zone.
///
/// If `shorten` is not `None` then the result is added to the given shorten zone.
/// The returned promise can only be fulfilled when a record is found.
/// The user should consider setting a timeout in case no record can be found.
///
/// # Example
///
/// ```rust
/// use gnunet::{Cfg, gns};
/// use gnunet::util::async;
///
/// let config = Cfg::default().unwrap();
/// let mut event_port = async::EventPort::new().unwrap();
/// let network = event_port.get_network();
/// let gnu_org = ::std::rc::Rc::new("gnu.org".to_string());
///
/// async::EventLoop::top_level(|wait_scope| -> Result<(), ::std::io::Error> {
///     let record_promise = gnunet::lookup_in_master(&config, &network, gnu_org, gns::RecordType::A, None);
///     let record = record_promise.wait(wait_scope, &mut event_port).unwrap();
///     println!("Got the IPv4 record for gnu.org: {}", record);
///     Ok(())
/// }).expect("top_level");
/// ```
///
/// # Note
///
/// This is a convenience function that connects to the identity service, fetches the default ego
/// for gns-master, then connects to the GNS service, performs the lookup, retrieves one result,
/// then disconnects from everything. If you are performing lots of lookups this function should be
/// avoided and `GNS::lookup` should be used instead.
pub fn lookup_in_master(cfg: &Cfg,
                        network: &Network,
                        name: Rc<String>,
                        record_type: RecordType,
                        shorten: Option<EcdsaPrivateKey>) -> Promise<Record, ConnectLookupInMasterError> {
    println!("Getting default ego");
    let network2 = network.clone();
    let cfg2 = cfg.clone();
    identity::get_default_ego(cfg, Rc::new("gns-master".to_string()), network)
        .lift()
        .then(move |ego| {
            let pk = ego.get_public_key();
            let opt: LocalOptions;
            {
                let mut it = name.split('.');
                opt = match (it.next(), it.next(), it.next()) {
                    (Some(_), Some("gnu"), None)  => LocalOptions::NoDHT,
                    _                             => LocalOptions::LocalMaster,
                };
            }
            println!("doing lookup");
            lookup(&cfg2, &network2, name, pk, record_type, opt, shorten).lift()
        })
}
